//! Integration tests transport-owned policy на позднем HTTP Range redirect-е.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use source_core::CancellationToken;
use web_media_transport_api::MediaComponentRole;

use super::tests::{registry, request_with_serialized_cookies};

/// Authorization value, который target CDN никогда не должен увидеть.
const AUTHORIZATION_SECRET: &str = "Bearer late-range-redirect-secret";
/// Initial Cookie value, ограниченный base origin-ом.
const INITIAL_COOKIE_SECRET: &str = "initial=range-cookie-secret";
/// Set-Cookie value, полученный base origin-ом во время probe.
const REFRESHED_COOKIE_SECRET: &str = "refreshed=range-cookie-secret";

/// Роль одного hermetic HTTP fixture server-а.
#[derive(Clone)]
enum FixtureRole {
    /// Probe возвращает 206, а следующий Range — cross-origin 302.
    RedirectingBase { target_url: String },
    /// Возвращает exact final 206 media bytes.
    RangeTarget,
}

/// Локальный server с test-owned request capture.
struct RangeRedirectServer {
    /// Loopback listener address.
    address: SocketAddr,
    /// Raw requests доступны только assertions.
    requests: Arc<Mutex<Vec<String>>>,
    /// Cooperative accept-loop stop.
    stop: Arc<AtomicBool>,
    /// Join handle предотвращает test thread leak.
    join_handle: Option<JoinHandle<()>>,
}

impl RangeRedirectServer {
    /// Запускает fixture с immutable role и media bytes.
    fn spawn(media_bytes: Vec<u8>, role: FixtureRole) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind range redirect server");
        listener
            .set_nonblocking(true)
            .expect("set range redirect server nonblocking");
        let address = listener.local_addr().expect("read fixture address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::Builder::new()
            .name("web-media-http-range-redirect-test".to_owned())
            .spawn(move || {
                while !worker_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request = read_request(&mut stream);
                            worker_requests
                                .lock()
                                .expect("fixture request capture")
                                .push(request.clone());
                            respond(&mut stream, &media_bytes, &role, &request);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => return,
                    }
                }
            })
            .expect("spawn range redirect fixture");
        Self {
            address,
            requests,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// Строит operational fixture URL без secret query.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    /// Проверяет, попало ли sensitive test value в request capture.
    fn captured_value(&self, value: &str) -> bool {
        self.requests
            .lock()
            .expect("fixture request capture")
            .iter()
            .any(|request| request.contains(value))
    }

    /// Возвращает число физических requests fixture-а.
    fn request_count(&self) -> usize {
        self.requests.lock().expect("fixture request capture").len()
    }
}

impl Drop for RangeRedirectServer {
    /// Останавливает accept loop и присоединяет worker при любом исходе теста.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// Поздний cross-origin redirect проходит через prefetch и снимает все secrets.
#[test]
fn cross_origin_range_redirect_prefetches_without_forwarding_secrets() {
    let media_bytes = b"late-range-redirect-media".to_vec();
    let target_server = RangeRedirectServer::spawn(media_bytes.clone(), FixtureRole::RangeTarget);
    let base_server = RangeRedirectServer::spawn(
        media_bytes.clone(),
        FixtureRole::RedirectingBase {
            target_url: target_server.url("/final.mp4"),
        },
    );
    let (registry, provider) = registry();
    let opened = registry
        .open(request_with_serialized_cookies(
            provider,
            &base_server.url("/base.mp4"),
            MediaComponentRole::Muxed,
            1,
            Some(AUTHORIZATION_SECRET),
            Some(INITIAL_COOKIE_SECRET),
            CancellationToken::new(),
        ))
        .expect("late redirect must remain a seekable open");
    let mut source = opened
        .into_input()
        .into_seekable()
        .expect("redirected input remains seekable");
    let mut output = vec![0_u8; media_bytes.len()];
    let bytes_read = source
        .read(&mut output, &CancellationToken::new())
        .expect("read prefetched redirected bytes");

    assert_eq!(&output[..bytes_read], media_bytes.as_slice());
    assert!(base_server.request_count() >= 2, "probe + prefetch Range");
    assert!(target_server.request_count() >= 1, "redirected 206 Range");
    assert!(base_server.captured_value(AUTHORIZATION_SECRET));
    assert!(base_server.captured_value("initial=range-cookie-secret"));
    assert!(base_server.captured_value("refreshed=range-cookie-secret"));
    assert!(!target_server.captured_value(AUTHORIZATION_SECRET));
    assert!(!target_server.captured_value("initial=range-cookie-secret"));
    assert!(!target_server.captured_value("refreshed=range-cookie-secret"));
}

/// Читает complete GET headers одного fixture request-а.
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set fixture read timeout");
    let mut request_bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let bytes_read = stream.read(&mut chunk).expect("read fixture request");
        if bytes_read == 0 {
            break;
        }
        request_bytes.extend_from_slice(&chunk[..bytes_read]);
    }
    String::from_utf8(request_bytes).expect("fixture request is UTF-8")
}

/// Возвращает role-specific 206 либо поздний redirect.
fn respond(stream: &mut TcpStream, media_bytes: &[u8], role: &FixtureRole, request: &str) {
    match role {
        FixtureRole::RedirectingBase { target_url } if requested_range(request) == (0, 0) => {
            respond_partial_content(stream, media_bytes, 0, 0, true);
        }
        FixtureRole::RedirectingBase { target_url } => {
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write late redirect");
        }
        FixtureRole::RangeTarget => {
            let (start, end) = requested_range(request);
            respond_partial_content(stream, media_bytes, start, end, false);
        }
    }
}

/// Извлекает exact bounded Range request-а.
fn requested_range(request: &str) -> (usize, usize) {
    let value = request
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("range").then_some(value.trim())
        })
        .and_then(|value| value.strip_prefix("bytes="))
        .expect("fixture Range header");
    let (start, end) = value.split_once('-').expect("bounded fixture Range");
    (
        start.parse().expect("fixture Range start"),
        end.parse().expect("fixture Range end"),
    )
}

/// Возвращает exact partial response и optional scoped Set-Cookie.
fn respond_partial_content(
    stream: &mut TcpStream,
    media_bytes: &[u8],
    start: usize,
    end: usize,
    set_cookie: bool,
) {
    let selected_bytes = &media_bytes[start..=end];
    let set_cookie_header = set_cookie
        .then_some(format!("Set-Cookie: {REFRESHED_COOKIE_SECRET}; Path=/\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\n{set_cookie_header}Connection: close\r\n\r\n",
        selected_bytes.len(),
        media_bytes.len(),
    )
    .expect("write fixture partial headers");
    stream
        .write_all(selected_bytes)
        .expect("write fixture partial body");
}
