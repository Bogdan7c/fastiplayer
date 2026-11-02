//! Клиент Marionette — протокола управления Firefox (то, на чём стоит geckodriver).
//!
//! Формат кадра: `<длина в байтах>:<JSON>`. Сервер первым присылает приветствие
//! `{"applicationType":"gecko","marionetteProtocol":3}`. Команда — массив
//! `[0, id, "Имя", {параметры}]`, ответ — `[1, id, ошибка|null, результат|null]`.
//! BiDi не пускает на `moz-extension://` (сессия 07), поэтому стенд берёт Marionette.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::BrowserError;

const PROTOCOL: &str = "Marionette";
/// Потолок на чтение одного кадра.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Самая длинная допустимая запись длины кадра (десятичные цифры).
const MAX_LENGTH_DIGITS: usize = 10;
/// Тип кадра «ответ» в протоколе.
const RESPONSE_KIND: u64 = 1;

/// Соединение с Marionette одного запущенного Firefox.
pub(crate) struct Marionette {
    stream: TcpStream,
    next_id: u64,
}

impl Marionette {
    /// Подключается к порту, повторяя попытки: Marionette поднимается не сразу после старта.
    pub(crate) fn connect(port: u16, timeout: Duration) -> Result<Self, BrowserError> {
        let deadline = Instant::now() + timeout;
        loop {
            match Self::try_connect(port) {
                Ok(connection) => return Ok(connection),
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_not_ready_yet) => std::thread::sleep(Duration::from_millis(300)),
            }
        }
    }

    fn try_connect(port: u16) -> Result<Self, BrowserError> {
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|source| {
            BrowserError::io(format!("connect to Marionette port {port}"), source)
        })?;
        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(READ_TIMEOUT)))
            .map_err(|source| BrowserError::io("set Marionette socket timeouts", source))?;
        let mut connection = Self { stream, next_id: 1 };
        let hello = connection.read_frame()?;
        if hello.get("marionetteProtocol").is_none() {
            return Err(BrowserError::protocol(
                PROTOCOL,
                format!("unexpected hello {hello}"),
            ));
        }
        Ok(connection)
    }

    /// Выполняет команду и возвращает результат (`null` как `Value::Null`).
    pub(crate) fn command(&mut self, name: &str, parameters: Value) -> Result<Value, BrowserError> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!([0, id, name, parameters]).to_string();
        let frame = format!("{}:{payload}", payload.len());
        self.stream
            .write_all(frame.as_bytes())
            .map_err(|source| BrowserError::io(format!("send Marionette {name}"), source))?;
        loop {
            let reply = self.read_frame()?;
            let Some(parts) = reply.as_array() else {
                continue;
            };
            let is_our_reply = parts.first().and_then(Value::as_u64) == Some(RESPONSE_KIND)
                && parts.get(1).and_then(Value::as_u64) == Some(id);
            if !is_our_reply {
                continue;
            }
            let failure = parts.get(2).filter(|value| !value.is_null());
            if let Some(failure) = failure {
                return Err(BrowserError::CommandFailed {
                    protocol: PROTOCOL,
                    command: name.to_owned(),
                    message: failure.to_string(),
                });
            }
            return Ok(parts.get(3).cloned().unwrap_or(Value::Null));
        }
    }

    /// Читает один кадр `<длина>:<JSON>`.
    fn read_frame(&mut self) -> Result<Value, BrowserError> {
        let mut digits = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            self.stream
                .read_exact(&mut byte)
                .map_err(|source| BrowserError::io("read Marionette frame length", source))?;
            if byte[0] == b':' {
                break;
            }
            digits.push(byte[0]);
            if digits.len() > MAX_LENGTH_DIGITS {
                return Err(BrowserError::protocol(PROTOCOL, "frame length is too long"));
            }
        }
        let length: usize = String::from_utf8_lossy(&digits)
            .parse()
            .map_err(|_| BrowserError::protocol(PROTOCOL, "frame length is not a number"))?;
        let mut body = vec![0_u8; length];
        self.stream
            .read_exact(&mut body)
            .map_err(|source| BrowserError::io("read Marionette frame body", source))?;
        serde_json::from_slice(&body).map_err(|error| {
            BrowserError::protocol(PROTOCOL, format!("frame is not JSON: {error}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Поддельный сервер Marionette: приветствие, затем один ответ на любую команду.
    fn fake_server(reply_error: Option<Value>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let hello = r#"{"applicationType":"gecko","marionetteProtocol":3}"#;
            stream
                .write_all(format!("{}:{hello}", hello.len()).as_bytes())
                .expect("hello");
            // Читаем команду: длина, двоеточие, тело.
            let mut length = String::new();
            loop {
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte).expect("length byte");
                if byte[0] == b':' {
                    break;
                }
                length.push(char::from(byte[0]));
            }
            let mut body = vec![0_u8; length.parse().expect("length")];
            stream.read_exact(&mut body).expect("body");
            let command: Value = serde_json::from_slice(&body).expect("command json");
            let id = command[1].clone();
            let reply = match reply_error {
                Some(error) => json!([1, id, error, null]),
                None => json!([1, id, null, { "value": command[2] }]),
            }
            .to_string();
            stream
                .write_all(format!("{}:{reply}", reply.len()).as_bytes())
                .expect("reply");
        });
        port
    }

    #[test]
    fn command_round_trip_returns_the_result() {
        let port = fake_server(None);
        let mut client = Marionette::connect(port, Duration::from_secs(5)).expect("connect");
        let result = client
            .command("WebDriver:Navigate", json!({ "url": "http://x/" }))
            .expect("command succeeds");
        // Поддельный сервер возвращает имя полученной команды: так видно, что ушло в сеть.
        assert_eq!(result["value"], "WebDriver:Navigate");
    }

    #[test]
    fn error_reply_becomes_command_failed() {
        let port = fake_server(Some(json!({ "error": "no such window" })));
        let mut client = Marionette::connect(port, Duration::from_secs(5)).expect("connect");
        let error = client
            .command("WebDriver:SwitchToWindow", json!({ "handle": "x" }))
            .expect_err("server refuses");
        assert!(matches!(
            error,
            BrowserError::CommandFailed { ref command, ref message, .. }
                if command == "WebDriver:SwitchToWindow" && message.contains("no such window")
        ));
    }
}
