//! Chrome DevTools: HTTP-конечные точки (`/json/*`) и команды CDP по WebSocket.
//!
//! Документация: <https://chromedevtools.github.io/devtools-protocol/>. Для своего
//! стенда CDP допустим (под ним рвёт поток только живой `YouTube`, решение сессии 05).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use super::BrowserError;

const PROTOCOL: &str = "DevTools";
/// Потолок на один HTTP-запрос и на чтение одного сообщения CDP.
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// Выполняет HTTP-запрос к DevTools (`127.0.0.1:port`) и возвращает тело ответа.
///
/// `path_and_query` вставляется как есть: `/json/new?<url>` берёт ВСЁ после `?` адресом.
pub(crate) fn http_request(
    port: u16,
    method: &str,
    path_and_query: &str,
) -> Result<String, BrowserError> {
    let address = format!("127.0.0.1:{port}");
    let mut stream = TcpStream::connect(&address)
        .map_err(|source| BrowserError::io(format!("connect to DevTools {address}"), source))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|source| BrowserError::io("set DevTools socket timeouts", source))?;
    let request = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|source| BrowserError::io("send DevTools request", source))?;
    // Chromium не всегда закрывает соединение после ответа (`PUT /json/new` держит его),
    // поэтому читаем заголовки, а затем ровно `Content-Length` байт тела.
    let (head, body) = read_response(&mut stream)?;
    let status_ok = head
        .lines()
        .next()
        .is_some_and(|status_line| status_line.contains(" 200"));
    if status_ok {
        Ok(body)
    } else {
        Err(BrowserError::CommandFailed {
            protocol: PROTOCOL,
            command: format!("{method} {path_and_query}"),
            message: format!("{} / {body}", head.lines().next().unwrap_or("")),
        })
    }
}

/// Читает HTTP-ответ: заголовки до пустой строки, затем тело по `Content-Length`.
fn read_response(stream: &mut TcpStream) -> Result<(String, String), BrowserError> {
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    let (head_end, body_length) = loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|source| BrowserError::io("read DevTools response", source))?;
        if read == 0 {
            return Err(BrowserError::protocol(
                PROTOCOL,
                "connection closed before headers ended",
            ));
        }
        received.extend_from_slice(&chunk[..read]);
        if let Some(position) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&received[..position]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            break (position + 4, length);
        }
    };
    while received.len() < head_end + body_length {
        let read = stream
            .read(&mut chunk)
            .map_err(|source| BrowserError::io("read DevTools response body", source))?;
        if read == 0 {
            break;
        }
        received.extend_from_slice(&chunk[..read]);
    }
    let head = String::from_utf8_lossy(&received[..head_end]).into_owned();
    let body = String::from_utf8_lossy(&received[head_end..]).into_owned();
    Ok((head, body))
}

/// Разобранный JSON-ответ DevTools.
pub(crate) fn http_json(
    port: u16,
    method: &str,
    path_and_query: &str,
) -> Result<Value, BrowserError> {
    let body = http_request(port, method, path_and_query)?;
    serde_json::from_str(&body).map_err(|error| {
        BrowserError::protocol(
            PROTOCOL,
            format!("{path_and_query}: not JSON ({error}): {body}"),
        )
    })
}

/// Соединение CDP с одной целью (страницей или самим браузером).
pub(crate) struct CdpConnection {
    socket: WebSocket<TcpStream>,
    next_id: u64,
}

impl CdpConnection {
    /// Подключается по `webSocketDebuggerUrl` (`ws://127.0.0.1:port/devtools/...`).
    pub(crate) fn connect(socket_url: &str) -> Result<Self, BrowserError> {
        let address = socket_url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .ok_or_else(|| {
                BrowserError::protocol(PROTOCOL, format!("bad socket url {socket_url}"))
            })?;
        let stream = TcpStream::connect(address)
            .map_err(|source| BrowserError::io(format!("connect to {address}"), source))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(|source| BrowserError::io("set CDP socket timeouts", source))?;
        let (socket, _response) = tungstenite::client(socket_url, stream).map_err(|error| {
            BrowserError::protocol(
                PROTOCOL,
                format!("WebSocket handshake with {socket_url}: {error}"),
            )
        })?;
        Ok(Self { socket, next_id: 1 })
    }

    /// Вызывает метод и ждёт ответ с тем же `id` (события по дороге пропускаются).
    pub(crate) fn call(&mut self, method: &str, params: Value) -> Result<Value, BrowserError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "method": method, "params": params });
        self.socket
            .send(Message::text(request.to_string()))
            .map_err(|error| BrowserError::protocol(PROTOCOL, format!("send {method}: {error}")))?;
        loop {
            let message = self.socket.read().map_err(|error| {
                BrowserError::protocol(PROTOCOL, format!("read reply to {method}: {error}"))
            })?;
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(reply) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            if reply.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = reply.get("error") {
                return Err(BrowserError::CommandFailed {
                    protocol: PROTOCOL,
                    command: method.to_owned(),
                    message: error.to_string(),
                });
            }
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Видимый текст страницы (`document.body.innerText`); пустая строка, если body ещё нет.
    pub(crate) fn body_text(&mut self) -> Result<String, BrowserError> {
        let result = self.call(
            "Runtime.evaluate",
            json!({
                "expression": "document.body ? document.body.innerText : ''",
                "returnByValue": true,
            }),
        )?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(BrowserError::CommandFailed {
                protocol: PROTOCOL,
                command: "Runtime.evaluate".to_owned(),
                message: exception.to_string(),
            });
        }
        result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| BrowserError::protocol(PROTOCOL, format!("no string value in {result}")))
    }
}
