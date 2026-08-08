use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpHeader, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig,
    StreamingByteSource, ValidatedHttpHeaders,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_transport_api::{
    HttpRangeRequestLimit, MediaComponentIdentity, MediaComponentRole, MediaPresentation,
    RedirectHopLimit, RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportInput, TransportOpenError, TransportOpenRequest, TransportProvider,
    TransportRefreshError, TransportRefreshRequest, TransportRefreshRequestError,
    TransportRegistry,
};

use super::{WebMediaHttpProvider, prefetch_config_with_source_limit};

/// Поведение hermetic HTTP fixture server-а.
#[derive(Clone)]
enum TestServerBehavior {
    /// Любой request получает полный non-Range response.
    FullBody,
    /// Range request получает exact partial response.
    Range,
    /// Range response обновляет session cookie перед последующими reads.
    RangeWithSetCookie { serialized_cookie: String },
    /// Любой request требует authentication.
    Unauthorized,
    /// `/start` перенаправляет на caller-provided target, остальные path получают body.
    Redirect { target: String },
    /// Redirect пытается расширить cookie на другой origin.
    RedirectWithSetCookie {
        target: String,
        serialized_cookie: String,
    },
}

/// Локальный server хранит только test-owned request capture.
struct TestServer {
    /// Loopback listener address.
    address: SocketAddr,
    /// Число принятых HTTP requests.
    request_count: Arc<AtomicUsize>,
    /// Raw requests доступны только assertions и никогда не форматируются production error-ами.
    requests: Arc<Mutex<Vec<String>>>,
    /// Cooperative accept-loop stop.
    stop: Arc<AtomicBool>,
    /// Join handle гарантирует отсутствие test thread leak-а.
    join_handle: Option<JoinHandle<()>>,
}

impl TestServer {
    /// Запускает nonblocking loopback server с immutable body/behavior.
    fn spawn(body: Vec<u8>, behavior: TestServerBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let address = listener.local_addr().expect("read test address");
        let behavior = Arc::new(behavior);
        let request_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_behavior = Arc::clone(&behavior);
        let worker_request_count = Arc::clone(&request_count);
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::Builder::new()
            .name("web-media-http-test-server".to_owned())
            .spawn(move || {
                while !worker_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => handle_connection(
                            &mut stream,
                            &body,
                            worker_behavior.as_ref(),
                            &worker_request_count,
                            &worker_requests,
                        ),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => return,
                    }
                }
            })
            .expect("spawn test server");
        Self {
            address,
            request_count,
            requests,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// Собирает operational URL без секретов.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    /// Возвращает exact request count для duplicate-fetch assertions.
    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    /// Проверяет, попадал ли header value в какой-либо request fixture-а.
    fn captured_value(&self, needle: &str) -> bool {
        self.requests
            .lock()
            .expect("request capture mutex")
            .iter()
            .any(|request| request.contains(needle))
    }

    /// Клонирует bounded fixture requests для assertions по порядку hops.
    fn captured_requests(&self) -> Vec<String> {
        self.requests.lock().expect("request capture mutex").clone()
    }
}

impl Drop for TestServer {
    /// Останавливает accept loop и присоединяет test thread.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// Читает request headers и пишет один bounded response.
fn handle_connection(
    stream: &mut TcpStream,
    body: &[u8],
    behavior: &TestServerBehavior,
    request_count: &AtomicUsize,
    requests: &Mutex<Vec<String>>,
) {
    let request = read_request(stream);
    request_count.fetch_add(1, Ordering::SeqCst);
    requests
        .lock()
        .expect("request capture mutex")
        .push(request.clone());

    match behavior {
        TestServerBehavior::FullBody => respond_full(stream, body),
        TestServerBehavior::Range => respond_range(stream, body, &request),
        TestServerBehavior::RangeWithSetCookie { serialized_cookie } => {
            respond_range_with_set_cookie(stream, body, &request, serialized_cookie);
        }
        TestServerBehavior::Unauthorized => {
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .expect("write unauthorized response");
        }
        TestServerBehavior::Redirect { target } if request.starts_with("GET /start ") => {
            let response =
                format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n");
            stream
                .write_all(response.as_bytes())
                .expect("write redirect response");
        }
        TestServerBehavior::Redirect { .. } => respond_full(stream, body),
        TestServerBehavior::RedirectWithSetCookie {
            target,
            serialized_cookie,
        } if request.starts_with("GET /start ") => {
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nSet-Cookie: {serialized_cookie}\r\nContent-Length: 0\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .expect("write redirect with Set-Cookie response");
        }
        TestServerBehavior::RedirectWithSetCookie { .. } => respond_full(stream, body),
    }
}

/// Читает HTTP/1 headers до bounded terminator-а.
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set request timeout");
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while bytes.len() < 16 * 1024 {
        let read = stream.read(&mut chunk).expect("read request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Возвращает полный body и намеренно игнорирует Range.
fn respond_full(stream: &mut TcpStream, body: &[u8]) {
    let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
    stream
        .write_all(headers.as_bytes())
        .expect("write full headers");
    stream.write_all(body).expect("write full body");
}

/// Возвращает exact requested byte range.
fn respond_range(stream: &mut TcpStream, body: &[u8], request: &str) {
    respond_range_with_optional_cookie(stream, body, request, None);
}

/// Возвращает range и сохраняет scoped Set-Cookie в per-source session.
fn respond_range_with_set_cookie(
    stream: &mut TcpStream,
    body: &[u8],
    request: &str,
    serialized_cookie: &str,
) {
    respond_range_with_optional_cookie(stream, body, request, Some(serialized_cookie));
}

/// Общий exact range response builder с optional Set-Cookie fixture header.
fn respond_range_with_optional_cookie(
    stream: &mut TcpStream,
    body: &[u8],
    request: &str,
    serialized_cookie: Option<&str>,
) {
    let range = request
        .lines()
        .find_map(|line| line.strip_prefix("range: bytes="))
        .or_else(|| {
            request
                .lines()
                .find_map(|line| line.strip_prefix("Range: bytes="))
        })
        .expect("provider sends Range header");
    let (start, end) = range.split_once('-').expect("bounded byte range");
    let start = start.parse::<usize>().expect("range start");
    let end = end.parse::<usize>().expect("range end");
    let end = end.min(body.len().saturating_sub(1));
    let selected = &body[start..=end];
    let set_cookie_header = serialized_cookie
        .map(|cookie| format!("Set-Cookie: {cookie}\r\n"))
        .unwrap_or_default();
    let headers = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\n{set_cookie_header}\r\n",
        selected.len(),
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("write range headers");
    stream.write_all(selected).expect("write range body");
}

/// Создаёт concrete provider registry с production dependency shape.
pub(super) fn registry() -> (
    TransportRegistry,
    web_media_transport_api::TransportProviderId,
) {
    let source_config = SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
        .expect("default network config");
    let provider =
        WebMediaHttpProvider::new(source_config, media_prefetch::PrefetchConfig::default())
            .expect("provider descriptor");
    let provider_id = provider.descriptor().provider_id().clone();
    let mut registry = TransportRegistry::new();
    registry
        .register(Box::new(provider))
        .expect("register concrete provider");
    (registry, provider_id)
}

/// Строит exact request для одной component role/generation.
pub(super) fn request(
    provider: web_media_transport_api::TransportProviderId,
    url: &str,
    role: MediaComponentRole,
    source_generation: u64,
    authorization: Option<&str>,
    cancellation: CancellationToken,
) -> TransportOpenRequest {
    request_with_serialized_cookies(
        provider,
        url,
        role,
        source_generation,
        authorization,
        None,
        cancellation,
    )
}

/// Строит request с раздельными Authorization и serialized Cookie boundaries.
pub(super) fn request_with_serialized_cookies(
    provider: web_media_transport_api::TransportProviderId,
    url: &str,
    role: MediaComponentRole,
    source_generation: u64,
    authorization: Option<&str>,
    serialized_cookies: Option<&str>,
    cancellation: CancellationToken,
) -> TransportOpenRequest {
    let target = HttpRequestTarget::parse_exact(url).expect("valid test target");
    let source = SourceIdentity::new(7);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(11),
        CandidateFormatIdentity::new(match role {
            MediaComponentRole::Muxed => "muxed",
            MediaComponentRole::ContentProbed => "content-probed",
            MediaComponentRole::Video => "video",
            MediaComponentRole::Audio => "audio",
            MediaComponentRole::Subtitle => "subtitle",
        })
        .expect("format identity"),
    );
    let semantic = SemanticIdentity::new(
        source,
        match role {
            MediaComponentRole::Muxed => "muxed",
            MediaComponentRole::ContentProbed => "content-probed",
            MediaComponentRole::Video => "video",
            MediaComponentRole::Audio => "audio",
            MediaComponentRole::Subtitle => "subtitle",
        },
    )
    .expect("semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, role).expect("component identity");
    let scope =
        SecretRequestScope::from_target(&target, HttpPathScope::new("/").expect("root scope"));
    let headers = authorization
        .map(|value| vec![HttpHeader::new("authorization", value)])
        .unwrap_or_default();
    let mut secret_builder = SecretRequestContext::builder(scope)
        .with_headers(ValidatedHttpHeaders::new(headers).expect("validated headers"));
    if let Some(serialized_cookies) = serialized_cookies {
        secret_builder = secret_builder
            .with_serialized_cookies(serialized_cookies)
            .expect("validated serialized cookies");
    }
    let secrets = secret_builder.build();
    TransportOpenRequest::new(
        provider,
        component,
        target,
        MediaPresentation::Vod,
        SourceGeneration::new(source_generation),
        secrets,
        RedirectPolicy::cross_origin_without_secrets(
            RedirectHopLimit::new(4).expect("test redirect limit"),
        ),
        cancellation,
    )
    .expect("transport request")
}

/// Читает forward-only transport до EOF.
fn read_streaming_body(mut source: Box<dyn StreamingByteSource>) -> Vec<u8> {
    let cancellation = CancellationToken::new();
    let mut output = Vec::new();
    let mut chunk = [0_u8; 16];
    loop {
        let read = source
            .read(&mut chunk, &cancellation)
            .expect("streaming read");
        if read == 0 {
            break;
        }
        output.extend_from_slice(&chunk[..read]);
    }
    output
}

/// Возвращает запрошенное число bytes из одного bounded Range header-а.
fn requested_http_range_bytes(request: &str) -> Option<u64> {
    let range_header = request
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))?;
    let (_name, raw_value) = range_header.split_once(':')?;
    let byte_range = raw_value.trim().strip_prefix("bytes=")?;
    let (start, end) = byte_range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    end.checked_sub(start)?.checked_add(1)
}

#[test]
fn non_range_reuses_probe_response_without_duplicate_request() {
    let body = b"progressive-body".to_vec();
    let server = TestServer::spawn(body.clone(), TestServerBehavior::FullBody);
    let (registry, provider) = registry();
    let opened = registry
        .open(request(
            provider,
            &server.url("/media.webm"),
            MediaComponentRole::Muxed,
            1,
            None,
            CancellationToken::new(),
        ))
        .expect("open non-range source");
    let source = opened
        .into_input()
        .into_streaming()
        .expect("streaming input");

    assert_eq!(read_streaming_body(source), body);
    assert_eq!(server.request_count(), 1);
}

#[test]
fn range_source_uses_existing_prefetch_path() {
    let body = b"seekable-range-body".to_vec();
    let server = TestServer::spawn(body.clone(), TestServerBehavior::Range);
    let (registry, provider) = registry();
    let opened = registry
        .open(request(
            provider,
            &server.url("/media.mp4"),
            MediaComponentRole::Muxed,
            1,
            None,
            CancellationToken::new(),
        ))
        .expect("open range source");
    let mut source = opened.into_input().into_seekable().expect("seekable input");
    let mut output = vec![0_u8; body.len()];
    let read = source
        .read(&mut output, &CancellationToken::new())
        .expect("prefetched range read");

    assert_eq!(&output[..read], body.as_slice());
    assert!(server.request_count() >= 2);
}

#[test]
fn source_range_limit_caps_initial_and_follow_up_prefetch_requests() {
    const RANGE_LIMIT_BYTES: u64 = 64 * 1024;
    let body = vec![0x5a; (RANGE_LIMIT_BYTES * 3) as usize];
    let server = TestServer::spawn(body, TestServerBehavior::Range);
    let (registry, provider) = registry();
    let range_limit =
        HttpRangeRequestLimit::new(RANGE_LIMIT_BYTES).expect("positive source range limit");
    let open_request = request(
        provider,
        &server.url("/limited.mp4"),
        MediaComponentRole::Muxed,
        1,
        None,
        CancellationToken::new(),
    )
    .with_http_range_request_limit(range_limit);
    let opened = registry
        .open(open_request)
        .expect("range-limited source opens");
    let mut source = opened.into_input().into_seekable().expect("seekable input");
    let mut output = vec![0_u8; RANGE_LIMIT_BYTES as usize];
    source
        .read(&mut output, &CancellationToken::new())
        .expect("limited prefetch read");

    let requested_ranges = server
        .captured_requests()
        .iter()
        .filter_map(|request| requested_http_range_bytes(request))
        .collect::<Vec<_>>();
    assert!(
        requested_ranges.iter().any(|bytes| *bytes > 1),
        "fixture должен увидеть хотя бы один prefetch range после probe"
    );
    assert!(
        requested_ranges
            .iter()
            .all(|bytes| *bytes <= RANGE_LIMIT_BYTES),
        "ни один HTTP Range не должен превышать source-specific limit"
    );
}

#[test]
fn source_range_limit_preserves_global_memory_window() {
    let default_config = media_prefetch::PrefetchConfig::default();
    let range_limit = HttpRangeRequestLimit::new(1024 * 1024).expect("strict source limit");
    let effective_config = prefetch_config_with_source_limit(default_config, Some(range_limit))
        .expect("typed limit сохраняет prefetch invariants");

    assert_eq!(
        effective_config.initial_chunk_bytes(),
        default_config.initial_chunk_bytes()
    );
    assert_eq!(effective_config.chunk_bytes(), 1024 * 1024);
    assert_eq!(
        effective_config.window_bytes(),
        default_config.window_bytes()
    );

    let youtube_limit = HttpRangeRequestLimit::new(10 * 1024 * 1024).expect("YouTube limit");
    let youtube_config = prefetch_config_with_source_limit(default_config, Some(youtube_limit))
        .expect("мягкий source limit сохраняет global policy");
    assert_eq!(youtube_config, default_config);
}

#[test]
fn set_cookie_refreshes_subsequent_range_requests_inside_source_scope() {
    let body = b"cookie-refresh-range-body".to_vec();
    let server = TestServer::spawn(
        body.clone(),
        TestServerBehavior::RangeWithSetCookie {
            serialized_cookie: "session=refreshed-secret; Path=/".to_owned(),
        },
    );
    let (registry, provider) = registry();
    let opened = registry
        .open(request_with_serialized_cookies(
            provider,
            &server.url("/media.mp4"),
            MediaComponentRole::Muxed,
            1,
            None,
            Some("session=initial-secret"),
            CancellationToken::new(),
        ))
        .expect("open cookie-protected range source");
    let mut source = opened.into_input().into_seekable().expect("seekable input");
    let mut output = vec![0_u8; body.len()];
    source
        .read(&mut output, &CancellationToken::new())
        .expect("range read after Set-Cookie");

    let requests = server.captured_requests();
    assert!(
        requests
            .first()
            .is_some_and(|request| request.contains("initial-secret"))
    );
    assert!(
        requests
            .iter()
            .skip(1)
            .any(|request| request.contains("refreshed-secret"))
    );
    assert!(
        requests
            .iter()
            .skip(1)
            .all(|request| !request.contains("initial-secret"))
    );
}

#[test]
fn cookie_jar_is_isolated_between_source_opens() {
    let server = TestServer::spawn(b"isolated".to_vec(), TestServerBehavior::FullBody);
    let (registry, provider) = registry();
    registry
        .open(request_with_serialized_cookies(
            provider.clone(),
            &server.url("/first.webm"),
            MediaComponentRole::Muxed,
            1,
            None,
            Some("session=first-source-secret"),
            CancellationToken::new(),
        ))
        .expect("first protected source opens");
    registry
        .open(request(
            provider,
            &server.url("/second.webm"),
            MediaComponentRole::Muxed,
            2,
            None,
            CancellationToken::new(),
        ))
        .expect("second public source opens");

    let requests = server.captured_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("first-source-secret"));
    assert!(!requests[1].contains("first-source-secret"));
}

#[test]
fn muxed_separate_video_only_and_audio_only_share_one_provider_contract() {
    let body = b"component".to_vec();
    let server = TestServer::spawn(body, TestServerBehavior::FullBody);
    let (registry, provider) = registry();

    for (index, role) in [
        MediaComponentRole::Muxed,
        MediaComponentRole::Video,
        MediaComponentRole::Audio,
    ]
    .into_iter()
    .enumerate()
    {
        let opened = registry
            .open(request(
                provider.clone(),
                &server.url("/component.m4a"),
                role,
                (index + 1) as u64,
                None,
                CancellationToken::new(),
            ))
            .expect("component opens");
        assert!(matches!(opened.into_input(), TransportInput::Streaming(_)));
    }
    assert_eq!(server.request_count(), 3);
}

#[test]
fn cross_origin_redirect_never_forwards_authorization_header() {
    let target_server = TestServer::spawn(b"redirected".to_vec(), TestServerBehavior::FullBody);
    let redirect_server = TestServer::spawn(
        Vec::new(),
        TestServerBehavior::Redirect {
            target: target_server.url("/final.webm"),
        },
    );
    let (registry, provider) = registry();
    let secret = "Bearer super-secret-auth-value";
    registry
        .open(request(
            provider,
            &redirect_server.url("/start"),
            MediaComponentRole::Muxed,
            1,
            Some(secret),
            CancellationToken::new(),
        ))
        .expect("cross-origin redirect opens without secrets");

    assert!(redirect_server.captured_value(secret));
    assert!(!target_server.captured_value(secret));
}

#[test]
fn cross_origin_redirect_never_forwards_initial_or_set_cookie_state() {
    let target_server = TestServer::spawn(b"redirected".to_vec(), TestServerBehavior::FullBody);
    let redirect_server = TestServer::spawn(
        Vec::new(),
        TestServerBehavior::RedirectWithSetCookie {
            target: target_server.url("/final.webm"),
            serialized_cookie: "session=redirect-secret; Domain=127.0.0.1; Path=/".to_owned(),
        },
    );
    let (registry, provider) = registry();
    registry
        .open(request_with_serialized_cookies(
            provider,
            &redirect_server.url("/start"),
            MediaComponentRole::Muxed,
            1,
            None,
            Some("session=initial-secret"),
            CancellationToken::new(),
        ))
        .expect("cross-origin redirect strips cookie state");

    assert!(redirect_server.captured_value("initial-secret"));
    assert!(!target_server.captured_value("initial-secret"));
    assert!(!target_server.captured_value("redirect-secret"));
}

#[test]
fn same_origin_redirect_without_secrets_is_not_blocked_by_empty_scope() {
    let server = TestServer::spawn(
        b"same-origin".to_vec(),
        TestServerBehavior::Redirect {
            target: "/final.webm".to_owned(),
        },
    );
    let (registry, provider) = registry();
    let opened = registry
        .open(request(
            provider,
            &server.url("/start"),
            MediaComponentRole::Muxed,
            1,
            None,
            CancellationToken::new(),
        ))
        .expect("same-origin redirect without secrets");
    let source = opened
        .into_input()
        .into_streaming()
        .expect("redirected response remains streaming");

    assert_eq!(read_streaming_body(source), b"same-origin");
    assert_eq!(server.request_count(), 2);
}

#[test]
fn auth_errors_and_debug_output_redact_header_value_and_url_payload() {
    let server = TestServer::spawn(Vec::new(), TestServerBehavior::Unauthorized);
    let (registry, provider) = registry();
    let secret = "Bearer never-print-this";
    let secret_url = format!(
        "{}?token=never-print-query",
        server.url("/private/video.mp4")
    );
    let open_request = request(
        provider,
        &secret_url,
        MediaComponentRole::Muxed,
        1,
        Some(secret),
        CancellationToken::new(),
    );
    let request_debug = format!("{open_request:?}");
    let error = registry
        .open(open_request)
        .expect_err("server rejects auth");
    let error_debug = format!("{error:?}");

    assert!(!request_debug.contains(secret));
    assert!(!request_debug.contains("never-print-query"));
    assert!(!error_debug.contains(secret));
    assert!(!error_debug.contains("never-print-query"));
}

#[test]
fn cancellation_and_stale_refresh_reject_before_network_mutation() {
    let server = TestServer::spawn(b"refresh".to_vec(), TestServerBehavior::FullBody);
    let (registry, provider) = registry();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let cancellation_error = registry
        .open(request(
            provider.clone(),
            &server.url("/cancelled.webm"),
            MediaComponentRole::Muxed,
            1,
            None,
            cancelled,
        ))
        .expect_err("cancelled request rejected");
    assert!(matches!(cancellation_error, TransportOpenError::Cancelled));
    assert_eq!(server.request_count(), 0);

    let first = registry
        .open(request_with_serialized_cookies(
            provider.clone(),
            &server.url("/refresh.webm"),
            MediaComponentRole::Muxed,
            1,
            None,
            Some("session=initial-refresh-secret"),
            CancellationToken::new(),
        ))
        .expect("initial open");
    let replacement = request(
        provider.clone(),
        &server.url("/refresh.webm"),
        MediaComponentRole::Muxed,
        2,
        None,
        CancellationToken::new(),
    );
    let refresh = TransportRefreshRequest::new(first.identity().clone(), replacement)
        .expect("exact refresh identity");
    let stale_error = registry
        .refresh_if_current(refresh, SourceGeneration::new(99))
        .expect_err("stale generation rejected");
    assert!(matches!(
        stale_error,
        TransportRefreshError::StaleSourceGeneration { .. }
    ));

    let active_replacement = request_with_serialized_cookies(
        provider.clone(),
        &server.url("/refresh.webm"),
        MediaComponentRole::Muxed,
        2,
        None,
        Some("session=reextracted-refresh-secret"),
        CancellationToken::new(),
    );
    let active_refresh = TransportRefreshRequest::new(first.identity().clone(), active_replacement)
        .expect("active refresh identity");
    registry
        .refresh_if_current(active_refresh, SourceGeneration::new(1))
        .expect("active refresh opens replacement generation");
    let refresh_requests = server.captured_requests();
    assert_eq!(refresh_requests.len(), 2);
    assert!(refresh_requests[0].contains("initial-refresh-secret"));
    assert!(refresh_requests[1].contains("reextracted-refresh-secret"));
    assert!(!refresh_requests[1].contains("initial-refresh-secret"));

    let mismatched_replacement = request(
        provider,
        &server.url("/refresh.webm"),
        MediaComponentRole::Video,
        2,
        None,
        CancellationToken::new(),
    );
    let mismatch_error =
        TransportRefreshRequest::new(first.identity().clone(), mismatched_replacement)
            .expect_err("semantic mismatch rejected before provider");
    assert_eq!(
        mismatch_error,
        TransportRefreshRequestError::SemanticIdentityChanged
    );
    assert_eq!(server.request_count(), 2);
}
