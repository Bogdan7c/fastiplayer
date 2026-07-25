use super::test_support::*;
use super::*;

#[test]
fn seek_command_sets_target_and_commits_position_after_gates() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, Vec::new());
    let request = SeekRequest::absolute(MediaTime::from_millis(1_500));

    session
        .dispatch_command(PlayerCommand::Seek(request))
        .unwrap();

    assert_eq!(session.snapshot().current_position, Duration::ZERO);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_millis(1_500))
    );
    assert!(session.snapshot().timeline.seeking);

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(
        session.snapshot().current_position,
        Duration::from_millis(1_500)
    );
    assert_eq!(
        session.snapshot().timeline.current_position,
        MediaTime::from_millis(1_500)
    );
    assert!(session.take_events().iter().any(
        |event| matches!(event, PlayerEvent::SeekRequested(accepted) if *accepted == request)
    ));
}

#[test]
fn relative_seek_target_resolves_from_current_timeline_position() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, Vec::new());
    session.update_current_position(Duration::from_secs(10));
    let request = SeekRequest {
        target: SeekTarget::Relative(Duration::from_secs(5)),
        mode: crate::SeekMode::Accurate,
    };

    session
        .dispatch_command(PlayerCommand::Seek(request))
        .unwrap();
    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.snapshot().current_position, Duration::from_secs(15));
}

#[test]
fn keyframe_before_seek_keeps_demuxer_target_on_requested_position() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(8)),
            mode: SeekMode::KeyframeBefore,
        }))
        .unwrap();

    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::from_secs(8)]
    );
    assert_eq!(
        session
            .seek_commit()
            .map(|seek_commit| seek_commit.target_position),
        Some(MediaTime::from_secs(8))
    );
}

#[test]
fn accurate_video_seek_passes_requested_target_to_demuxer() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::from_secs(8)]
    );
    assert_eq!(
        session
            .seek_commit()
            .map(|seek_commit| seek_commit.target_position),
        Some(MediaTime::from_secs(8))
    );
}

#[test]
fn keyframe_before_seek_keeps_actual_frame_as_runtime_anchor() {
    let target_position = Duration::from_secs(8);
    let actual_position = Duration::from_millis(7_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));
    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_duration(target_position)),
            mode: SeekMode::KeyframeBefore,
        }))
        .unwrap();
    let seek_commit = harness.aligned_seek_commit();

    assert_eq!(seek_commit.seek_mode, SeekMode::KeyframeBefore);
    assert_eq!(harness.session.pipeline.media_clock_base(), actual_position);
    assert!(
        !harness
            .session
            .should_drop_decoded_frame_for_seek(actual_position)
    );
    let diagnostics = harness
        .session
        .active_seek_diagnostics(Instant::now(), &seek_regression_tick_config())
        .expect("keyframe-before seek тоже имеет обычный active seek snapshot");
    assert_eq!(diagnostics.seek_mode, SeekMode::KeyframeBefore);
    assert!(
        !diagnostics.accurate_preroll.active,
        "KeyframeBefore не должен включать Accurate skip/preroll diagnostics"
    );

    harness.push_decoded_frame(actual_position, 75, 1);
    let tick_result = harness.tick_once();

    assert_eq!(tick_result.video_frames_presented, 1);
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.snapshot().current_position, actual_position);
    assert_eq!(harness.session.pipeline.media_clock_base(), actual_position);
}

#[test]
fn seek_transaction_passes_demux_request_without_runtime_index_hint() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(8));
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
}

#[test]
fn audio_only_accurate_final_seek_uses_demux_accurate_mode() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(2, TrackKind::Audio)],
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(8));
    assert_eq!(requests[0].mode, DemuxSeekMode::Accurate);
}

#[test]
fn keyframe_before_video_seek_uses_decode_point_before_mode() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(8)),
            mode: SeekMode::KeyframeBefore,
        }))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
}

#[test]
fn keyframe_after_seek_is_rejected_before_demux_seek() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(8)),
            mode: SeekMode::KeyframeAfter,
        }))
        .unwrap();

    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(&PlayerErrorKind::SeekUnavailable)
    );
}

#[test]
fn not_seekable_demuxer_marks_timeline_and_blocks_seek() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media_with_seekability(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        },
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    assert!(!session.snapshot().timeline.seekable);
    assert_eq!(
        session.snapshot().timeline.not_seekable_reason,
        Some(TimelineNotSeekableReason::SourceNotSeekable)
    );
    assert!(seek_log.lock().expect("seek log lock").is_empty());
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(&PlayerErrorKind::SeekUnavailable)
    );
}

#[test]
fn seek_transaction_clears_pending_packets_and_calls_demux_seek() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            TrackId::new(2),
            Duration::ZERO,
            None,
            None,
            session.pipeline.seek_generation(),
            Bytes::from_static(&[1, 2, 3]),
        ));
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::ZERO,
            session.pipeline.seek_generation(),
            Bytes::from_static(&[4, 5, 6]),
            true,
        ));
    session.pipeline.mark_video_decoder_bootstrapped();
    session.pipeline.note_video_packet_sent_to_decoder();
    session.pipeline.note_video_packet_sent_to_decoder();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    assert!(session.pipeline.pending_audio_packet_is_empty());
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert_eq!(
        *seek_log
            .lock()
            .expect("seek log mutex should not be poisoned"),
        vec![Duration::from_secs(5)]
    );
    assert_eq!(session.pipeline.seek_generation(), 1);
    assert!(session.pipeline.video_decoder_needs_keyframe());
    assert_eq!(session.pipeline.video_decode_in_flight_packets(), 0);
    assert!(session.seek_commit().is_some());
}

#[test]
fn seek_transaction_resets_installed_audio_decoder() {
    let mut session = PlayerSession::new();
    let reset_count = Arc::new(AtomicUsize::new(0));
    let _seek_log = install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.pipeline.select_audio_track(TrackId::new(2));
    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(Arc::clone(&reset_count)));

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    assert_eq!(reset_count.load(Ordering::Relaxed), 1);
}

#[test]
fn seek_without_video_decoder_treats_absent_flush_as_noop() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let initial_generation = session.pipeline.seek_generation();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    assert_eq!(
        *seek_log
            .lock()
            .expect("seek log mutex should not be poisoned"),
        vec![Duration::from_secs(5)]
    );
    assert_eq!(
        session.pipeline.seek_generation(),
        initial_generation.saturating_add(1)
    );
    assert!(session.seek_commit().is_some());
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(5))
    );
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn seek_successful_decoder_flush_calls_demux_seek_and_advances_generation() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    let initial_generation = session.pipeline.seek_generation();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(7),
        )))
        .unwrap();

    assert_eq!(decoder.flush_count(), 1);
    assert_eq!(
        *seek_log
            .lock()
            .expect("seek log mutex should not be poisoned"),
        vec![Duration::from_secs(7)]
    );
    assert_eq!(
        session.pipeline.seek_generation(),
        initial_generation.saturating_add(1)
    );
    assert!(session.seek_commit().is_some());
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(7))
    );
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn seek_flush_failure_does_not_call_demux_seek_or_advance_generation() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session
        .pipeline
        .set_video_decoder_thread(FailingFlushVideoDecoderThread::new("flush failed"));
    let initial_generation = session.pipeline.seek_generation();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    assert!(
        seek_log
            .lock()
            .expect("seek log mutex should not be poisoned")
            .is_empty()
    );
    assert_eq!(session.pipeline.seek_generation(), initial_generation);
    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
    assert!(!session.snapshot().timeline.stale_frame);
    assert_eq!(session.snapshot().timeline.target_position, None);
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::DecoderFlushFailed)
    ));
    assert!(session.take_events().iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::DecoderFlushFailed
    )));
}

#[test]
fn seek_flush_failure_clears_existing_seek_commit() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(3),
        )))
        .unwrap();
    let generation_after_first_seek = session.pipeline.seek_generation();
    assert!(session.seek_commit().is_some());

    session
        .pipeline
        .set_video_decoder_thread(FailingFlushVideoDecoderThread::new("flush failed"));
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    assert_eq!(
        *seek_log
            .lock()
            .expect("seek log mutex should not be poisoned"),
        vec![Duration::from_secs(3)]
    );
    assert_eq!(
        session.pipeline.seek_generation(),
        generation_after_first_seek.saturating_add(1)
    );
    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
    assert!(!session.snapshot().timeline.stale_frame);
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::DecoderFlushFailed)
    ));
}
