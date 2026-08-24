//! Focused contract tests abortable async bounded HTTP boundary-а.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rustiplayer_config::NetworkConfig;
use tokio::runtime::Builder;

use crate::{
    CancellationToken, HttpBoundedByteRange, HttpBoundedFetchHop, HttpBoundedFetchKind,
    HttpBoundedFetchRequest, HttpRequestTarget, HttpSourceSession, SourceError,
    SourceRuntimeConfig,
};

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

/// Создаёт session с production default network policy.
fn source_session() -> HttpSourceSession {
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
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
