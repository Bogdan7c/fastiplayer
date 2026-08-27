// Previewed → Receipted cancellation на реальном loopback streaming transport.

use super::*;

/// Реальный preview seek обязан закрыть partial response до того, как тот задержит final receipt.
#[test]
fn final_receipt_physically_cancels_partial_preview_body_without_stale_publication() {
    let segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|segment_index| {
                long_muxed_ts_segment(
                    segment_index
                        .saturating_mul(SEGMENT_SECONDS)
                        .saturating_mul(90_000),
                    SEGMENT_SECONDS,
                )
            })
            .collect::<Vec<_>>(),
    );
    let preview_partial = Arc::new(long_muxed_ts_segment_without_rap(90_000_000, 2));
    let target_attempts = Arc::new(AtomicUsize::new(0));
    let (preview_prefix_sender, preview_prefix_receiver) = mpsc::sync_channel(1);
    let (preview_disconnected_sender, preview_disconnected_receiver) = mpsc::sync_channel(1);
    let (final_started_sender, final_started_receiver) = mpsc::sync_channel(1);
    let server_segments = Arc::clone(&segments);
    let server_preview_partial = Arc::clone(&preview_partial);
    let server_target_attempts = Arc::clone(&target_attempts);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request
            .request_line
            .contains(&format!("/segment-{TARGET_SEGMENT_INDEX}.ts"))
        {
            let attempt = server_target_attempts.fetch_add(1, Ordering::AcqRel);
            if attempt == 1 {
                let declared_bytes = server_preview_partial.len().saturating_add(188);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {declared_bytes}\r\nConnection: close\r\n\r\n"
                )
                .expect("write preview response headers");
                stream
                    .write_all(&server_preview_partial)
                    .expect("write preview response prefix");
                stream.flush().expect("flush preview response prefix");
                preview_prefix_sender
                    .send(())
                    .expect("publish preview prefix delivery");
                stream
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .expect("bound preview disconnect observation");
                let disconnect_deadline = Instant::now() + TEST_TIMEOUT;
                let mut peer_probe = [0_u8; 1];
                loop {
                    match stream.peek(&mut peer_probe) {
                        Ok(0) => {
                            preview_disconnected_sender
                                .send(())
                                .expect("publish physical preview disconnect");
                            return;
                        }
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => {
                            preview_disconnected_sender
                                .send(())
                                .expect("publish physical preview disconnect error");
                            return;
                        }
                    }
                    assert!(
                        Instant::now() < disconnect_deadline,
                        "cancelled preview response остался физически подключён"
                    );
                }
            }
        }
        if request
            .request_line
            .contains(&format!("/segment-{SUPERSEDING_SEGMENT_INDEX}.ts"))
        {
            final_started_sender
                .send(())
                .expect("publish final target request");
        }
        stream
            .write_all(&ordinary_segment_response(
                &request.request_line,
                &server_segments,
                &server_segments[TARGET_SEGMENT_INDEX as usize],
            ))
            .expect("write ordinary fixture segment");
    });
    let mut open_request = discontinuous_post_target_request(&server);
    let selected_url = server.target("/transient-manifest-retry.m3u8");
    open_request.http = adaptive_context_without_completed_cache(
        &selected_url,
        CancellationToken::new(),
        SourceGeneration::new(1),
        TestQueries::default(),
    );
    let (seek_handle, mut demuxer) = prepared_worker_from_request(open_request);

    let initial_target_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match next_ready_event(&mut *demuxer).expect("initial target-index playback") {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video
                    && packet.pts
                        >= Duration::from_secs(TARGET_SEGMENT_INDEX * SEGMENT_SECONDS) =>
            {
                break;
            }
            DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::Packet(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("stream ended before target anchor proof"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
        assert!(
            Instant::now() < initial_target_deadline,
            "initial playback не доказал target preview anchor"
        );
    }
    loop {
        match next_ready_event(&mut *demuxer).expect("finish initial index playback") {
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::Packet(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }

    let preview = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(75)))
        .expect("preview command публикует ранее доказанный exact anchor");
    assert_eq!(
        preview.actual_position.as_duration(),
        Duration::from_secs(70)
    );
    let preview_start_deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if preview_prefix_receiver.try_recv().is_ok() {
            break;
        }
        match demuxer.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_))
            | Ok(DemuxReadEvent::TracksChanged(_))
            | Ok(DemuxReadEvent::MediaMetadataChanged(_)) => {}
            Ok(event) => panic!(
                "preview replacement завершился до partial body: {event:?}; attempts={}; requests={:?}",
                target_attempts.load(Ordering::Acquire),
                server.requests()
            ),
            Err(error) => panic!(
                "preview replacement failed before partial body: {error:#}; attempts={}; requests={:?}",
                target_attempts.load(Ordering::Acquire),
                server.requests()
            ),
        }
        assert!(
            Instant::now() < preview_start_deadline,
            "preview replacement не открыл partial body; attempts={}; requests={:?}",
            target_attempts.load(Ordering::Acquire),
            server.requests()
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let final_fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            final_fence,
            DemuxSeekRequest::decode_point_before(Duration::from_secs(35)),
        )
        .expect("enqueue final receipted seek");
    preview_disconnected_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("final receipt must physically close preview body");
    final_started_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("single worker must start final request after preview disconnect");

    let receipt = wait_for_receipt(&seek_handle);
    assert_eq!(receipt.fence, final_fence);
    let ProgressiveAsyncSeekOutcome::Succeeded(final_result) = receipt.outcome else {
        panic!("final seek обязан завершиться успешно: {receipt:?}");
    };
    assert_eq!(
        final_result.actual_position.as_duration(),
        Duration::from_secs(40)
    );
    assert_eq!(
        target_attempts.load(Ordering::Acquire),
        2,
        "obsolete preview нельзя рестартовать либо публиковать в completed cache"
    );
    let presented_packet = first_post_seek_video_packet(&mut *demuxer);
    assert_eq!(
        presented_packet.pts,
        Duration::from_secs(40),
        "stale discontinuous preview generation не должна публиковать packet"
    );
}
