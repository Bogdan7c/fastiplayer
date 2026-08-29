//! Focused contract tests abortable async bounded HTTP boundary-а.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use rustiplayer_config::NetworkConfig;
use tokio::runtime::Builder;

use crate::{
    CancellationToken, HttpBoundedByteRange, HttpBoundedFetchHop, HttpBoundedFetchKind,
    HttpBoundedFetchRequest, HttpBoundedStreamingFetchHop, HttpRequestTarget, HttpSourceSession,
    SourceError, SourceRuntimeConfig,
};

mod lifecycle_tests;

/// Все fixture waits остаются существенно меньше production timeout policy.
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(2);

/// Одноразовый loopback HTTP server с наблюдаемым числом network side effects.
struct OneShotServer {
    /// Exact public target для production source boundary.
    target: HttpRequestTarget,
    /// Число реально принятых requests.
    accepted_requests: Arc<AtomicUsize>,
    /// Cooperative stop для negative no-network fixture-а.
    stop: Arc<AtomicBool>,
    /// Bounded worker thread fixture-а.
    worker: Option<thread::JoinHandle<()>>,
}

impl OneShotServer {
    /// Запускает fixture с заранее заданным wire response.
    fn start(response: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind async HTTP fixture");
        listener
            .set_nonblocking(true)
            .expect("set async HTTP fixture nonblocking");
        let address = listener.local_addr().expect("async HTTP fixture address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/resource"))
            .expect("async HTTP target");
        let accepted_requests = Arc::new(AtomicUsize::new(0));
        let worker_request_count = Arc::clone(&accepted_requests);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + FIXTURE_TIMEOUT;
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        worker_request_count.fetch_add(1, Ordering::SeqCst);
                        read_request_headers(&mut stream);
                        stream
                            .write_all(&response)
                            .expect("write async HTTP response");
                        stream.flush().expect("flush async HTTP response");
                        return;
                    }
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        if worker_stop.load(Ordering::SeqCst) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                    Err(error) => panic!("accept async HTTP request: {error}"),
                }
            }
        });
        Self {
            target,
            accepted_requests,
            stop,
            worker: Some(worker),
        }
    }

    /// Возвращает точное число принятых requests.
    fn accepted_requests(&self) -> usize {
        self.accepted_requests.load(Ordering::SeqCst)
    }
}

impl Drop for OneShotServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join async HTTP fixture");
        }
    }
}

/// Loopback fixture удерживает хвост body, чтобы отделить первый chunk от EOF.
struct GatedBodyServer {
    /// Exact public target для production source boundary.
    target: HttpRequestTarget,
    /// Разрешает fixture дописать хвост response body.
    tail_release: Option<SyncSender<()>>,
    /// Подтверждает, что headers и prefix уже физически отправлены.
    prefix_ready: Receiver<()>,
    /// Bounded worker thread fixture-а.
    worker: Option<thread::JoinHandle<()>>,
}

impl GatedBodyServer {
    /// Запускает response с четырёхбайтовым prefix и удерживаемым tail.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind gated HTTP fixture");
        let address = listener.local_addr().expect("gated HTTP fixture address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/resource"))
            .expect("gated HTTP target");
        let (tail_release, wait_for_tail_release) = sync_channel(1);
        let (prefix_ready_sender, prefix_ready) = sync_channel(1);
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept gated HTTP request");
            read_request_headers(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nhead")
                .expect("write gated HTTP prefix");
            stream.flush().expect("flush gated HTTP prefix");
            prefix_ready_sender
                .send(())
                .expect("publish gated HTTP prefix readiness");
            if wait_for_tail_release.recv_timeout(FIXTURE_TIMEOUT).is_ok() {
                stream.write_all(b"tail").expect("write gated HTTP tail");
                stream.flush().expect("flush gated HTTP tail");
            }
        });
        Self {
            target,
            tail_release: Some(tail_release),
            prefix_ready,
            worker: Some(worker),
        }
    }

    /// Ждёт физической отправки prefix до чтения transport body.
    fn wait_until_prefix_ready(&self) {
        self.prefix_ready
            .recv_timeout(FIXTURE_TIMEOUT)
            .expect("gated HTTP prefix must become ready");
    }

    /// Разрешает server дописать оставшуюся половину body.
    fn release_tail(&mut self) {
        self.tail_release
            .take()
            .expect("gated HTTP tail released once")
            .send(())
            .expect("release gated HTTP tail");
    }
}

impl Drop for GatedBodyServer {
    fn drop(&mut self) {
        if let Some(tail_release) = self.tail_release.take() {
            let _ = tail_release.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join gated HTTP fixture");
        }
    }
}

/// Читает request headers до отправки fixture response.
fn read_request_headers(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(FIXTURE_TIMEOUT))
        .expect("set async request timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 512];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read_bytes = stream.read(&mut chunk).expect("read async HTTP request");
        assert_ne!(read_bytes, 0, "request ended before headers");
        request.extend_from_slice(&chunk[..read_bytes]);
    }
}

/// Формирует один close-delimited-by-length HTTP response.
fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut wire = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    )
    .into_bytes();
    for (name, value) in headers {
        wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(body);
    wire
}

/// Создаёт одинаковый full-resource intent для разных HTTP frontends.
fn full_request(target: HttpRequestTarget, maximum_body_bytes: usize) -> HttpBoundedFetchRequest {
    HttpBoundedFetchRequest::full(
        target,
        Vec::new(),
        NonZeroUsize::new(maximum_body_bytes).expect("nonzero body bound"),
        HttpBoundedFetchKind::Metadata,
    )
}

/// Создаёт session с production default network policy.
fn source_session() -> HttpSourceSession {
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    HttpSourceSession::new(&source_config).expect("HTTP source session")
}

/// Создаёт session с коротким read timeout для детерминированных timeout tests.
fn source_session_with_read_timeout(read_timeout: Duration) -> HttpSourceSession {
    let network_config = NetworkConfig {
        read_timeout_ms: u64::try_from(read_timeout.as_millis()).expect("test timeout fits u64"),
        ..NetworkConfig::default()
    };
    let source_config =
        SourceRuntimeConfig::from_network_config(&network_config).expect("source config");
    HttpSourceSession::new(&source_config).expect("HTTP source session")
}

/// Выполняет async boundary на выделенном current-thread runtime-е теста.
fn run_fetch(
    session: &HttpSourceSession,
    request: HttpBoundedFetchRequest,
    cancellation: &CancellationToken,
) -> Result<HttpBoundedFetchHop, SourceError> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime")
        .block_on(session.fetch_bounded_single_hop_abortable(request, cancellation))
}

/// Active full-resource stub проходит тот же bounded response contract.
#[test]
fn async_full_hop_returns_exact_body() {
    let server = OneShotServer::start(response("200 OK", &[], b"manifest"));
    let request = HttpBoundedFetchRequest::full(
        server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(16).expect("body bound"),
        HttpBoundedFetchKind::Metadata,
    );

    let outcome =
        run_fetch(&source_session(), request, &CancellationToken::new()).expect("async full fetch");
    let HttpBoundedFetchHop::Complete(response) = outcome else {
        panic!("full response must not become redirect");
    };
    assert_eq!(response.into_bytes(), b"manifest");
    assert_eq!(server.accepted_requests(), 1);
}

/// Exact Range сохраняет bytes, total length и validator metadata.
#[test]
fn async_range_hop_preserves_validated_metadata() {
    let server = OneShotServer::start(response(
        "206 Partial Content",
        &[("Content-Range", "bytes 4-6/10"), ("ETag", "\"v1\"")],
        b"456",
    ));
    let byte_range = HttpBoundedByteRange::new(4, NonZeroUsize::new(3).expect("range length"))
        .expect("valid byte range");
    let request = HttpBoundedFetchRequest::range(
        server.target.clone(),
        Vec::new(),
        byte_range,
        HttpBoundedFetchKind::Media,
    );

    let outcome = run_fetch(&source_session(), request, &CancellationToken::new())
        .expect("async range fetch");
    let HttpBoundedFetchHop::Complete(response) = outcome else {
        panic!("range response must not become redirect");
    };
    assert_eq!(
        response
            .range_metadata()
            .expect("range metadata")
            .total_resource_bytes(),
        Some(10)
    );
    assert_eq!(response.into_bytes(), b"456");
}

/// Body overflow остаётся typed error и не публикует partial success.
#[test]
fn async_full_hop_enforces_caller_body_bound() {
    let server = OneShotServer::start(response("200 OK", &[], b"oversized"));
    let request = HttpBoundedFetchRequest::full(
        server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(4).expect("small body bound"),
        HttpBoundedFetchKind::Metadata,
    );

    let error = run_fetch(&source_session(), request, &CancellationToken::new())
        .expect_err("oversized async body must fail");
    assert!(matches!(
        error,
        SourceError::HttpBodyTooLarge {
            maximum_bytes: 4,
            ..
        }
    ));
}

/// Pre-cancelled caller не создаёт socket side effect и не меняет shared session.
#[test]
fn async_hop_rejects_pre_cancelled_request_before_network() {
    let server = OneShotServer::start(response("200 OK", &[], b"unused"));
    let session = source_session();
    let request = HttpBoundedFetchRequest::full(
        server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(16).expect("body bound"),
        HttpBoundedFetchKind::Metadata,
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = run_fetch(&session, request, &cancellation)
        .expect_err("pre-cancelled async request must fail");
    assert!(matches!(error, SourceError::Cancelled));

    let blocking_error = session
        .fetch_bounded_single_hop(full_request(server.target.clone(), 16), &cancellation)
        .expect_err("pre-cancelled blocking request must fail");
    assert!(matches!(blocking_error, SourceError::Cancelled));

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let streaming_error =
        runtime
            .block_on(session.open_bounded_single_hop_stream(
                full_request(server.target.clone(), 16),
                &cancellation,
            ))
            .expect_err("pre-cancelled streaming request must fail");
    assert!(matches!(streaming_error, SourceError::Cancelled));
    assert_eq!(server.accepted_requests(), 0);

    let recovery_server = OneShotServer::start(response("200 OK", &[], b"recovered"));
    let recovery_request = HttpBoundedFetchRequest::full(
        recovery_server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(16).expect("recovery body bound"),
        HttpBoundedFetchKind::Metadata,
    );
    let recovery = run_fetch(&session, recovery_request, &CancellationToken::new())
        .expect("new cancellation lifetime must reuse unchanged session");
    let HttpBoundedFetchHop::Complete(response) = recovery else {
        panic!("recovery response must not become redirect");
    };
    assert_eq!(response.into_bytes(), b"recovered");
}

/// Streaming boundary отдаёт уже пришедший prefix, пока server удерживает tail.
#[test]
fn streaming_hop_does_not_wait_for_complete_body() {
    let mut server = GatedBodyServer::start();
    let session = source_session();
    let cancellation = CancellationToken::new();
    let request = HttpBoundedFetchRequest::full(
        server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(8).expect("body bound"),
        HttpBoundedFetchKind::Media,
    );
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let opened = runtime
        .block_on(session.open_bounded_single_hop_stream(request, &cancellation))
        .expect("open streaming response");
    let opened_debug = format!("{opened:?}");
    assert!(opened_debug.contains("Body"));
    assert!(!opened_debug.contains(server.target.expose_secret_for_request()));
    let HttpBoundedStreamingFetchHop::Body(mut body) = opened else {
        panic!("streaming fixture must not redirect");
    };
    let request_attempt_id = body.request_attempt_id();
    let body_debug = format!("{body:?}");
    assert!(body_debug.contains("received_body_bytes: 0"));
    assert!(!body_debug.contains(server.target.expose_secret_for_request()));
    assert!(body.range_metadata().is_none());
    server.wait_until_prefix_ready();

    let prefix = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect("read streaming prefix")
        .expect("prefix before EOF");
    assert_eq!(prefix, b"head".as_slice());
    assert_eq!(body.received_body_bytes(), 4);

    server.release_tail();
    let tail = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect("read streaming tail")
        .expect("tail before EOF");
    assert_eq!(tail, b"tail".as_slice());
    assert!(
        runtime
            .block_on(body.next_chunk(&cancellation))
            .expect("read validated streaming EOF")
            .is_none()
    );
    assert!(
        runtime
            .block_on(body.next_chunk(&cancellation))
            .expect("repeat validated streaming EOF")
            .is_none()
    );
    assert_eq!(body.received_body_bytes(), 8);
    assert_eq!(body.request_attempt_id(), request_attempt_id);
}

/// Законная пауза consumer-а не превращается в reqwest total-request timeout.
#[test]
fn streaming_hop_timeout_excludes_unpolled_backpressure_pause() {
    let mut server = GatedBodyServer::start();
    let read_timeout = Duration::from_millis(100);
    let session = source_session_with_read_timeout(read_timeout);
    let cancellation = CancellationToken::new();
    let request = HttpBoundedFetchRequest::full(
        server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(8).expect("body bound"),
        HttpBoundedFetchKind::Media,
    );
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let opened = runtime
        .block_on(session.open_bounded_single_hop_stream(request, &cancellation))
        .expect("open streaming response");
    let HttpBoundedStreamingFetchHop::Body(mut body) = opened else {
        panic!("streaming fixture must not redirect");
    };
    server.wait_until_prefix_ready();
    let prefix = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect("read streaming prefix")
        .expect("prefix before EOF");
    assert_eq!(prefix, b"head".as_slice());

    thread::sleep(read_timeout + Duration::from_millis(50));
    server.release_tail();
    let tail = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect("backpressure pause must not expire idle response")
        .expect("tail before EOF");
    assert_eq!(tail, b"tail".as_slice());
}

/// Когда consumer активно ждёт следующий byte, configured read timeout сохраняется.
#[test]
fn streaming_hop_times_out_an_actively_polled_stalled_body() {
    let server = GatedBodyServer::start();
    let session = source_session_with_read_timeout(Duration::from_millis(100));
    let cancellation = CancellationToken::new();
    let request = HttpBoundedFetchRequest::full(
        server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(8).expect("body bound"),
        HttpBoundedFetchKind::Media,
    );
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let opened = runtime
        .block_on(session.open_bounded_single_hop_stream(request, &cancellation))
        .expect("open streaming response");
    let HttpBoundedStreamingFetchHop::Body(mut body) = opened else {
        panic!("streaming fixture must not redirect");
    };
    server.wait_until_prefix_ready();
    let prefix = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect("read streaming prefix")
        .expect("prefix before EOF");
    assert_eq!(prefix, b"head".as_slice());

    let error = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect_err("active stalled body must respect read timeout");
    assert!(matches!(error, SourceError::HttpTimeout { .. }));
}

/// Cancellation после open не публикует buffered partial body как success.
#[test]
fn streaming_hop_observes_cancellation_before_next_read() {
    let server = OneShotServer::start(response("200 OK", &[], b"unused"));
    let session = source_session();
    let cancellation = CancellationToken::new();
    let request = HttpBoundedFetchRequest::full(
        server.target.clone(),
        Vec::new(),
        NonZeroUsize::new(16).expect("body bound"),
        HttpBoundedFetchKind::Media,
    );
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let opened = runtime
        .block_on(session.open_bounded_single_hop_stream(request, &cancellation))
        .expect("open streaming response");
    let HttpBoundedStreamingFetchHop::Body(mut body) = opened else {
        panic!("streaming fixture must not redirect");
    };
    cancellation.cancel();

    let error = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect_err("cancelled streaming body must fail");
    assert!(matches!(error, SourceError::Cancelled));
}
