//! Functional evidence completed-resource cache через production streaming boundary.

use super::*;

/// Первый response висит до cancellation, второй отдаётся полностью.
struct CancelledThenCompleteServer {
    target: HttpRequestTarget,
    request_count: Arc<AtomicUsize>,
    first_disconnected: std::sync::mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    address: SocketAddr,
    worker: Option<thread::JoinHandle<()>>,
}

impl CancelledThenCompleteServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cancellable cache server");
        listener
            .set_nonblocking(true)
            .expect("set cancellable listener nonblocking");
        let address = listener.local_addr().expect("cancellable server address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/segment.ts"))
            .expect("cancellable target");
        let request_count = Arc::new(AtomicUsize::new(0));
        let worker_request_count = Arc::clone(&request_count);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (disconnect_sender, first_disconnected) = std::sync::mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("accept cancellable cache request: {error}"),
                };
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let _request = read_request(&mut stream);
                let request_index = worker_request_count.fetch_add(1, Ordering::AcqRel);
                if request_index == 0 {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write cancellable headers");
                    stream.flush().expect("flush cancellable headers");
                    stream
                        .set_read_timeout(Some(TEST_TIMEOUT))
                        .expect("set cancellation observation timeout");
                    let mut probe = [0_u8; 1];
                    match stream.read(&mut probe) {
                        Ok(0) => disconnect_sender
                            .send(())
                            .expect("publish cancellation disconnect"),
                        Ok(_) => panic!("HTTP response client не должен писать body socket"),
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::BrokenPipe
                            ) =>
                        {
                            disconnect_sender
                                .send(())
                                .expect("publish cancellation reset")
                        }
                        Err(error) => panic!("cancelled response не был закрыт: {error}"),
                    }
                    continue;
                }

                stream
                    .write_all(&response("200 OK", &[], b"complete"))
                    .expect("write completed retry response");
            }
        });
        Self {
            target,
            request_count,
            first_disconnected,
            stop,
            address,
            worker: Some(worker),
        }
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::Acquire)
    }
}

impl Drop for CancelledThenCompleteServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join cancellable cache server");
        }
    }
}

/// Полностью drain-ит streaming resource, включая validated EOF/cache admission.
fn drain_streaming_resource(
    resource: &mut AdaptiveStreamingResource,
) -> Result<Vec<bytes::Bytes>, AdaptiveTransportError> {
    let mut chunks = Vec::new();
    while let Some(chunk) = resource.next_chunk()? {
        chunks.push(chunk);
    }
    Ok(chunks)
}

/// Объединяет transport chunks только внутри test assertion-а.
fn joined_chunks(chunks: &[bytes::Bytes]) -> Vec<u8> {
    chunks
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect()
}

/// Validated EOF публикует exact chunks/Range metadata и второй open не идёт в сеть.
#[test]
fn completed_vod_stream_replays_identical_range_chunks_without_second_request() {
    let server = LocalServer::start(|_, request| {
        assert!(
            request
                .headers
                .to_ascii_lowercase()
                .contains("range: bytes=2-5")
        );
        response(
            "206 Partial Content",
            &[("Content-Range", "bytes 2-5/8".to_owned())],
            b"cdef",
        )
    });
    let target = server.target("/segment.ts");
    let context = context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let request = AdaptiveResourceFetchRequest::range(
        SourceGeneration::new(1),
        target,
        source_core::HttpBoundedByteRange::new(2, NonZeroUsize::new(4).expect("range length"))
            .expect("valid exact range"),
        NonZeroUsize::new(16).expect("streaming body bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    );

    let mut first = context
        .open_resource_streaming_blocking(
            request.clone(),
            media_core::DemuxSeekCancellationToken::new(),
        )
        .expect("open first range response");
    let first_resource_id = first.resource_correlation_id();
    assert!(
        first.network_request_attempt_id().is_some(),
        "network open обязан иметь physical request id"
    );
    let first_chunks = drain_streaming_resource(&mut first).expect("drain first range response");
    let first_final_target = first.final_target().clone();
    let first_range_metadata = first.range_metadata().cloned();

    let replay_controller = AdaptiveRestartableReadInterruption::new();
    let replay_attempt = replay_controller
        .new_attempt()
        .expect("allocate cache replay attempt");
    let mut replay = context
        .open_resource_streaming_blocking_with_restartable_read_attempt(
            request,
            media_core::DemuxSeekCancellationToken::new(),
            replay_attempt.clone(),
        )
        .expect("open cached range replay");
    assert_eq!(
        replay_attempt.arm_as_current(),
        AdaptiveRestartableReadArmOutcome::Armed
    );
    assert_eq!(
        replay_controller.request_active_read_interruption(),
        AdaptiveRestartableReadInterruptionRequest::AlreadyQuiescent,
        "completed replay не имеет physical body future и не должен быть poisoned"
    );
    assert_ne!(first_resource_id, replay.resource_correlation_id());
    assert_eq!(
        replay.network_request_attempt_id(),
        None,
        "cache replay не должен выдумывать network request id"
    );
    let replay_diagnostics = format!("{replay:?}");
    assert!(!replay_diagnostics.contains("segment.ts"));
    assert!(!replay_diagnostics.contains("token"));
    let replay_chunks = drain_streaming_resource(&mut replay).expect("drain cached range replay");

    assert_eq!(server.request_count(), 1);
    assert_eq!(first_chunks, replay_chunks);
    assert_eq!(joined_chunks(&replay_chunks), b"cdef");
    assert_eq!(replay.final_target(), &first_final_target);
    assert_eq!(replay.range_metadata(), first_range_metadata.as_ref());
    assert_eq!(replay.received_body_bytes(), 4);
}

/// Initialization использует тот же completed-only cache contract, но отдельный purpose key.
#[test]
fn completed_vod_initialization_replays_without_second_request() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"initialization"));
    let target = server.target("/init.mp4");
    let context = context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let request = AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target,
        NonZeroUsize::new(64).expect("initialization bound"),
        AdaptiveResourcePurpose::Initialization,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    );

    for open_index in 0..2 {
        let mut resource = context
            .open_resource_streaming_blocking(
                request.clone(),
                media_core::DemuxSeekCancellationToken::new(),
            )
            .unwrap_or_else(|error| panic!("open initialization #{open_index}: {error}"));
        let chunks = drain_streaming_resource(&mut resource)
            .unwrap_or_else(|error| panic!("drain initialization #{open_index}: {error}"));
        assert_eq!(joined_chunks(&chunks), b"initialization");
    }

    assert_eq!(server.request_count(), 1);
}

/// Truncated response не попадает в completed cache и следующий open повторяет HTTP request.
#[test]
fn truncated_stream_is_not_cached_and_next_open_refetches() {
    let server = LocalServer::start(|index, _| {
        if index == 0 {
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nbad".to_vec()
        } else {
            response("200 OK", &[], b"complete")
        }
    });
    let target = server.target("/segment.ts");
    let context = context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let request = AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target,
        NonZeroUsize::new(16).expect("streaming body bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    );
    let mut truncated = context
        .open_resource_streaming_blocking(
            request.clone(),
            media_core::DemuxSeekCancellationToken::new(),
        )
        .expect("open truncated response");
    let truncation_error = loop {
        match truncated.next_chunk() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("truncated response не должен завершиться validated EOF"),
            Err(error) => break error,
        }
    };
    assert!(matches!(
        truncation_error,
        AdaptiveTransportError::Source(_)
    ));

    let mut refetched = context
        .open_resource_streaming_blocking(request, media_core::DemuxSeekCancellationToken::new())
        .expect("refetch after truncated response");
    let refetched_chunks =
        drain_streaming_resource(&mut refetched).expect("drain refetched response");

    assert_eq!(joined_chunks(&refetched_chunks), b"complete");
    assert_eq!(server.request_count(), 2);
}

/// Уже отменённый cursor закрывает response и не публикует неполный cache entry.
#[test]
fn cancelled_stream_is_not_cached_and_next_open_refetches() {
    let server = CancelledThenCompleteServer::start();
    let context = context(
        &server.target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let request = AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        server.target.clone(),
        NonZeroUsize::new(16).expect("streaming body bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    );
    let seek_cancellation = media_core::DemuxSeekCancellationToken::new();
    let mut cancelled = context
        .open_resource_streaming_blocking(request.clone(), seek_cancellation.clone())
        .expect("open cancellable response");
    assert!(
        context
            .lock_completed_resource_cache()
            .pending_charge_bytes()
            > 0,
        "network open обязан зарезервировать место до validated EOF"
    );

    // Program order намеренно доказывает pre-cancelled public read, не полагаясь на scheduler.
    seek_cancellation.cancel();
    let cancelled_result = cancelled.next_chunk();
    assert!(matches!(
        cancelled_result,
        Err(AdaptiveTransportError::Cancelled)
    ));
    assert_eq!(cancelled.received_body_bytes(), 0);
    assert_eq!(
        context
            .lock_completed_resource_cache()
            .pending_charge_bytes(),
        0,
        "cancellation обязана освободить pending cache reservation"
    );
    assert_eq!(
        context
            .lock_completed_resource_cache()
            .accounted_charge_bytes(),
        0,
        "pre-cancelled response не должен оставить completed cache entry"
    );
    server
        .first_disconnected
        .recv_timeout(TEST_TIMEOUT)
        .expect("cancelled response socket must close");
    let mut refetched = context
        .open_resource_streaming_blocking(request, media_core::DemuxSeekCancellationToken::new())
        .expect("refetch after cancellation");
    let refetched_chunks =
        drain_streaming_resource(&mut refetched).expect("drain response after cancellation");

    assert_eq!(joined_chunks(&refetched_chunks), b"complete");
    assert_eq!(server.request_count(), 2);
}

/// Drop непрочитанного network cursor-а освобождает initial metadata reservation через RAII.
#[test]
fn dropped_unread_stream_releases_pending_cache_reservation() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"unread"));
    let target = server.target("/unread-segment.ts");
    let context = context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    let request = AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target,
        NonZeroUsize::new(16).expect("streaming body bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    );
    let resource = context
        .open_resource_streaming_blocking(request, media_core::DemuxSeekCancellationToken::new())
        .expect("open unread response");
    assert!(
        context
            .lock_completed_resource_cache()
            .pending_charge_bytes()
            > 0
    );

    drop(resource);

    assert_eq!(
        context
            .lock_completed_resource_cache()
            .pending_charge_bytes(),
        0
    );
}

/// Live resources не сохраняются даже после полного EOF одного response-а.
#[test]
fn completed_live_stream_is_never_cached() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"live"));
    let target = server.target("/live-segment.ts");
    let context = context_with_presentation(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
        MediaPresentation::Live,
    );
    let request = AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target,
        NonZeroUsize::new(16).expect("streaming body bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    );

    for open_index in 0..2 {
        let mut resource = context
            .open_resource_streaming_blocking(
                request.clone(),
                media_core::DemuxSeekCancellationToken::new(),
            )
            .unwrap_or_else(|error| panic!("open live response #{open_index}: {error}"));
        let chunks = drain_streaming_resource(&mut resource)
            .unwrap_or_else(|error| panic!("drain live response #{open_index}: {error}"));
        assert_eq!(joined_chunks(&chunks), b"live");
    }

    assert_eq!(server.request_count(), 2);
}
