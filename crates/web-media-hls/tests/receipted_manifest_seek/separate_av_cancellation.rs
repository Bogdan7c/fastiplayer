// Separate A/V cancellation проходит через настоящий composite worker и loopback HTTP bodies.

use super::*;

#[derive(Clone, Copy)]
enum BlockedComponent {
    Video,
    Audio,
}

#[test]
fn final_receipt_cancels_partial_separate_video_before_atomic_pair_commit() {
    run_partial_component_scenario(BlockedComponent::Video);
}

#[test]
fn final_receipt_cancels_partial_separate_audio_before_atomic_pair_commit() {
    run_partial_component_scenario(BlockedComponent::Audio);
}

fn run_partial_component_scenario(blocked: BlockedComponent) {
    let video_segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|index| long_video_ts_segment(index * SEGMENT_SECONDS * 90_000))
            .collect::<Vec<_>>(),
    );
    let audio_segments = Arc::new(
        (0..SEGMENT_COUNT)
            .map(|index| long_audio_ts_segment(index * SEGMENT_SECONDS * 90_000, SEGMENT_SECONDS))
            .collect::<Vec<_>>(),
    );
    let video_playlist = Arc::new(component_playlist_text("video"));
    let audio_playlist = Arc::new(component_playlist_text("audio"));
    let blocked_attempts = Arc::new(AtomicUsize::new(0));
    let failed_final_video_attempts = Arc::new(AtomicUsize::new(0));
    let successful_final_video_attempts = Arc::new(AtomicUsize::new(0));
    let final_phase = Arc::new(AtomicBool::new(false));
    let successful_final_enabled = Arc::new(AtomicBool::new(false));
    let (prefix_sender, prefix_receiver) = mpsc::sync_channel(1);
    let (disconnect_sender, disconnect_receiver) = mpsc::sync_channel(1);
    let (final_sender, final_receiver) = mpsc::sync_channel(2);
    let server_video = Arc::clone(&video_segments);
    let server_audio = Arc::clone(&audio_segments);
    let server_video_playlist = Arc::clone(&video_playlist);
    let server_audio_playlist = Arc::clone(&audio_playlist);
    let server_attempts = Arc::clone(&blocked_attempts);
    let server_failed_final_attempts = Arc::clone(&failed_final_video_attempts);
    let server_successful_final_attempts = Arc::clone(&successful_final_video_attempts);
    let server_final_phase = Arc::clone(&final_phase);
    let server_successful_final_enabled = Arc::clone(&successful_final_enabled);
    let server = TestServer::start_streaming(move |_, request, stream| {
        if request.request_line.contains("/master.m3u8") {
            stream.write_all(&response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Test audio\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"aud\"\nvideo.m3u8\n",
            )).expect("master response");
            return;
        }
        if request.request_line.contains("/video.m3u8") {
            stream
                .write_all(&response("200 OK", &[], server_video_playlist.as_bytes()))
                .expect("video playlist");
            return;
        }
        if request.request_line.contains("/audio.m3u8") {
            stream
                .write_all(&response("200 OK", &[], server_audio_playlist.as_bytes()))
                .expect("audio playlist");
            return;
        }
        let is_blocked_target = match blocked {
            BlockedComponent::Video => request.request_line.contains("/video-7.ts"),
            BlockedComponent::Audio => request.request_line.contains("/audio-7.ts"),
        };
        if is_blocked_target && server_attempts.fetch_add(1, Ordering::AcqRel) == 1 {
            let prefix = match blocked {
                BlockedComponent::Video => &server_video[7],
                BlockedComponent::Audio => &server_audio[7],
            };
            let prefix_length = prefix.len().min(188 * 6);
            let declared_length = prefix_length + 188;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n"
            )
            .expect("partial A/V headers");
            stream
                .write_all(&prefix[..prefix_length])
                .expect("partial A/V prefix");
            stream.flush().expect("flush partial A/V prefix");
            prefix_sender.send(()).expect("publish partial A/V prefix");
            observe_disconnect(stream, &disconnect_sender);
            return;
        }
        if request.request_line.contains("/video-4.ts")
            && server_final_phase.load(Ordering::Acquire)
        {
            if !server_successful_final_enabled.load(Ordering::Acquire) {
                let failed_attempt = server_failed_final_attempts.fetch_add(1, Ordering::AcqRel);
                if failed_attempt == 0 {
                    final_sender
                        .try_send(false)
                        .expect("publish failed final A/V attempt");
                }
                stream
                    .write_all(&response("404 Not Found", &[], b"failed final proof"))
                    .expect("failed final A/V proof response");
                return;
            }
            let successful_attempt =
                server_successful_final_attempts.fetch_add(1, Ordering::AcqRel);
            if successful_attempt == 0 {
                final_sender
                    .try_send(true)
                    .expect("publish successful final A/V attempt");
            }
        }
        let body = (0..SEGMENT_COUNT)
            .find_map(|index| {
                if request.request_line.contains(&format!("/video-{index}.ts")) {
                    Some(server_video[index as usize].as_slice())
                } else if request.request_line.contains(&format!("/audio-{index}.ts")) {
                    Some(server_audio[index as usize].as_slice())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        stream
            .write_all(&response("200 OK", &[], body))
            .expect("ordinary A/V segment");
    });

    let mut request = separate_av_request(&server);
    // Два queued packets плюс один уже читаемый worker-ом packet не могут поглотить
    // доказательный video/audio tail `75s` после остановки на anchor `70s`.
    request.policy.progressive_limits = ProgressiveDemuxBufferLimits::new(
        NonZeroUsize::new(2).expect("old A/V tail event capacity"),
        NonZeroUsize::new(64 * 1_024).expect("old A/V tail packet bytes"),
    );
    let selected_url = server.target("/master.m3u8");
    request.http = adaptive_context_without_completed_cache(
        &selected_url,
        CancellationToken::new(),
        SourceGeneration::new(1),
        TestQueries::default(),
    );
    let opened = prepare_hls_vod_receipted(
        request,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("open cancellable separate A/V");
    let seek_handle = opened.async_seek_handle().expect("A/V seek handle");
    let mut demuxer = opened.into_demuxer();
    let stable_tracks = initial_track_signature(&mut *demuxer);
    read_initial_old_pair_anchor(&mut *demuxer, &stable_tracks);
    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(75)))
        .expect("preview A/V anchor");
    prefix_receiver
        .recv_timeout(TEST_TIMEOUT)
        .unwrap_or_else(|_| {
            panic!(
                "partial A/V body did not start; requests={:?}",
                server.requests()
            )
        });

    let cancelling_fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    final_phase.store(true, Ordering::Release);
    seek_handle
        .enqueue(
            cancelling_fence,
            DemuxSeekRequest::decode_point_before(Duration::from_secs(35)),
        )
        .expect("enqueue cancelling A/V receipt");
    disconnect_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("cancelled component TCP close");
    assert!(
        !final_receiver
            .recv_timeout(TEST_TIMEOUT)
            .expect("cancelling receipt starts after close")
    );
    let cancelling_receipt = wait_for_receipt(&seek_handle);
    assert_eq!(cancelling_receipt.fence, cancelling_fence);
    assert!(
        matches!(
            cancelling_receipt.outcome,
            ProgressiveAsyncSeekOutcome::Failed
        ),
        "fixture receipt должен завершиться до replacement commit: {cancelling_receipt:?}"
    );
    assert_old_pair_tail_is_still_readable(&mut *demuxer, &stable_tracks);

    successful_final_enabled.store(true, Ordering::Release);
    let successful_fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(2),
    };
    seek_handle
        .enqueue(
            successful_fence,
            DemuxSeekRequest::decode_point_before(Duration::from_secs(35)),
        )
        .expect("enqueue successful A/V receipt");
    assert!(
        final_receiver
            .recv_timeout(TEST_TIMEOUT)
            .expect("successful final A/V starts")
    );
    let receipt = wait_for_receipt(&seek_handle);
    assert_eq!(receipt.fence, successful_fence);
    let ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("final A/V seek failed: {receipt:?}");
    };
    assert!(
        result.actual_position.as_duration() >= Duration::from_secs(40)
            && result.actual_position.as_duration() < Duration::from_secs(41),
        "video RAP должен принадлежать final target segment: {result:?}"
    );
    let mut saw_video = false;
    let mut saw_audio = false;
    let mut tracks_changed_count = 0;
    while !(saw_video && saw_audio) {
        match next_ready_event(&mut *demuxer).expect("post-final A/V event") {
            DemuxReadEvent::TracksChanged(update) => {
                tracks_changed_count += 1;
                assert_eq!(
                    update
                        .tracks
                        .iter()
                        .map(|track| (track.id, track.kind))
                        .collect::<Vec<_>>(),
                    stable_tracks
                );
            }
            DemuxReadEvent::Packet(packet) => {
                assert!(
                    packet.pts < Duration::from_secs(70),
                    "stale preview packet published"
                );
                saw_video |=
                    packet.kind == TrackKind::Video && packet.keyframe == PacketKeyframe::Keyframe;
                saw_audio |= packet.kind == TrackKind::Audio;
            }
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("final A/V ended before both components"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }
    assert_eq!(
        tracks_changed_count, 1,
        "успешная A/V замена должна публиковаться ровно один раз"
    );
    assert!(
        blocked_attempts.load(Ordering::Acquire) >= 2,
        "fixture обязана физически открыть initial и cancelled component bodies"
    );
    assert_eq!(failed_final_video_attempts.load(Ordering::Acquire), 1);
    assert_eq!(successful_final_video_attempts.load(Ordering::Acquire), 1);
}

fn read_initial_old_pair_anchor(demuxer: &mut dyn Demuxer, stable_tracks: &[(TrackId, TrackKind)]) {
    let video_track_id = stable_track_id(stable_tracks, TrackKind::Video);
    let audio_track_id = stable_track_id(stable_tracks, TrackKind::Audio);
    let expected_anchor_pts = Duration::from_secs(70);
    let mut saw_video_anchor = false;
    let mut saw_audio_anchor = false;
    while !(saw_video_anchor && saw_audio_anchor) {
        match next_ready_event(demuxer).expect("initial old A/V anchor") {
            DemuxReadEvent::Packet(packet) => match packet.kind {
                TrackKind::Video => {
                    assert_eq!(packet.track_id, video_track_id);
                    assert!(packet.pts <= expected_anchor_pts);
                    saw_video_anchor |= packet.pts == expected_anchor_pts;
                }
                TrackKind::Audio => {
                    assert_eq!(packet.track_id, audio_track_id);
                    assert!(packet.pts <= expected_anchor_pts);
                    saw_audio_anchor |= packet.pts == expected_anchor_pts;
                }
            },
            DemuxReadEvent::TracksChanged(_) => {
                panic!("initial topology не должна повторно меняться до preview")
            }
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("old A/V pair закончилась до initial anchor"),
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }
}

fn assert_old_pair_tail_is_still_readable(
    demuxer: &mut dyn Demuxer,
    stable_tracks: &[(TrackId, TrackKind)],
) {
    let video_track_id = stable_track_id(stable_tracks, TrackKind::Video);
    let audio_track_id = stable_track_id(stable_tracks, TrackKind::Audio);
    let expected_tail_pts = Duration::from_secs(75);
    let mut saw_video_tail = false;
    let mut saw_audio_tail = false;
    while !(saw_video_tail && saw_audio_tail) {
        match next_ready_event(demuxer).expect("old A/V tail after cancelled candidate") {
            DemuxReadEvent::Packet(packet) => match packet.kind {
                TrackKind::Video => {
                    assert_eq!(packet.track_id, video_track_id);
                    if !saw_video_tail {
                        assert!(packet.pts <= expected_tail_pts);
                        saw_video_tail = packet.pts == expected_tail_pts;
                    }
                }
                TrackKind::Audio => {
                    assert_eq!(packet.track_id, audio_track_id);
                    if !saw_audio_tail {
                        assert!(packet.pts <= expected_tail_pts);
                        saw_audio_tail = packet.pts == expected_tail_pts;
                    }
                }
            },
            DemuxReadEvent::TracksChanged(_) => {
                panic!("cancelled candidate не должен публиковать replacement topology")
            }
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                panic!(
                    "old A/V pair стала EOF после cancelled candidate: \
                     saw_video_tail={saw_video_tail}, saw_audio_tail={saw_audio_tail}"
                )
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => unreachable!(),
        }
    }
}

fn stable_track_id(stable_tracks: &[(TrackId, TrackKind)], kind: TrackKind) -> TrackId {
    stable_tracks
        .iter()
        .find_map(|(track_id, track_kind)| (*track_kind == kind).then_some(*track_id))
        .unwrap_or_else(|| panic!("stable topology не содержит {kind:?} track"))
}

fn observe_disconnect(stream: &mut std::net::TcpStream, sender: &mpsc::SyncSender<()>) {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("disconnect timeout");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut probe = [0_u8; 1];
    loop {
        match stream.peek(&mut probe) {
            Ok(0) => {
                sender.send(()).expect("publish A/V disconnect");
                return;
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                sender.send(()).expect("publish A/V disconnect error");
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "cancelled A/V body remained connected"
        );
    }
}
