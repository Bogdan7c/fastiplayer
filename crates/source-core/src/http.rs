use std::io::Read;

use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderMap, HeaderName, HeaderValue, LAST_MODIFIED, RANGE,
};

use crate::{
    ByteSource, CancellationToken, NotSeekableReason, RangeDiagnostics, Seekability, SourceError,
    SourceFingerprint, SourceResult, SourceRuntimeConfig, SourceValidators,
};

/// HTTP header, который внешний service layer передал как данные direct URL-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    /// Имя HTTP header-а.
    pub name: String,

    /// Значение HTTP header-а.
    pub value: String,
}

impl HttpHeader {
    /// Создаёт header без validation; validation выполняется при открытии source.
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Конфигурация HTTP Range source.
#[derive(Debug, Clone)]
pub struct HttpRangeSourceConfig {
    /// Direct media URL.
    pub url: String,

    /// Headers direct URL-а, полученные от caller-а.
    pub headers: Vec<HttpHeader>,

    /// Runtime-настройки, полученные из пользовательского config.
    pub source_config: SourceRuntimeConfig,
}

impl HttpRangeSourceConfig {
    /// Создаёт конфигурацию HTTP source без hardcoded service headers.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        headers: Vec<HttpHeader>,
        source_config: SourceRuntimeConfig,
    ) -> Self {
        Self {
            url: url.into(),
            headers,
            source_config,
        }
    }
}

/// HTTP byte source, который читает только через Range requests.
#[derive(Debug)]
pub struct HttpRangeSource {
    /// Reqwest blocking client с timeout-ами из config.
    client: Client,

    /// Direct media URL.
    url: String,

    /// Базовые headers direct URL-а.
    headers: HeaderMap,

    /// Текущий byte cursor.
    position: u64,

    /// Seekability, подтверждённая probe-запросом.
    seekability: Seekability,

    /// Validators из HTTP response.
    validators: SourceValidators,

    /// Длина source-а, если server сообщил total bytes.
    content_length: Option<u64>,

    /// Fingerprint source-а.
    fingerprint: SourceFingerprint,

    /// Range diagnostics для telemetry panel.
    diagnostics: RangeDiagnostics,
}

impl HttpRangeSource {
    /// Открывает HTTP Range source и выполняет seekability probe.
    pub fn open(config: HttpRangeSourceConfig) -> SourceResult<Self> {
        let client = Client::builder()
            .connect_timeout(config.source_config.connect_timeout())
            .timeout(config.source_config.read_timeout())
            .build()
            .map_err(|source| SourceError::HttpClientBuild { source })?;
        let headers = build_header_map(&config.headers)?;
        let probe = probe_seekability(&client, &config.url, &headers)?;
        let fingerprint =
            build_http_fingerprint(&config.url, probe.content_length, &probe.validators);

        Ok(Self {
            client,
            url: config.url,
            headers,
            position: 0,
            seekability: probe.seekability,
            validators: probe.validators,
            content_length: probe.content_length,
            fingerprint,
            diagnostics: RangeDiagnostics::default(),
        })
    }

    /// Возвращает текущие HTTP Range counters.
    #[must_use]
    pub const fn range_diagnostics(&self) -> RangeDiagnostics {
        self.diagnostics
    }

    /// Читает один bounded range с единственным retry при transport/body failure.
    fn read_range_with_retry(
        &mut self,
        offset: u64,
        output: &mut [u8],
        cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        let mut attempts = 0_u8;
        let length = output.len();

        loop {
            let result = self.read_range_once(offset, output, cancellation);
            match result {
                Ok(bytes_read) => return Ok(bytes_read),
                Err(error) if attempts == 0 && error.is_retryable_range_failure() => {
                    self.record_range_error(&error);
                    attempts = attempts.saturating_add(1);
                    tracing::warn!(
                        url = %self.url,
                        offset,
                        length,
                        error = %error,
                        "HTTP Range read failed; retrying once"
                    );
                }
                Err(error) => {
                    self.record_range_error(&error);
                    return Err(error);
                }
            }
        }
    }

    /// Выполняет один HTTP Range request и полностью читает response body.
    fn read_range_once(
        &mut self,
        offset: u64,
        output: &mut [u8],
        cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let length = output.len();
        let range = ByteRange::new(offset, length);
        self.diagnostics.range_requests = self.diagnostics.range_requests.saturating_add(1);
        self.diagnostics.bytes_requested = self
            .diagnostics
            .bytes_requested
            .saturating_add(length as u64);
        let response =
            send_range_request(&self.client, &self.url, &self.headers, &range, "range-read")?;

        if response.status() != StatusCode::PARTIAL_CONTENT {
            if response.status() == StatusCode::OK {
                return Err(SourceError::HttpRangeUnsupported {
                    reason: NotSeekableReason::HttpRangeStatus {
                        status: response.status().as_u16(),
                    },
                });
            }

            return Err(SourceError::HttpStatus {
                operation: "range-read",
                url: self.url.clone(),
                status: response.status(),
            });
        }

        validate_content_range(&self.url, response.headers(), &range)?;
        let bytes_read =
            read_response_body_into(&self.url, response, offset, output, cancellation)?;
        self.diagnostics.bytes_read = self
            .diagnostics
            .bytes_read
            .saturating_add(bytes_read as u64);
        Ok(bytes_read)
    }

    /// Учитывает ошибку range path в diagnostics.
    fn record_range_error(&mut self, error: &SourceError) {
        if matches!(error, SourceError::HttpTimeout { .. }) {
            self.diagnostics.timeouts = self.diagnostics.timeouts.saturating_add(1);
        }
    }
}

impl ByteSource for HttpRangeSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }

        if let Seekability::NotSeekable { reason } = &self.seekability {
            return Err(SourceError::NotSeekable {
                reason: reason.clone(),
            });
        }

        let requested_length =
            bounded_read_length(self.position, output.len(), self.content_length);
        if requested_length == 0 {
            return Ok(0);
        }

        let bytes_read = self.read_range_with_retry(
            self.position,
            &mut output[..requested_length],
            cancellation,
        )?;
        self.position = self.position.saturating_add(bytes_read as u64);
        Ok(bytes_read)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.position = offset;
        Ok(())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        self.seekability.clone()
    }

    fn validators(&self) -> SourceValidators {
        self.validators.clone()
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.fingerprint.clone()
    }
}

#[derive(Debug, Clone, Copy)]
struct ByteRange {
    /// Начальный byte offset.
    start: u64,

    /// Количество bytes в range.
    length: usize,
}

impl ByteRange {
    /// Создаёт bounded byte range.
    const fn new(start: u64, length: usize) -> Self {
        Self { start, length }
    }

    /// Возвращает последний byte offset включительно.
    fn end_inclusive(self) -> u64 {
        self.start
            .saturating_add(self.length as u64)
            .saturating_sub(1)
    }

    /// Форматирует header `Range`.
    fn header_value(self) -> String {
        format!("bytes={}-{}", self.start, self.end_inclusive())
    }
}

#[derive(Debug)]
struct ProbeResult {
    /// Seekability после Range probe.
    seekability: Seekability,

    /// Validators из probe response.
    validators: SourceValidators,

    /// Длина source-а, если известна.
    content_length: Option<u64>,
}

#[derive(Debug)]
struct ParsedContentRange {
    /// Начальный offset response range.
    start: u64,

    /// Конечный offset response range включительно.
    end_inclusive: u64,

    /// Общая длина source-а, если server её сообщил.
    total_length: Option<u64>,
}

/// Выполняет `Range: bytes=0-0` и подтверждает seekability только через 206.
fn probe_seekability(client: &Client, url: &str, headers: &HeaderMap) -> SourceResult<ProbeResult> {
    let probe_range = ByteRange::new(0, 1);
    let response = send_range_request(client, url, headers, &probe_range, "range-probe")?;
    let validators = validators_from_headers(response.headers());

    if response.status() == StatusCode::PARTIAL_CONTENT {
        let parsed_range = parse_content_range_header(url, response.headers())?;
        return Ok(ProbeResult {
            seekability: Seekability::Seekable,
            validators,
            content_length: parsed_range.total_length,
        });
    }

    if response.status() == StatusCode::OK {
        return Ok(ProbeResult {
            seekability: Seekability::NotSeekable {
                reason: NotSeekableReason::HttpRangeStatus {
                    status: response.status().as_u16(),
                },
            },
            validators,
            content_length: response.content_length().or_else(|| {
                response
                    .headers()
                    .get(CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
            }),
        });
    }

    Err(SourceError::HttpStatus {
        operation: "range-probe",
        url: url.to_string(),
        status: response.status(),
    })
}

/// Отправляет GET с Range header, сохраняя service-provided headers как данные.
fn send_range_request(
    client: &Client,
    url: &str,
    headers: &HeaderMap,
    range: &ByteRange,
    operation: &'static str,
) -> SourceResult<reqwest::blocking::Response> {
    let mut request_headers = headers.clone();
    request_headers.remove(RANGE);
    request_headers.insert(
        RANGE,
        HeaderValue::from_str(&range.header_value()).map_err(|_| {
            SourceError::InvalidHttpHeaderValue {
                name: RANGE.as_str().to_string(),
            }
        })?,
    );

    client
        .get(url)
        .headers(request_headers)
        .send()
        .map_err(|source| map_reqwest_error(operation, url, source))
}

/// Валидирует, что `Content-Range` соответствует запрошенному range.
fn validate_content_range(url: &str, headers: &HeaderMap, range: &ByteRange) -> SourceResult<()> {
    let parsed_range = parse_content_range_header(url, headers)?;
    if parsed_range.start != range.start || parsed_range.end_inclusive != range.end_inclusive() {
        let header = headers
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<invalid utf-8>")
            .to_string();
        return Err(SourceError::InvalidContentRange {
            url: url.to_string(),
            header,
        });
    }

    Ok(())
}

/// Читает ровно ожидаемое количество bytes из response body в caller buffer.
fn read_response_body_into(
    url: &str,
    mut response: reqwest::blocking::Response,
    offset: u64,
    output: &mut [u8],
    cancellation: &CancellationToken,
) -> SourceResult<usize> {
    let expected_length = output.len();
    let mut total_read = 0_usize;

    while total_read < expected_length {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        match response.read(&mut output[total_read..]) {
            Ok(0) => break,
            Ok(bytes_read) => {
                total_read = total_read.saturating_add(bytes_read);
            }
            Err(source) if source.kind() == std::io::ErrorKind::TimedOut => {
                return Err(SourceError::HttpTimeout {
                    operation: "range-read",
                    url: url.to_string(),
                });
            }
            Err(source) => {
                return Err(SourceError::HttpBodyRead {
                    operation: "range-read",
                    url: url.to_string(),
                    source,
                });
            }
        }
    }

    if total_read != expected_length {
        return Err(SourceError::UnexpectedEof {
            offset,
            expected_bytes: expected_length,
            actual_bytes: total_read,
        });
    }

    Ok(total_read)
}

/// Строит reqwest HeaderMap из внешних headers.
fn build_header_map(headers: &[HttpHeader]) -> SourceResult<HeaderMap> {
    let mut header_map = HeaderMap::new();

    for header in headers {
        let header_name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| {
            SourceError::InvalidHttpHeaderName {
                name: header.name.clone(),
            }
        })?;
        let header_value = HeaderValue::from_str(&header.value).map_err(|_| {
            SourceError::InvalidHttpHeaderValue {
                name: header.name.clone(),
            }
        })?;
        header_map.insert(header_name, header_value);
    }

    Ok(header_map)
}

/// Достаёт validators из HTTP response headers.
fn validators_from_headers(headers: &HeaderMap) -> SourceValidators {
    SourceValidators {
        etag: headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
        last_modified: headers
            .get(LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
    }
}

/// Парсит обязательный для 206 response header `Content-Range`.
fn parse_content_range_header(url: &str, headers: &HeaderMap) -> SourceResult<ParsedContentRange> {
    let header = headers
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| SourceError::InvalidContentRange {
            url: url.to_string(),
            header: "<missing>".to_string(),
        })?;

    parse_content_range_value(header).ok_or_else(|| SourceError::InvalidContentRange {
        url: url.to_string(),
        header: header.to_string(),
    })
}

/// Парсит значение вида `bytes 0-99/1000`.
fn parse_content_range_value(header: &str) -> Option<ParsedContentRange> {
    let range_and_total = header.strip_prefix("bytes ")?;
    let (range_part, total_part) = range_and_total.split_once('/')?;
    let (start, end_inclusive) = range_part.split_once('-')?;
    let total_length = if total_part == "*" {
        None
    } else {
        Some(total_part.parse::<u64>().ok()?)
    };

    Some(ParsedContentRange {
        start: start.parse::<u64>().ok()?,
        end_inclusive: end_inclusive.parse::<u64>().ok()?,
        total_length,
    })
}

/// Ограничивает read известной длиной source-а.
fn bounded_read_length(offset: u64, requested_length: usize, content_length: Option<u64>) -> usize {
    let Some(content_length) = content_length else {
        return requested_length;
    };

    if offset >= content_length {
        return 0;
    }

    let remaining = content_length - offset;
    requested_length.min(usize::try_from(remaining).unwrap_or(usize::MAX))
}

/// Нормализует reqwest errors в player/UI-friendly source errors.
fn map_reqwest_error(operation: &'static str, url: &str, source: reqwest::Error) -> SourceError {
    if source.is_timeout() {
        SourceError::HttpTimeout {
            operation,
            url: url.to_string(),
        }
    } else {
        SourceError::HttpRequest {
            operation,
            url: url.to_string(),
            source,
        }
    }
}

/// Строит fingerprint из URL, validators и длины, не добавляя service-specific logic.
fn build_http_fingerprint(
    url: &str,
    content_length: Option<u64>,
    validators: &SourceValidators,
) -> SourceFingerprint {
    SourceFingerprint::new(format!(
        "http:{}:{}:{}:{}",
        url,
        content_length
            .map(|length| length.to_string())
            .unwrap_or_else(|| "unknown-length".to_string()),
        validators.etag.as_deref().unwrap_or("no-etag"),
        validators
            .last_modified
            .as_deref()
            .unwrap_or("no-last-modified")
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Clone)]
    struct TestRequest {
        headers: BTreeMap<String, String>,
    }

    struct TestHttpServer {
        url: String,
        address: SocketAddr,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
        requests: Arc<Mutex<Vec<TestRequest>>>,
    }

    impl TestHttpServer {
        fn spawn(handler: impl Fn(usize, TestRequest, TcpStream) + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("test server bound");
            listener
                .set_nonblocking(true)
                .expect("test server nonblocking");
            let address = listener.local_addr().expect("test server address");
            let url = format!("http://{address}/media.bin");
            let stop = Arc::new(AtomicBool::new(false));
            let stop_for_thread = Arc::clone(&stop);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let requests_for_thread = Arc::clone(&requests);
            let handler = Arc::new(handler);

            let handle = thread::spawn(move || {
                let mut request_index = 0_usize;
                while !stop_for_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stop_for_thread.load(Ordering::SeqCst) {
                                break;
                            }

                            let Ok(request) = read_test_request(&stream) else {
                                continue;
                            };
                            requests_for_thread
                                .lock()
                                .expect("requests lock")
                                .push(request.clone());
                            handler(request_index, request, stream);
                            request_index = request_index.saturating_add(1);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                url,
                address,
                stop,
                handle: Some(handle),
                requests,
            }
        }

        fn config(&self, headers: Vec<HttpHeader>) -> HttpRangeSourceConfig {
            HttpRangeSourceConfig::new(
                self.url.clone(),
                headers,
                SourceRuntimeConfig::for_tests(
                    1024 * 1024,
                    Duration::from_millis(100),
                    Duration::from_millis(100),
                ),
            )
        }

        fn requests(&self) -> Vec<TestRequest> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(self.address);
            if let Some(handle) = self.handle.take() {
                handle.join().expect("test server joined");
            }
        }
    }

    fn read_test_request(stream: &TcpStream) -> std::io::Result<TestRequest> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;

        let mut headers = BTreeMap::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let trimmed_line = line.trim_end_matches(['\r', '\n']);
            if trimmed_line.is_empty() {
                break;
            }

            if let Some((name, value)) = trimmed_line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }

        Ok(TestRequest { headers })
    }

    fn write_response(
        mut stream: TcpStream,
        status: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) {
        write!(stream, "HTTP/1.1 {status}\r\n").expect("status written");
        for (name, value) in headers {
            write!(stream, "{name}: {value}\r\n").expect("header written");
        }
        write!(stream, "Connection: close\r\n\r\n").expect("headers finished");
        stream.write_all(body).expect("body written");
        stream.flush().expect("response flushed");
    }

    fn respond_with_range(stream: TcpStream, request: &TestRequest, media: &[u8]) {
        let (start, end) = parse_test_range(
            request
                .headers
                .get("range")
                .expect("range header is present"),
        );
        let body = &media[start..=end];
        write_response(
            stream,
            "206 Partial Content",
            &[
                ("Content-Length", body.len().to_string()),
                (
                    "Content-Range",
                    format!("bytes {start}-{end}/{}", media.len()),
                ),
                ("ETag", "\"test-etag\"".to_string()),
                ("Last-Modified", "Tue, 12 May 2026 10:00:00 GMT".to_string()),
            ],
            body,
        );
    }

    fn parse_test_range(range_header: &str) -> (usize, usize) {
        let range = range_header
            .strip_prefix("bytes=")
            .expect("bytes range prefix");
        let (start, end) = range.split_once('-').expect("range separator");
        (
            start.parse::<usize>().expect("range start"),
            end.parse::<usize>().expect("range end"),
        )
    }

    #[test]
    fn http_source_reads_206_ranges_and_preserves_headers() {
        let media = Arc::new(b"0123456789".to_vec());
        let media_for_server = Arc::clone(&media);
        let server = TestHttpServer::spawn(move |_index, request, stream| {
            assert_eq!(
                request.headers.get("x-direct-token").map(String::as_str),
                Some("token-1")
            );
            respond_with_range(stream, &request, &media_for_server);
        });

        let mut source = HttpRangeSource::open(
            server.config(vec![HttpHeader::new("X-Direct-Token", "token-1")]),
        )
        .expect("http source opened");
        let token = CancellationToken::never_cancelled();
        let mut output = [0_u8; 4];

        assert!(source.seekability().is_seekable());
        assert_eq!(source.content_length(), Some(media.len() as u64));
        assert_eq!(source.validators().etag.as_deref(), Some("\"test-etag\""));

        let bytes_read = source.read(&mut output, &token).expect("range read works");
        assert_eq!(bytes_read, 4);
        assert_eq!(&output, b"0123");

        source.seek(5).expect("seek works");
        let mut second_output = [0_u8; 3];
        let bytes_read = source
            .read(&mut second_output, &token)
            .expect("second range read works");
        assert_eq!(bytes_read, 3);
        assert_eq!(&second_output, b"567");

        let ranges = server
            .requests()
            .into_iter()
            .filter_map(|request| request.headers.get("range").cloned())
            .collect::<Vec<_>>();
        assert_eq!(ranges, vec!["bytes=0-0", "bytes=0-3", "bytes=5-7"]);
        let diagnostics = source.range_diagnostics();
        assert_eq!(diagnostics.range_requests, 2);
        assert_eq!(diagnostics.bytes_requested, 7);
        assert_eq!(diagnostics.bytes_read, 7);
        assert_eq!(diagnostics.timeouts, 0);
    }

    #[test]
    fn http_source_returns_partial_tail_read_at_content_length() {
        let media = Arc::new(b"0123456789".to_vec());
        let media_for_server = Arc::clone(&media);
        let server = TestHttpServer::spawn(move |_index, request, stream| {
            respond_with_range(stream, &request, &media_for_server);
        });

        let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
        let token = CancellationToken::never_cancelled();
        let mut output = *b"XXXXXXXX";

        source.seek(7).expect("seek to tail works");
        let bytes_read = source
            .read(&mut output, &token)
            .expect("tail range read works");

        assert_eq!(bytes_read, 3);
        assert_eq!(&output, b"789XXXXX");
        assert_eq!(source.position(), 10);

        let ranges = server
            .requests()
            .into_iter()
            .filter_map(|request| request.headers.get("range").cloned())
            .collect::<Vec<_>>();
        assert_eq!(ranges, vec!["bytes=0-0", "bytes=7-9"]);

        let diagnostics = source.range_diagnostics();
        assert_eq!(diagnostics.range_requests, 1);
        assert_eq!(diagnostics.bytes_requested, 3);
        assert_eq!(diagnostics.bytes_read, 3);
    }

    #[test]
    fn http_source_reports_not_seekable_when_range_returns_200() {
        let media = b"plain-body".to_vec();
        let media_for_server = media.clone();
        let server = TestHttpServer::spawn(move |_index, _request, stream| {
            write_response(
                stream,
                "200 OK",
                &[("Content-Length", media_for_server.len().to_string())],
                &media_for_server,
            );
        });

        let mut source =
            HttpRangeSource::open(server.config(Vec::new())).expect("non-range http source opens");

        assert_eq!(
            source.seekability(),
            Seekability::NotSeekable {
                reason: NotSeekableReason::HttpRangeStatus { status: 200 }
            }
        );
        assert_eq!(source.content_length(), Some(media.len() as u64));

        let mut output = [0_u8; 4];
        let error = source
            .read(&mut output, &CancellationToken::never_cancelled())
            .expect_err("range read is rejected for non-seekable source");
        assert!(matches!(error, SourceError::NotSeekable { .. }));
    }

    #[test]
    fn http_source_reports_timeout() {
        let server = TestHttpServer::spawn(move |_index, _request, _stream| {
            thread::sleep(Duration::from_millis(250));
        });
        let config = HttpRangeSourceConfig::new(
            server.url.clone(),
            Vec::new(),
            SourceRuntimeConfig::for_tests(
                1024,
                Duration::from_millis(50),
                Duration::from_millis(50),
            ),
        );

        let error = HttpRangeSource::open(config).expect_err("probe times out");
        assert!(matches!(error, SourceError::HttpTimeout { .. }));
    }

    #[test]
    fn cancelled_range_read_returns_cancelled_without_advancing_position() {
        let media = Arc::new(b"abcdefghij".to_vec());
        let media_for_server = Arc::clone(&media);
        let cancellation = CancellationToken::new();
        let cancellation_for_server = cancellation.clone();
        let server = TestHttpServer::spawn(move |index, request, stream| {
            if index == 1 {
                cancellation_for_server.cancel();
            }

            respond_with_range(stream, &request, &media_for_server);
        });

        let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
        let mut output = [0_u8; 5];
        let error = source
            .read(&mut output, &cancellation)
            .expect_err("range read observes cancellation");

        assert!(matches!(error, SourceError::Cancelled));
        assert_eq!(source.position(), 0);
        assert_eq!(server.requests().len(), 2);

        let diagnostics = source.range_diagnostics();
        assert_eq!(diagnostics.range_requests, 1);
        assert_eq!(diagnostics.bytes_requested, 5);
        assert_eq!(diagnostics.bytes_read, 0);
    }

    #[test]
    fn interrupted_range_response_retries_once() {
        let media = Arc::new(b"abcdefghij".to_vec());
        let media_for_server = Arc::clone(&media);
        let server = TestHttpServer::spawn(move |index, request, mut stream| {
            if index == 1 {
                let (start, end) = parse_test_range(request.headers.get("range").unwrap());
                let body = &media_for_server[start..=end];
                write!(stream, "HTTP/1.1 206 Partial Content\r\n").expect("status written");
                write!(stream, "Content-Length: {}\r\n", body.len()).expect("length written");
                write!(
                    stream,
                    "Content-Range: bytes {start}-{end}/{}\r\n",
                    media_for_server.len()
                )
                .expect("range written");
                write!(stream, "Connection: close\r\n\r\n").expect("headers written");
                stream.write_all(&body[..2]).expect("partial body written");
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }

            respond_with_range(stream, &request, &media_for_server);
        });

        let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
        let mut output = [0_u8; 5];
        let bytes_read = source
            .read(&mut output, &CancellationToken::never_cancelled())
            .expect("interrupted read retried");

        assert_eq!(bytes_read, 5);
        assert_eq!(&output, b"abcde");
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn range_failure_retries_once() {
        let media = Arc::new(b"abcdefghij".to_vec());
        let media_for_server = Arc::clone(&media);
        let server = TestHttpServer::spawn(move |index, request, stream| {
            if index == 1 {
                write_response(
                    stream,
                    "500 Internal Server Error",
                    &[("Content-Length", "0".to_string())],
                    b"",
                );
                return;
            }

            respond_with_range(stream, &request, &media_for_server);
        });

        let mut source = HttpRangeSource::open(server.config(Vec::new())).expect("source opens");
        let mut output = [0_u8; 4];
        let bytes_read = source
            .read(&mut output, &CancellationToken::never_cancelled())
            .expect("server error retried once");

        assert_eq!(bytes_read, 4);
        assert_eq!(&output, b"abcd");
        assert_eq!(server.requests().len(), 3);
    }
}
