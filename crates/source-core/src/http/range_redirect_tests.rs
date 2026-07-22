//! Focused tests read-time HTTP Range redirect mechanics.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{
    ByteSource, CancellationToken, HttpRangeRedirectBodyForwarding, HttpRangeRedirectHandler,
    HttpRangeRedirectHopCount, HttpRangeRedirectRejection, HttpRangeRedirectRequestMaterial,
    HttpRedirectHop, HttpRequestBody, HttpRequestTarget, HttpSingleHopRequest, HttpSourceHop,
    HttpSourceSession, SourceRuntimeConfig,
};

/// Тело, которое должно остаться только на stable base POST request-е.
const SECRET_REQUEST_BODY: &str = "ephemeral-test-body";

/// Локальный server: base range перенаправляет через `302 -> 307` на final 206.
struct RedirectRangeServer {
    /// Loopback listener address.
    address: SocketAddr,
    /// Captured requests нужны только focused assertions.
    requests: Arc<Mutex<Vec<String>>>,
    /// Cooperative accept-loop stop.
    stop: Arc<AtomicBool>,
    /// Join handle исключает утечку test thread-а.
    join_handle: Option<JoinHandle<()>>,
}

impl RedirectRangeServer {
    /// Запускает nonblocking hermetic fixture с immutable media bytes.
    fn spawn(media_bytes: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect range server");
        listener
            .set_nonblocking(true)
            .expect("set redirect range server nonblocking");
        let address = listener.local_addr().expect("read redirect server address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::Builder::new()
            .name("source-core-range-redirect-test".to_owned())
            .spawn(move || {
                while !worker_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request = read_request(&mut stream);
                            worker_requests
                                .lock()
                                .expect("redirect request capture")
                                .push(request.clone());
                            respond_to_request(&mut stream, &media_bytes, &request);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => return,
                    }
                }
            })
            .expect("spawn redirect range server");
        Self {
            address,
            requests,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// Возвращает exact stable base URL fixture-а.
    fn base_url(&self) -> String {
        format!("http://{}/base", self.address)
    }

    /// Возвращает snapshot captured requests в физическом порядке.
    fn captured_requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("redirect request capture")
            .clone()
    }
}

impl Drop for RedirectRangeServer {
    /// Останавливает server и дожидается worker-а даже при failed assertion.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// Fake policy намеренно просит сохранить body на каждом hop-е.
struct PreserveBodyRedirectHandler {
    /// Сколько независимых logical reads начал source.
    begin_count: Arc<AtomicUsize>,
    /// Какие per-read completed hop counts увидел boundary.
    observed_hop_counts: Arc<Mutex<Vec<u8>>>,
}

impl HttpRangeRedirectHandler for PreserveBodyRedirectHandler {
    /// Регистрирует сброс redirect chain перед logical read/retry.
    fn begin_range_request(&mut self) {
        self.begin_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Возвращает permissive material; source обязан всё равно применить 302 semantics.
    fn material_for_redirect(
        &mut self,
        _current_target: &HttpRequestTarget,
        _redirect: &HttpRedirectHop,
        completed_hops: HttpRangeRedirectHopCount,
    ) -> Result<HttpRangeRedirectRequestMaterial, HttpRangeRedirectRejection> {
        self.observed_hop_counts
            .lock()
            .expect("observed hop counts")
            .push(completed_hops.value());
        Ok(HttpRangeRedirectRequestMaterial::new(
            Vec::new(),
            HttpRangeRedirectBodyForwarding::PreserveCurrent,
        ))
    }
}

/// Stable base повторяется на каждом read, а `302 -> 307` не воскрешает POST body.
#[test]
fn range_redirect_restarts_stable_base_and_never_resurrects_body() {
    let media_bytes = b"0123456789abcdef".to_vec();
    let server = RedirectRangeServer::spawn(media_bytes.clone());
    let source_config =
        SourceRuntimeConfig::for_tests(1024 * 1024, Duration::from_secs(1), Duration::from_secs(2));
    let source_session = HttpSourceSession::new(&source_config).expect("HTTP source session");
    let base_target =
        HttpRequestTarget::parse_exact(server.base_url()).expect("valid stable base target");
    let opened = source_session
        .open_single_hop(
            HttpSingleHopRequest::new(
                base_target,
                Vec::new(),
                HttpRequestBody::Bytes(SECRET_REQUEST_BODY.as_bytes().to_vec()),
            ),
            &CancellationToken::new(),
        )
        .expect("open stable seekable source");
    let HttpSourceHop::Seekable(source) = opened else {
        panic!("fixture must return seekable source");
    };
    let begin_count = Arc::new(AtomicUsize::new(0));
    let observed_hop_counts = Arc::new(Mutex::new(Vec::new()));
    let mut source = source.with_range_redirect_handler(Box::new(PreserveBodyRedirectHandler {
        begin_count: Arc::clone(&begin_count),
        observed_hop_counts: Arc::clone(&observed_hop_counts),
    }));

    let mut first_output = [0_u8; 4];
    let first_read = source
        .read(&mut first_output, &CancellationToken::new())
        .expect("first redirected range read");
    let mut second_output = [0_u8; 4];
    let second_read = source
        .read(&mut second_output, &CancellationToken::new())
        .expect("second redirected range read");

    assert_eq!(&first_output[..first_read], &media_bytes[..4]);
    assert_eq!(&second_output[..second_read], &media_bytes[4..8]);
    assert_eq!(begin_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        *observed_hop_counts.lock().expect("observed hop counts"),
        vec![0, 1, 0, 1]
    );
    let diagnostics = source.range_diagnostics();
    assert_eq!(diagnostics.range_requests, 6);
    assert_eq!(diagnostics.bytes_requested, 24);

    let requests = server.captured_requests();
    let base_requests = requests
        .iter()
        .filter(|request| request_path(request) == "/base")
        .collect::<Vec<_>>();
    let middle_requests = requests
        .iter()
        .filter(|request| request_path(request) == "/middle")
        .collect::<Vec<_>>();
    let final_requests = requests
        .iter()
        .filter(|request| request_path(request) == "/final")
        .collect::<Vec<_>>();
    assert_eq!(base_requests.len(), 3, "probe + two stable base reads");
    assert_eq!(middle_requests.len(), 2);
    assert_eq!(final_requests.len(), 2);
    assert!(base_requests.iter().all(|request| {
        request.starts_with("POST ") && request_body(request) == SECRET_REQUEST_BODY
    }));
    assert!(
        middle_requests
            .iter()
            .all(|request| { request.starts_with("GET ") && request_body(request).is_empty() })
    );
    assert!(
        final_requests
            .iter()
            .all(|request| { request.starts_with("GET ") && request_body(request).is_empty() })
    );
}

/// Читает один complete HTTP/1 request, включая declared body.
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set request read timeout");
    let mut request_bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let bytes_read = stream.read(&mut chunk).expect("read test request");
        if bytes_read == 0 {
            break;
        }
        request_bytes.extend_from_slice(&chunk[..bytes_read]);
        if request_is_complete(&request_bytes) {
            break;
        }
    }
    String::from_utf8(request_bytes).expect("test request is UTF-8")
}

/// Проверяет, получены ли headers и declared Content-Length body.
fn request_is_complete(request_bytes: &[u8]) -> bool {
    let Some(header_end) = request_bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
    else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request_bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    request_bytes.len() >= header_end + 4 + content_length
}

/// Возвращает request path без остальных operational headers/body.
fn request_path(request: &str) -> &str {
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("request path")
}

/// Возвращает fixture body после HTTP header terminator-а.
fn request_body(request: &str) -> &str {
    request.split_once("\r\n\r\n").map_or("", |(_, body)| body)
}

/// Отвечает согласно exact fixture path.
fn respond_to_request(stream: &mut TcpStream, media_bytes: &[u8], request: &str) {
    match request_path(request) {
        "/base" if requested_range(request) == (0, 0) => {
            respond_partial_content(stream, media_bytes, 0, 0);
        }
        "/base" => respond_redirect(stream, 302, "/middle"),
        "/middle" => respond_redirect(stream, 307, "/final"),
        "/final" => {
            let (start, end) = requested_range(request);
            respond_partial_content(stream, media_bytes, start, end);
        }
        _ => panic!("unexpected fixture request path"),
    }
}

/// Извлекает bounded Range header fixture request-а.
fn requested_range(request: &str) -> (usize, usize) {
    let value = request
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("range").then_some(value.trim())
        })
        .and_then(|value| value.strip_prefix("bytes="))
        .expect("Range header");
    let (start, end) = value.split_once('-').expect("bounded Range header");
    (
        start.parse().expect("Range start"),
        end.parse().expect("Range end"),
    )
}

/// Возвращает один relative redirect hop без body.
fn respond_redirect(stream: &mut TcpStream, status: u16, location: &str) {
    write!(
        stream,
        "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .expect("write redirect response");
}

/// Возвращает exact partial media range.
fn respond_partial_content(stream: &mut TcpStream, media_bytes: &[u8], start: usize, end: usize) {
    let selected_bytes = &media_bytes[start..=end];
    write!(
        stream,
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nConnection: close\r\n\r\n",
        selected_bytes.len(),
        media_bytes.len(),
    )
    .expect("write partial response headers");
    stream
        .write_all(selected_bytes)
        .expect("write partial response body");
}
