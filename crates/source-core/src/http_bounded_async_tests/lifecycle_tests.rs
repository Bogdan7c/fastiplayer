//! Сквозные ошибки и accounting трёх bounded HTTP frontends.

use super::*;

/// Loopback fixture принимает request, но не публикует response headers.
struct StalledResponseServer {
    /// Exact public target для production source boundary.
    target: HttpRequestTarget,
    /// Подтверждает физическое принятие request-а до проверки timeout outcome.
    request_received: Receiver<()>,
    /// Разрешает bounded cleanup удерживаемого connection-а.
    release: Option<SyncSender<()>>,
    /// Останавливает never-accepted path без ожидания production timeout-а.
    stop: Arc<AtomicBool>,
    /// Bounded worker thread fixture-а.
    worker: Option<thread::JoinHandle<()>>,
}

impl StalledResponseServer {
    /// Запускает server, который удерживает connection после чтения request headers.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stalled HTTP fixture");
        listener
            .set_nonblocking(true)
            .expect("set stalled HTTP fixture nonblocking");
        let address = listener.local_addr().expect("stalled HTTP fixture address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/resource"))
            .expect("stalled HTTP target");
        let (request_received_sender, request_received) = sync_channel(1);
        let (release, wait_for_release) = sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + FIXTURE_TIMEOUT;
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        if worker_stop.load(Ordering::SeqCst) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                    Err(error) => panic!("accept stalled HTTP request: {error}"),
                }
            };
            read_request_headers(&mut stream);
            let _ = request_received_sender.send(());
            let _ = wait_for_release.recv_timeout(FIXTURE_TIMEOUT);
        });
        Self {
            target,
            request_received,
            release: Some(release),
            stop,
            worker: Some(worker),
        }
    }

    /// Доказывает, что timeout произошёл после network side effect-а.
    fn wait_until_request_received(&self) {
        self.request_received
            .recv_timeout(FIXTURE_TIMEOUT)
            .expect("stalled HTTP request must be received");
    }
}

impl Drop for StalledResponseServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join stalled HTTP fixture");
        }
    }
}

/// Формирует response с caller-owned Content-Length для truncated-body сценариев.
fn response_with_declared_length(
    status: &str,
    headers: &[(&str, &str)],
    declared_length: usize,
    body: &[u8],
) -> Vec<u8> {
    let mut wire =
        format!("HTTP/1.1 {status}\r\nContent-Length: {declared_length}\r\nConnection: close\r\n")
            .into_bytes();
    for (name, value) in headers {
        wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(body);
    wire
}

/// Формирует close-delimited body без Content-Length для проверки stream accounting.
fn response_until_close(status: &str, body: &[u8]) -> Vec<u8> {
    let mut wire = format!("HTTP/1.1 {status}\r\nConnection: close\r\n\r\n").into_bytes();
    wire.extend_from_slice(body);
    wire
}

/// Создаёт exact Range intent с единым media-purpose accounting.
fn range_request(target: HttpRequestTarget, start: u64, length: usize) -> HttpBoundedFetchRequest {
    let byte_range = HttpBoundedByteRange::new(
        start,
        NonZeroUsize::new(length).expect("nonzero range length"),
    )
    .expect("valid test byte range");
    HttpBoundedFetchRequest::range(target, Vec::new(), byte_range, HttpBoundedFetchKind::Media)
}

/// Все HTTP frontends сохраняют transport close до headers как request failure.
#[test]
fn bounded_frontends_preserve_request_transport_failure() {
    let session = source_session();
    let cancellation = CancellationToken::new();

    let blocking_server = OneShotServer::start(Vec::new());
    let blocking_error = session
        .fetch_bounded_single_hop(
            full_request(blocking_server.target.clone(), 16),
            &cancellation,
        )
        .expect_err("blocking transport close must fail");
    assert!(matches!(blocking_error, SourceError::HttpRequest { .. }));
    assert_eq!(blocking_server.accepted_requests(), 1);

    let abortable_server = OneShotServer::start(Vec::new());
    let abortable_error = run_fetch(
        &session,
        full_request(abortable_server.target.clone(), 16),
        &cancellation,
    )
    .expect_err("abortable transport close must fail");
    assert!(matches!(abortable_error, SourceError::HttpRequest { .. }));
    assert_eq!(abortable_server.accepted_requests(), 1);

    let streaming_server = OneShotServer::start(Vec::new());
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let streaming_error = runtime
        .block_on(session.open_bounded_single_hop_stream(
            full_request(streaming_server.target.clone(), 16),
            &cancellation,
        ))
        .expect_err("streaming transport close must fail");
    assert!(matches!(streaming_error, SourceError::HttpRequest { .. }));
    assert_eq!(streaming_server.accepted_requests(), 1);
}

/// Async frontends сохраняют typed header timeout вместо generic request failure.
#[test]
fn async_frontends_preserve_header_timeout() {
    let abortable_server = StalledResponseServer::start();
    let session = source_session_with_read_timeout(Duration::from_millis(100));
    let cancellation = CancellationToken::new();
    let abortable_error = run_fetch(
        &session,
        full_request(abortable_server.target.clone(), 16),
        &cancellation,
    )
    .expect_err("abortable response header stall must time out");
    assert!(matches!(abortable_error, SourceError::HttpTimeout { .. }));
    abortable_server.wait_until_request_received();

    let streaming_server = StalledResponseServer::start();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let streaming_error = runtime
        .block_on(session.open_bounded_single_hop_stream(
            full_request(streaming_server.target.clone(), 16),
            &cancellation,
        ))
        .expect_err("streaming response header stall must time out");
    assert!(matches!(streaming_error, SourceError::HttpTimeout { .. }));
    streaming_server.wait_until_request_received();
}

/// Buffered frontends не сливают wire truncation и exact Range EOF в одну ошибку.
#[test]
fn buffered_frontends_keep_body_read_and_range_eof_distinct() {
    let session = source_session();
    let cancellation = CancellationToken::new();

    let blocking_body_server =
        OneShotServer::start(response_with_declared_length("200 OK", &[], 8, b"head"));
    let blocking_body_error = session
        .fetch_bounded_single_hop(
            full_request(blocking_body_server.target.clone(), 16),
            &cancellation,
        )
        .expect_err("blocking truncated HTTP body must fail");
    assert!(matches!(
        blocking_body_error,
        SourceError::HttpBodyRead { .. }
    ));

    let abortable_body_server =
        OneShotServer::start(response_with_declared_length("200 OK", &[], 8, b"head"));
    let abortable_body_error = run_fetch(
        &session,
        full_request(abortable_body_server.target.clone(), 16),
        &cancellation,
    )
    .expect_err("abortable truncated HTTP body must fail");
    assert!(matches!(
        abortable_body_error,
        SourceError::HttpBodyRead { .. }
    ));

    let blocking_range_server = OneShotServer::start(response(
        "206 Partial Content",
        &[("Content-Range", "bytes 4-6/10")],
        b"45",
    ));
    let blocking_range_error = session
        .fetch_bounded_single_hop(
            range_request(blocking_range_server.target.clone(), 4, 3),
            &cancellation,
        )
        .expect_err("blocking short exact Range must fail");
    assert!(matches!(
        blocking_range_error,
        SourceError::UnexpectedEof {
            offset: 4,
            expected_bytes: 3,
            actual_bytes: 2,
        }
    ));

    let abortable_range_server = OneShotServer::start(response(
        "206 Partial Content",
        &[("Content-Range", "bytes 4-6/10")],
        b"45",
    ));
    let abortable_range_error = run_fetch(
        &session,
        range_request(abortable_range_server.target.clone(), 4, 3),
        &cancellation,
    )
    .expect_err("abortable short exact Range must fail");
    assert!(matches!(
        abortable_range_error,
        SourceError::UnexpectedEof {
            offset: 4,
            expected_bytes: 3,
            actual_bytes: 2,
        }
    ));
}

/// Abortable buffered fetch сохраняет общий deadline во время body stall-а.
#[test]
fn abortable_fetch_times_out_while_body_is_stalled() {
    let server = GatedBodyServer::start();
    let session = source_session_with_read_timeout(Duration::from_millis(100));
    let error = run_fetch(
        &session,
        full_request(server.target.clone(), 8),
        &CancellationToken::new(),
    )
    .expect_err("abortable stalled body must time out");
    assert!(matches!(error, SourceError::HttpTimeout { .. }));
    server.wait_until_prefix_ready();
}

/// Streaming Range публикует metadata, но отвергает EOF раньше exact length.
#[test]
fn streaming_body_rejects_short_exact_range() {
    let server = OneShotServer::start(response(
        "206 Partial Content",
        &[("Content-Range", "bytes 4-6/10")],
        b"45",
    ));
    let session = source_session();
    let cancellation = CancellationToken::new();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let opened = runtime
        .block_on(session.open_bounded_single_hop_stream(
            range_request(server.target.clone(), 4, 3),
            &cancellation,
        ))
        .expect("open short streaming Range");
    let HttpBoundedStreamingFetchHop::Body(mut body) = opened else {
        panic!("short Range fixture must return a body");
    };
    assert_eq!(
        body.range_metadata()
            .expect("validated Range metadata")
            .total_resource_bytes(),
        Some(10)
    );
    let prefix = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect("read short Range prefix")
        .expect("short Range prefix before EOF");
    assert_eq!(prefix, b"45".as_slice());

    let error = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect_err("short exact Range EOF must fail");
    assert!(matches!(
        error,
        SourceError::UnexpectedEof {
            offset: 4,
            expected_bytes: 3,
            actual_bytes: 2,
        }
    ));
}

/// Отсутствие Content-Length не позволяет обойти caller-owned body bound.
#[test]
fn streaming_body_rejects_undeclared_overflow() {
    let server = OneShotServer::start(response_until_close("200 OK", b"oversized"));
    let session = source_session();
    let cancellation = CancellationToken::new();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let opened =
        runtime
            .block_on(session.open_bounded_single_hop_stream(
                full_request(server.target.clone(), 4),
                &cancellation,
            ))
            .expect("open close-delimited streaming body");
    let HttpBoundedStreamingFetchHop::Body(mut body) = opened else {
        panic!("close-delimited fixture must return a body");
    };

    let error = runtime
        .block_on(body.next_chunk(&cancellation))
        .expect_err("stream accounting must reject overflow");
    assert!(matches!(
        error,
        SourceError::HttpBodyTooLarge {
            maximum_bytes: 4,
            ..
        }
    ));
    assert_eq!(body.received_body_bytes(), 0);
}

/// Streaming open отвергает объявленный overflow и возвращает redirect без body read-а.
#[test]
fn streaming_open_preserves_declared_bound_and_redirect() {
    let oversized_server = OneShotServer::start(response("200 OK", &[], b"oversized"));
    let session = source_session();
    let cancellation = CancellationToken::new();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test Tokio runtime");
    let overflow_error = runtime
        .block_on(session.open_bounded_single_hop_stream(
            full_request(oversized_server.target.clone(), 4),
            &cancellation,
        ))
        .expect_err("declared oversized stream must fail at open");
    assert!(matches!(
        overflow_error,
        SourceError::HttpBodyTooLarge {
            maximum_bytes: 4,
            ..
        }
    ));

    let redirect_server =
        OneShotServer::start(response("302 Found", &[("Location", "/next")], b"ignored"));
    let redirect = runtime
        .block_on(session.open_bounded_single_hop_stream(
            full_request(redirect_server.target.clone(), 16),
            &cancellation,
        ))
        .expect("streaming redirect must be returned");
    let initial_target = redirect_server.target.expose_secret_for_request();
    let resolved_target = format!("{}/next", initial_target.trim_end_matches("/resource"));
    let redirect_debug = format!("{redirect:?}");
    assert!(redirect_debug.contains("Redirect"));
    assert!(!redirect_debug.contains(initial_target));
    assert!(!redirect_debug.contains(&resolved_target));
    let HttpBoundedStreamingFetchHop::Redirect(redirect) = redirect else {
        panic!("302 fixture must not publish a body");
    };
    let expected_target =
        HttpRequestTarget::parse_exact(resolved_target).expect("expected redirect target");
    assert_eq!(redirect.target(), &expected_target);
}

/// Abortable response policy сохраняет HTTP status вместо generic request failure.
#[test]
fn abortable_fetch_preserves_http_status_error() {
    let server = OneShotServer::start(response(
        "503 Service Unavailable",
        &[("Retry-After", "1")],
        b"unavailable",
    ));
    let error = run_fetch(
        &source_session(),
        full_request(server.target.clone(), 32),
        &CancellationToken::new(),
    )
    .expect_err("503 must remain typed response policy failure");
    assert!(matches!(
        error,
        SourceError::HttpStatus { status, .. } if status.as_u16() == 503
    ));
}
