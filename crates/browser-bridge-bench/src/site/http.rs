//! Минимальный HTTP/1.1: разбор запроса и запись ответа. Соединение закрывается после ответа
//! (`Connection: close`) — keep-alive стенду не нужен, а код проще и без гонок.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

/// Предел размера строки запроса и заголовков вместе.
pub(super) const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Предел тела запроса (отчёт страницы и строки журнала — единицы килобайт).
pub(super) const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Разобранный запрос. Имена заголовков хранятся в нижнем регистре.
#[derive(Debug)]
pub(super) struct Request {
    pub method: String,
    /// Путь БЕЗ строки запроса и уже с раскодированными `%XX`.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Почему запрос не удалось прочитать: от этого зависит код ответа.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RequestError {
    /// Синтаксис не HTTP/1.x → 400.
    Malformed(&'static str),
    /// Заголовки длиннее [`MAX_HEAD_BYTES`] → 431.
    HeadTooLarge,
    /// Тело длиннее [`MAX_BODY_BYTES`] → 413.
    BodyTooLarge,
    /// `Transfer-Encoding: chunked` и т. п. не поддерживаем → 501.
    Unsupported(&'static str),
    /// Клиент закрыл соединение / таймаут чтения: отвечать некому.
    Disconnected,
}

/// Читает один запрос из потока.
pub(super) fn read_request(stream: &mut impl Read) -> Result<Request, RequestError> {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(position) = find_head_end(&buffer) {
            break position;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(RequestError::HeadTooLarge);
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(RequestError::Disconnected),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
        }
    };
    if head_end > MAX_HEAD_BYTES {
        return Err(RequestError::HeadTooLarge);
    }
    let head = std::str::from_utf8(&buffer[..head_end])
        .map_err(|_| RequestError::Malformed("head is not UTF-8"))?;
    let RequestHead {
        method,
        path,
        headers,
    } = parse_head(head)?;
    if headers.iter().any(|(name, _)| name == "transfer-encoding") {
        return Err(RequestError::Unsupported("transfer-encoding"));
    }
    let content_length = declared_length(&headers)?;
    if content_length > MAX_BODY_BYTES {
        return Err(RequestError::BodyTooLarge);
    }
    // Часть тела могла прийти вместе с заголовками.
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(RequestError::Disconnected),
            Ok(count) => body.extend_from_slice(&chunk[..count]),
        }
    }
    body.truncate(content_length);
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Строка запроса и заголовки (без тела).
struct RequestHead {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

fn parse_head(head: &str) -> Result<RequestHead, RequestError> {
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(RequestError::Malformed("empty request"))?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(RequestError::Malformed("bad request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(RequestError::Malformed("unsupported HTTP version"));
    }
    if !target.starts_with('/') {
        return Err(RequestError::Malformed("target must be an absolute path"));
    }
    let raw_path = target.split(['?', '#']).next().unwrap_or("");
    let path = percent_decode(raw_path).ok_or(RequestError::Malformed("bad percent-encoding"))?;
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or(RequestError::Malformed("bad header line"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(RequestHead {
        method: method.to_owned(),
        path,
        headers,
    })
}

fn declared_length(headers: &[(String, String)]) -> Result<usize, RequestError> {
    match headers.iter().find(|(name, _)| name == "content-length") {
        None => Ok(0),
        Some((_, value)) => value
            .parse()
            .map_err(|_| RequestError::Malformed("bad content-length")),
    }
}

/// Раскодирует `%XX`. Результат должен быть корректной UTF-8; иначе `None`.
fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Тело ответа: пусто, в памяти или кусок файла (без чтения файла целиком в память).
pub(super) enum Body {
    Empty,
    Bytes(Vec<u8>),
    FileRange {
        file: File,
        offset: u64,
        length: u64,
    },
}

impl Body {
    fn length(&self) -> u64 {
        match self {
            Body::Empty => 0,
            Body::Bytes(bytes) => bytes.len() as u64,
            Body::FileRange { length, .. } => *length,
        }
    }
}

pub(super) struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Body,
}

impl Response {
    pub(super) fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    pub(super) fn text(status: u16, message: &str) -> Self {
        Self::new(status)
            .with_header("Content-Type", "text/plain; charset=utf-8")
            .with_body(Body::Bytes(message.as_bytes().to_vec()))
    }

    pub(super) fn with_header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub(super) fn with_body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Пишет ответ. Для `HEAD` (`include_body == false`) заголовки те же, тело не отправляется.
pub(super) fn write_response(
    stream: &mut impl Write,
    response: Response,
    include_body: bool,
) -> io::Result<()> {
    let length = response.body.length();
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason_phrase(response.status)
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    // 204 по стандарту без Content-Length и без тела.
    if response.status != 204 {
        head.push_str(&format!("Content-Length: {length}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    if !include_body {
        return stream.flush();
    }
    match response.body {
        Body::Empty => {}
        Body::Bytes(bytes) => stream.write_all(&bytes)?,
        Body::FileRange {
            mut file,
            offset,
            length,
        } => {
            file.seek(SeekFrom::Start(offset))?;
            io::copy(&mut file.take(length), stream)?;
        }
    }
    stream.flush()
}
