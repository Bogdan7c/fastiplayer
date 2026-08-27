//! Focused evidence для provider-neutral blocking resource boundary.

use super::*;

/// HTTP fixture публикует headers и ждёт физического закрытия body reader-а.
struct StalledBodyServer {
    /// Exact target для production adaptive boundary.
    target: HttpRequestTarget,
    /// Server замечает drop response socket-а после seek cancellation.
    disconnected: std::sync::mpsc::Receiver<()>,
    /// Exact physical request count исключает скрытый reopen/prefetch.
    request_count: Arc<AtomicUsize>,
    /// Bounded fixture thread.
    worker: Option<thread::JoinHandle<()>>,
}

impl StalledBodyServer {
    /// Открывает one-shot listener с заявленным, но никогда не отправленным body.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stalled body server");
        let address = listener.local_addr().expect("stalled body server address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/segment.ts"))
            .expect("stalled body target");
        let (disconnect_sender, disconnected) = std::sync::mpsc::sync_channel(1);
        let request_count = Arc::new(AtomicUsize::new(0));
        let worker_request_count = Arc::clone(&request_count);
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept stalled body request");
            worker_request_count.fetch_add(1, Ordering::SeqCst);
            let _request = read_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n")
                .expect("write stalled body headers");
            stream.flush().expect("flush stalled body headers");
            stream
                .set_read_timeout(Some(TEST_TIMEOUT))
                .expect("set stalled body disconnect timeout");
            let mut byte = [0_u8; 1];
            match stream.read(&mut byte) {
                Ok(0) => disconnect_sender
                    .send(())
                    .expect("publish stalled body disconnect"),
                Ok(_) => panic!("client не должен писать в HTTP response socket"),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    disconnect_sender
                        .send(())
                        .expect("publish reset stalled body disconnect")
                }
                Err(error) => panic!("stalled body socket не был закрыт: {error}"),
            }
        });
        Self {
            target,
            disconnected,
            request_count,
            worker: Some(worker),
        }
    }
}

impl Drop for StalledBodyServer {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join stalled body server");
        }
    }
}

fn fetch(
    context: &AdaptiveHttpContext,
    target: HttpRequestTarget,
    purpose: AdaptiveResourcePurpose,
    query_application: AdaptiveResourceQueryApplication,
) -> Result<AdaptiveFetchedResource, AdaptiveTransportError> {
    context.fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target,
        NonZeroUsize::new(64).expect("resource bound"),
        purpose,
        query_application,
    ))
}

#[test]
fn merge_replaces_duplicate_keys_and_reapplies_on_same_origin_redirect() {
    let server = LocalServer::start(|index, _| match index {
        0 => response(
            "302 Found",
            &[(
                "Location",
                "/final?existing=redirect&duplicate=stale".to_owned(),
            )],
            b"",
        ),
        _ => response("200 OK", &[], b"resource"),
    });
    let initial_target = server.target("/segment?existing=initial&duplicate=old&duplicate=older");
    let context = context(
        &initial_target,
        CancellationToken::new(),
        same_origin_redirects(),
        Some("Bearer scoped"),
        Some("duplicate=new-one&duplicate=new-two&added=1"),
    );
    let fetched = fetch(
        &context,
        initial_target,
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::MergeScopedAddition,
    )
    .expect("redirected merged fetch");
    assert_eq!(fetched.bytes(), b"resource");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(request.request_line.contains("duplicate=new-one"));
        assert!(request.request_line.contains("duplicate=new-two"));
        assert!(request.request_line.contains("added=1"));
        assert!(!request.request_line.contains("duplicate=old"));
        assert!(!request.request_line.contains("duplicate=stale"));
    }
    assert!(requests[0].request_line.contains("existing=initial"));
    assert!(requests[1].request_line.contains("existing=redirect"));
}

#[test]
fn merge_and_headers_are_stripped_monotonically_on_cross_origin_redirect() {
    let final_server = LocalServer::start(|_, _| response("200 OK", &[], b"resource"));
    let final_target = final_server.target("/final?public=1");
    let location = final_target.expose_secret_for_request().to_owned();
    let initial_server = LocalServer::start(move |_, _| {
        response("302 Found", &[("Location", location.clone())], b"")
    });
    let initial_target = initial_server.target("/segment");
    let context = context(
        &initial_target,
        CancellationToken::new(),
        RedirectPolicy::cross_origin_without_secrets(
            RedirectHopLimit::new(4).expect("redirect limit"),
        ),
        Some("Bearer scoped-secret"),
        Some("token=query-secret"),
    );
    fetch(
        &context,
        initial_target,
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::MergeScopedAddition,
    )
    .expect("cross-origin fetch");
    let initial = initial_server.requests();
    assert!(initial[0].headers.contains("scoped-secret"));
    assert!(initial[0].request_line.contains("query-secret"));
    let final_requests = final_server.requests();
    assert!(!final_requests[0].headers.contains("scoped-secret"));
    assert!(!final_requests[0].request_line.contains("query-secret"));
    assert!(final_requests[0].request_line.contains("public=1"));
}

#[test]
fn projected_key_fallback_merges_while_exact_aes_uri_bypasses_query() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"0123456789abcdef"));
    let manifest_target = server.target("/master.m3u8");
    let context = context_with_queries(
        &manifest_target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        Some("fallback=segment"),
        Some("fallback=segment"),
        MediaPresentation::Vod,
    );
    fetch(
        &context,
        server.target("/manifest-key?kept=1"),
        AdaptiveResourcePurpose::EncryptionKey,
        AdaptiveResourceQueryApplication::MergeScopedAddition,
    )
    .expect("manifest key merge");
    fetch(
        &context,
        server.target("/aes-replacement?exact=1"),
        AdaptiveResourcePurpose::EncryptionKey,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    )
    .expect("exact AES replacement");
    let requests = server.requests();
    assert!(requests[0].request_line.contains("kept=1"));
    assert!(requests[0].request_line.contains("fallback=segment"));
    assert!(requests[1].request_line.contains("exact=1"));
    assert!(!requests[1].request_line.contains("fallback=segment"));
}

#[test]
fn stale_generation_is_rejected_before_network_side_effect() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"resource"));
    let manifest_target = server.target("/master.m3u8");
    let context = context(
        &manifest_target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let stale = context
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            SourceGeneration::new(2),
            server.target("/stale"),
            NonZeroUsize::new(64).expect("resource bound"),
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        ))
        .expect_err("stale generation");
    assert!(matches!(
        stale,
        AdaptiveTransportError::StaleGeneration { .. }
    ));
    assert_eq!(server.request_count(), 0);
}

#[test]
fn context_derives_forwarding_intent_without_exposing_or_widening_scope() {
    let scoped_server = LocalServer::start(|_, _| response("200 OK", &[], b"scoped"));
    let foreign_server = LocalServer::start(|_, _| response("200 OK", &[], b"foreign"));
    let scoped_target = scoped_server.target("/manifest");
    let context = context(
        &scoped_target,
        CancellationToken::new(),
        same_origin_redirects(),
        Some("Bearer intent-secret"),
        Some("token=intent-secret"),
    );

    assert_eq!(
        context.resource_secret_forwarding_for(&scoped_server.target("/segment")),
        AdaptiveResourceSecretForwarding::ForwardScoped
    );
    assert_eq!(
        context.resource_secret_forwarding_for(&foreign_server.target("/segment")),
        AdaptiveResourceSecretForwarding::Suppress
    );
    fetch(
        &context,
        scoped_server.target("/segment"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    )
    .expect("scoped server lifecycle");
    context
        .fetch_resource_blocking(
            AdaptiveResourceFetchRequest::full(
                SourceGeneration::new(1),
                foreign_server.target("/segment"),
                NonZeroUsize::new(64).expect("resource bound"),
                AdaptiveResourcePurpose::MediaSegment,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            )
            .with_secret_forwarding(AdaptiveResourceSecretForwarding::Suppress),
        )
        .expect("foreign server lifecycle");
}

#[test]
fn explicit_suppress_fetches_out_of_scope_and_retries_without_any_secret_material() {
    let scoped_server = LocalServer::start(|_, _| response("200 OK", &[], b"manifest"));
    let foreign_server = LocalServer::start(|index, _| match index {
        0 => response("500 Internal Server Error", &[], b"retry"),
        _ => response("200 OK", &[], b"resource"),
    });
    let scoped_target = scoped_server.target("/manifest");
    let context = context(
        &scoped_target,
        CancellationToken::new(),
        same_origin_redirects(),
        Some("Bearer suppress-secret"),
        Some("token=suppress-query"),
    );
    let foreign_target = foreign_server.target("/segment?public=1");
    let request = AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        foreign_target,
        NonZeroUsize::new(64).expect("resource bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::MergeScopedAddition,
    )
    .with_secret_forwarding(AdaptiveResourceSecretForwarding::Suppress);
    let diagnostics = format!("{request:?}");

    let fetched = context
        .fetch_resource_blocking(request)
        .expect("suppressed out-of-scope retry");

    assert_eq!(fetched.bytes(), b"resource");
    let requests = foreign_server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| {
        !request.headers.contains("suppress-secret")
            && !request.request_line.contains("suppress-query")
            && request.request_line.contains("public=1")
    }));
    assert!(!diagnostics.contains("suppress-secret"));
    assert!(!diagnostics.contains("suppress-query"));
    assert!(!diagnostics.contains("/segment"));
}

#[test]
fn clock_request_is_manifest_bounded_and_cannot_inherit_source_secrets() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"2026-08-10T10:00:00Z\n"));
    let manifest_target = server.target("/manifest.mpd");
    let context = context(
        &manifest_target,
        CancellationToken::new(),
        same_origin_redirects(),
        Some("Bearer clock-must-not-see-this"),
        Some("source_secret=must-not-leak"),
    );
    let clock_bound = context.maximum_resource_bytes(AdaptiveResourcePurpose::ClockSynchronization);
    assert_eq!(
        clock_bound,
        context.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest)
    );

    context
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::clock_synchronization(
            SourceGeneration::new(1),
            server.target("/clock?public=1"),
            clock_bound,
        ))
        .expect("same-origin clock fetch без source secrets");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].request_line.contains("public=1"));
    assert!(!requests[0].request_line.contains("source_secret"));
    assert!(!requests[0].headers.contains("clock-must-not-see-this"));

    let weakened = AdaptiveResourceFetchRequest::clock_synchronization(
        SourceGeneration::new(1),
        server.target("/clock"),
        clock_bound,
    )
    .with_secret_forwarding(AdaptiveResourceSecretForwarding::ForwardScoped);
    assert!(matches!(
        context.fetch_resource_blocking(weakened),
        Err(AdaptiveTransportError::InvalidResourcePolicy {
            purpose: AdaptiveResourcePurpose::ClockSynchronization
        })
    ));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn derived_forwarding_intent_fetches_out_of_scope_cdn_without_secrets() {
    let scoped_server = LocalServer::start(|_, _| response("200 OK", &[], b"manifest"));
    let foreign_server = LocalServer::start(|_, _| response("200 OK", &[], b"segment"));
    let scoped_target = scoped_server.target("/manifest.m3u8");
    let context = context(
        &scoped_target,
        CancellationToken::new(),
        same_origin_redirects(),
        Some("Bearer cdn-secret"),
        Some("token=cdn-secret"),
    );
    let foreign_target = foreign_server.target("/segment1.ts");

    let rejected = context
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            SourceGeneration::new(1),
            foreign_target.clone(),
            NonZeroUsize::new(64).expect("resource bound"),
            AdaptiveResourcePurpose::MediaSegment,
            AdaptiveResourceQueryApplication::MergeScopedAddition,
        ))
        .expect_err("default ForwardScoped must reject out-of-scope CDN");
    assert!(matches!(
        rejected,
        AdaptiveTransportError::SecretScopeRejected
    ));
    assert_eq!(foreign_server.request_count(), 0);

    // Тот же intent, что теперь выставляет HLS через resource_secret_forwarding_for.
    let fetched = context
        .fetch_resource_blocking(
            AdaptiveResourceFetchRequest::full(
                SourceGeneration::new(1),
                foreign_target.clone(),
                NonZeroUsize::new(64).expect("resource bound"),
                AdaptiveResourcePurpose::MediaSegment,
                AdaptiveResourceQueryApplication::MergeScopedAddition,
            )
            .with_secret_forwarding(context.resource_secret_forwarding_for(&foreign_target)),
        )
        .expect("derived Suppress must fetch CDN without secrets");

    assert_eq!(fetched.bytes(), b"segment");
    let requests = foreign_server.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains("cdn-secret"));
    assert!(!requests[0].request_line.contains("cdn-secret"));
}

/// Supersede будит pending `Response::chunk()` и физически закрывает response socket.
#[test]
fn streaming_seek_cancellation_aborts_stalled_body_without_waiting_for_timeout() {
    let server = StalledBodyServer::start();
    let context = context(
        &server.target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let seek_cancellation = media_core::DemuxSeekCancellationToken::new();
    let mut resource = context
        .open_resource_streaming_blocking(
            AdaptiveResourceFetchRequest::full(
                SourceGeneration::new(1),
                server.target.clone(),
                NonZeroUsize::new(16).expect("streaming body bound"),
                AdaptiveResourcePurpose::MediaSegment,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            ),
            seek_cancellation.clone(),
        )
        .expect("open stalled streaming body");
    let (started_sender, started_receiver) = std::sync::mpsc::sync_channel(1);
    let (result_sender, result_receiver) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        started_sender.send(()).expect("publish body read start");
        result_sender
            .send(resource.next_chunk())
            .expect("publish cancelled body result");
    });
    started_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("body read must start");

    seek_cancellation.cancel();

    let result = result_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("cancelled body read must wake without HTTP timeout");
    assert!(matches!(result, Err(AdaptiveTransportError::Cancelled)));
    reader.join().expect("join cancelled body reader");
    server
        .disconnected
        .recv_timeout(TEST_TIMEOUT)
        .expect("dropping cancelled response must close transport socket");
    assert_eq!(server.request_count.load(Ordering::SeqCst), 1);
}

/// Active-read signal drop-ает тот же pending body, не отменяя request token и не делая reopen.
#[test]
fn restartable_active_read_interruption_aborts_stalled_body_without_extra_request() {
    let server = StalledBodyServer::start();
    let context = context(
        &server.target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let request_cancellation = media_core::DemuxSeekCancellationToken::new();
    let controller = AdaptiveRestartableReadInterruption::new();
    let attempt = controller
        .new_attempt()
        .expect("allocate exact body attempt");
    let mut resource = context
        .open_resource_streaming_blocking_with_restartable_read_attempt(
            AdaptiveResourceFetchRequest::full(
                SourceGeneration::new(1),
                server.target.clone(),
                NonZeroUsize::new(16).expect("streaming body bound"),
                AdaptiveResourcePurpose::MediaSegment,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            ),
            request_cancellation.clone(),
            attempt.clone(),
        )
        .expect("open stalled body with disarmed attempt");
    assert!(resource.network_request_attempt_id().is_some());
    assert_eq!(
        controller.request_active_read_interruption(),
        AdaptiveRestartableReadInterruptionRequest::AlreadyQuiescent,
        "offside attempt не должен быть poisoned до commit"
    );
    assert_eq!(
        attempt.arm_as_current(),
        AdaptiveRestartableReadArmOutcome::Armed
    );

    let (started_sender, started_receiver) = std::sync::mpsc::sync_channel(1);
    let (result_sender, result_receiver) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        started_sender
            .send(())
            .expect("publish active body read start");
        let first = resource.next_chunk();
        let second = resource.next_chunk();
        result_sender
            .send((first, second, resource.received_body_bytes()))
            .expect("publish restartable body outcomes");
    });
    started_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("active body read thread must start");

    let request_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match controller.request_active_read_interruption() {
            AdaptiveRestartableReadInterruptionRequest::InterruptionRequested => break,
            AdaptiveRestartableReadInterruptionRequest::AlreadyQuiescent => {
                assert!(
                    Instant::now() < request_deadline,
                    "body read должен занять active attempt slot"
                );
                thread::yield_now();
            }
            AdaptiveRestartableReadInterruptionRequest::InterruptionAlreadyRequested => {
                panic!("test посылает только один accepted active-read signal")
            }
        }
    }

    let (first, second, received_body_bytes) = result_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("restartable body read must wake without HTTP timeout");
    assert!(matches!(
        first,
        Err(AdaptiveTransportError::RestartableReadInterrupted)
    ));
    assert!(matches!(
        second,
        Err(AdaptiveTransportError::RestartableReadInterrupted)
    ));
    assert_eq!(
        received_body_bytes, 0,
        "stalled body не должен буферизоваться"
    );
    assert!(
        !request_cancellation.is_cancelled(),
        "active-read signal не должен отменять replacement request token"
    );
    reader.join().expect("join restartable body reader");
    server
        .disconnected
        .recv_timeout(TEST_TIMEOUT)
        .expect("dropping interrupted response must close transport socket");
    assert_eq!(server.request_count.load(Ordering::SeqCst), 1);
}
