use super::test_support::*;
use super::*;

fn final_seek_harness_with_actual_position(
    target: Duration,
    actual: Duration,
) -> SeekRegressionHarness {
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(vec![video_track.clone()], target, actual, Vec::new());
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));
    harness.start_final_seek(MediaTime::from_duration(target));
    let _events_before_present = harness.session.take_events();

    harness
}

/// Собирает video-only playing seek, где demux actual раньше requested target.
fn playing_final_seek_harness_with_actual_position(
    target: Duration,
    actual: Duration,
) -> SeekRegressionHarness {
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(vec![video_track.clone()], target, actual, Vec::new());
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));
    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness.start_final_seek(MediaTime::from_duration(target));
    let _events_before_present = harness.session.take_events();

    harness
}

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
fn seek_audio_gate_treats_no_selected_audio_as_ready() {
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);

    let gate_status = classify_seek_audio_gate(
        seek_commit,
        AudioSeekRuntimeState::NoSelectedAudio,
        seek_commit.generation,
        None,
        50.0,
    );

    assert_eq!(gate_status, SeekAudioGateStatus::Ready);
}

#[test]
fn seek_audio_gate_preserves_clear_decoder_output_and_preroll_blockers() {
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);

    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation - 1,
            Some(100.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForClear
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::WaitingForDecoder,
            seek_commit.generation,
            Some(100.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::WaitingForOutput,
            seek_commit.generation,
            Some(100.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForOutput
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation,
            None,
            50.0,
        ),
        SeekAudioGateStatus::WaitingForPreroll
    );
}

#[test]
fn final_play_seek_audio_gate_requires_minimal_preroll() {
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);

    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation,
            Some(49.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForPreroll
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation,
            Some(50.0),
            50.0,
        ),
        SeekAudioGateStatus::Ready
    );
}

#[test]
fn paused_seek_audio_gate_skips_runtime_preroll_after_clear() {
    let paused_commit = audio_gate_seek_commit(PlaybackResumeIntent::Pause);

    assert_eq!(
        classify_seek_audio_gate(
            paused_commit,
            AudioSeekRuntimeState::WaitingForOutput,
            paused_commit.generation,
            None,
            50.0,
        ),
        SeekAudioGateStatus::Ready
    );
}

#[test]
fn video_audio_seek_waits_for_audio_runtime_after_clear_ack() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("seek commit должен остаться открытым до audio runtime readiness");

    assert_eq!(
        session.pipeline.audio_buffer_clear_generation(),
        seek_commit.generation
    );
    assert_eq!(
        session.pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForDecoder
    );
    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert!(!session.seek_audio_gate_ready(seek_commit, 50.0));
}

#[test]
fn audio_only_seek_does_not_commit_when_output_is_absent() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("audio-only seek должен ждать audio runtime readiness");

    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert!(!session.seek_audio_gate_ready(seek_commit, 50.0));

    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at + Duration::from_millis(250),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(session.seek_commit().is_some());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);
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
        .enqueue_pending_audio_packet(PendingAudioPacket::new(
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
fn active_seek_diagnostics_identifies_demux_blocker_before_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    let diagnostics = session
        .active_seek_diagnostics(
            Instant::now() + Duration::from_millis(300),
            &PlayerTickConfig::default(),
        )
        .expect("active seek diagnostics should exist while seek commit is open");

    assert_eq!(diagnostics.kind, "seek");
    assert_eq!(diagnostics.target, Duration::from_secs(5));
    assert_eq!(
        diagnostics.blocker,
        crate::SeekProgressBlocker::WaitingForDemux
    );
    assert!(!diagnostics.target_frame_presented);
    assert_eq!(diagnostics.queues.present_queue_depth, 0);
}

#[test]
fn seek_progress_blocker_reports_post_flush_keyframe_drops() {
    let mut session = PlayerSession::new();

    session.pipeline.require_video_decoder_keyframe();
    session.record_video_decoder_bootstrap_started();
    session.record_video_packet_dropped_until_keyframe();

    let queues = PipelineQueueDepthSnapshot::default();
    let seek_bootstrap = session
        .diagnostics
        .snapshot_with_queues(queues)
        .seek_bootstrap;
    let blocker = session.video_target_frame_blocker(queues, seek_bootstrap);

    assert_eq!(blocker, SeekProgressBlocker::WaitingForPostFlushKeyframe);
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
    assert!(session.snapshot().timeline.seeking);
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
    assert!(session.snapshot().timeline.seeking);
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
    assert!(session.snapshot().timeline.stale_frame);
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
        generation_after_first_seek
    );
    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
    assert!(session.snapshot().timeline.stale_frame);
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::DecoderFlushFailed)
    ));
}

#[test]
fn commit_timeout_pauses_and_reports_recoverable_seek_error() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();
    let timeout_now = session
        .seek_commit()
        .expect("final seek должен быть активен до timeout")
        .started_at
        + Duration::from_secs(11);
    let timeout_diagnostics = session
        .active_seek_diagnostics(timeout_now, &PlayerTickConfig::default())
        .expect("active seek diagnostics должны быть доступны до timeout");

    assert_eq!(
        timeout_diagnostics.blocker,
        SeekProgressBlocker::WaitingForDemux
    );

    session.finish_seek_commit_if_ready_for_tests(
        timeout_now,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::SeekTimeout)
    ));
    let timeout_error = session
        .snapshot()
        .last_error
        .as_ref()
        .expect("timeout должен записать recoverable error");
    assert!(
        timeout_error
            .message
            .contains(timeout_diagnostics.blocker.metric_name())
    );
}

#[test]
fn final_seek_timeout_keeps_old_present_frame_stale() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session.set_snapshot_duration(Some(Duration::from_secs(120)));
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(968), 42));

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_millis(96_784),
        )))
        .unwrap();
    let active_seek = session
        .seek_commit()
        .expect("final seek должен быть активен до timeout");
    assert_eq!(active_seek.target_position, MediaTime::from_millis(96_784));
    let timeout_now = active_seek.started_at + Duration::from_secs(11);

    session.finish_seek_commit_if_ready_for_tests(
        timeout_now,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
    assert!(session.snapshot().timeline.stale_frame);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_millis(968))
    );
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::SeekTimeout)
    ));
}

#[test]
fn final_ready_gates_after_budget_commit_instead_of_timeout() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);
    let seek_commit = session
        .seek_commit()
        .expect("final seek должен быть активен до late tick");
    let late_tick_now = seek_commit.started_at + Duration::from_secs(11);

    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::Ready
    );

    session.finish_seek_commit_if_ready_for_tests(
        late_tick_now,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
    assert!(session.snapshot().last_error.is_none());
    let events = session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekCommitted(commit)
            if commit.target_position == Duration::from_secs(6)
                && commit.actual_position == Duration::from_secs(6)
                && commit.resume_intent == PlaybackResumeIntent::Pause
    )));
}

#[test]
fn current_generation_frame_before_actual_does_not_commit_final_seek() {
    let target_position = Duration::from_secs(6);
    let actual_position = Duration::from_secs(5);
    let stale_tail_position = Duration::from_millis(4_950);
    let mut harness = final_seek_harness_with_actual_position(target_position, actual_position);

    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            stale_tail_position,
            90,
        ));
    harness
        .session
        .note_presented_frame_for_seek(stale_tail_position);
    let seek_commit = harness.aligned_seek_commit();

    assert!(harness.session.snapshot().timeline.stale_frame);
    assert!(!harness.session.seek_video_gate_ready(seek_commit, 1));

    harness.session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(harness.session.seek_commit().is_some());
    assert_eq!(harness.session.snapshot().current_position, Duration::ZERO);
    assert!(
        !harness
            .session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );
}

#[test]
fn current_generation_frame_exactly_at_actual_does_not_commit_final_seek() {
    let target_position = Duration::from_secs(6);
    let actual_position = Duration::from_secs(5);
    let mut harness = final_seek_harness_with_actual_position(target_position, actual_position);

    present_frame_for_current_seek_generation(&mut harness.session, actual_position, 91);
    let seek_commit = harness.aligned_seek_commit();

    assert!(!harness.session.seek_video_gate_ready(seek_commit, 1));

    harness.session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(harness.session.seek_commit().is_some());
    assert_eq!(harness.session.snapshot().current_position, Duration::ZERO);
    assert!(
        !harness
            .session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );
}

#[test]
fn current_generation_frame_after_actual_before_target_does_not_commit_final_seek() {
    let target_position = Duration::from_secs(6);
    let actual_position = Duration::from_secs(5);
    let decode_safe_frame_position = Duration::from_millis(5_500);
    let mut harness = final_seek_harness_with_actual_position(target_position, actual_position);

    present_frame_for_current_seek_generation(&mut harness.session, decode_safe_frame_position, 92);
    let seek_commit = harness.aligned_seek_commit();

    assert!(!harness.session.seek_video_gate_ready(seek_commit, 1));

    harness.session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(harness.session.seek_commit().is_some());
    assert_eq!(harness.session.snapshot().current_position, Duration::ZERO);
    assert!(
        !harness
            .session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );
}

#[test]
fn paused_before_scrub_stays_paused_after_commit() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, Vec::new());

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
        .unwrap();

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn playing_before_scrub_resumes_after_gates() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, Vec::new());

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session.dispatch_command(PlayerCommand::Pause).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
        .unwrap();
    session.dispatch_command(PlayerCommand::Play).unwrap();

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn direct_scrub_pause_command_sets_pause_resume_intent() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, Vec::new());

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session.dispatch_command(PlayerCommand::Pause).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
        .unwrap();

    assert_eq!(
        session
            .seek_commit()
            .map(|seek_commit| seek_commit.resume_intent),
        Some(PlaybackResumeIntent::Pause)
    );

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn no_audio_media_seek_resumes_after_target_video_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn deselected_audio_path_seek_resumes_after_target_video_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.disable_selected_audio_path();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(6),
            42,
        ));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_016),
            43,
        ));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_033),
            44,
        ));
    session.note_presented_frame_for_seek(Duration::from_secs(6));

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(session.pipeline.selected_audio_track_id().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn active_seek_drains_target_frame_when_present_queue_is_full() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_secs(5), 5));
    fake_decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
        &session,
        Duration::from_secs(6),
        6,
    ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(1, 4),
    ));

    assert_eq!(tick_result.decoded_video_frames, 1);
    assert_eq!(tick_result.video_frames_presented, 1);
    assert!(session.seek_commit().is_none());
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(6))
    );
    assert!(session.pipeline.video_present_queue_is_empty());
    assert!(!session.snapshot().timeline.stale_frame);
    assert!(
        !tick_result
            .pipeline_pauses
            .iter()
            .any(|pause| { pause.reason == crate::PipelinePauseReason::WaitingForPresentQueue })
    );
    assert!(
        fake_decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(5))
    );
}

#[test]
fn active_seek_sends_video_packet_when_present_queue_is_full() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(6),
            60,
        ));
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(6_016),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"post-seek-video"),
            true,
        ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            max_video_present_queue: 1,
            min_video_present_queue: 1,
            target_video_present_queue: 1,
            max_video_packets_sent_per_tick: 1,
            seek_resume_video_min_ready_frames: 1,
            ..PlayerTickConfig::default()
        },
    ));

    assert_eq!(fake_decoder.sent_packets().len(), 1);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert!(
        !tick_result
            .pipeline_pauses
            .iter()
            .any(|pause| { pause.reason == crate::PipelinePauseReason::WaitingForPresentQueue })
    );
}

#[test]
fn active_accurate_seek_demux_reads_target_video_through_audio_preroll_backpressure() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        vec![
            fake_audio_packet(audio_track.id, target_position, Duration::from_millis(20)),
            fake_video_packet_with_keyframe(
                video_track.id,
                target_position,
                PacketKeyframe::Keyframe,
            ),
        ],
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 8,
            max_video_packets_sent_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 8,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        2,
        "accurate seek preroll должен читать target video в том же tick-е, а не останавливаться на audio packet"
    );
    assert_eq!(harness.sent_packets().len(), 1);
    assert_eq!(harness.sent_packets()[0].pts, target_position);
    assert!(
        !tick_result
            .pipeline_pauses
            .iter()
            .any(|pause| pause.reason == crate::PipelinePauseReason::DemuxBackpressure)
    );
}

#[test]
fn active_accurate_seek_demux_budget_ignores_dropped_audio_preroll() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let mut packets = Vec::new();
    for packet_index in 0..32u64 {
        packets.push(fake_audio_packet(
            audio_track.id,
            Duration::from_millis(packet_index * 20),
            Duration::from_millis(20),
        ));
    }
    packets.push(fake_video_packet_with_keyframe(
        video_track.id,
        target_position,
        PacketKeyframe::Keyframe,
    ));
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_time_budget: Duration::from_millis(50),
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        1,
        "полностью отброшенный audio preroll не должен создавать unbounded per-packet telemetry"
    );
    assert_eq!(
        tick_result.dropped_seek_audio_preroll_packets, 32,
        "полностью отброшенный audio preroll учитывается aggregate counter-ом"
    );
    assert_eq!(harness.sent_packets().len(), 1);
    assert_eq!(harness.sent_packets()[0].pts, target_position);
    assert!(harness.session.pipeline.pending_audio_packet_is_empty());

    let diagnostics = harness
        .session
        .active_seek_diagnostics(Instant::now(), &seek_regression_tick_config())
        .expect("active accurate seek должен иметь diagnostics до target frame");
    assert_eq!(diagnostics.seek_mode, SeekMode::Accurate);
    assert!(diagnostics.accurate_preroll.active);
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .skipped_audio_preroll_packets,
        32
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .demux_events
            .audio_packets,
        32
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .demux_events
            .video_packets,
        1
    );
    assert!(
        diagnostics
            .accurate_preroll
            .stages
            .first_post_seek_packet_elapsed
            .is_some()
    );
    assert!(
        diagnostics
            .accurate_preroll
            .stages
            .first_target_or_after_video_packet_elapsed
            .is_some()
    );
}

#[test]
fn active_accurate_seek_interleaves_demux_and_decoder_io_during_fast_preroll() {
    let target_position = Duration::from_millis(300);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let packets = vec![
        fake_audio_packet(
            audio_track.id,
            Duration::from_millis(0),
            Duration::from_millis(20),
        ),
        fake_video_packet_with_keyframe(
            video_track.id,
            Duration::from_millis(0),
            PacketKeyframe::Keyframe,
        ),
        fake_audio_packet(
            audio_track.id,
            Duration::from_millis(20),
            Duration::from_millis(20),
        ),
        fake_video_packet_with_keyframe(
            video_track.id,
            Duration::from_millis(100),
            PacketKeyframe::NotKeyframe,
        ),
        fake_audio_packet(
            audio_track.id,
            Duration::from_millis(40),
            Duration::from_millis(20),
        ),
        fake_video_packet_with_keyframe(
            video_track.id,
            target_position,
            PacketKeyframe::NotKeyframe,
        ),
    ];
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            max_decoded_video_frames_drained_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_video_packet_burst: 1,
            seek_fast_preroll_time_budget: Duration::from_millis(50),
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        3,
        "fast-preroll loop должен записывать per-packet telemetry только для queued video packets"
    );
    assert_eq!(
        tick_result.dropped_seek_audio_preroll_packets, 3,
        "pre-target audio preroll должен оставаться aggregate diagnostics"
    );
    assert_eq!(
        harness
            .sent_packets()
            .iter()
            .map(|packet| packet.pts)
            .collect::<Vec<_>>(),
        vec![
            Duration::from_millis(0),
            Duration::from_millis(100),
            target_position
        ],
        "active accurate seek должен отправлять каждый найденный GOP packet в том же tick-е"
    );
    assert!(harness.session.pipeline.pending_video_packet_is_empty());
    assert!(harness.session.pipeline.pending_audio_packet_is_empty());
}

#[test]
fn active_accurate_seek_without_catch_up_deadline_keeps_dropped_audio_scan_bounded() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let mut packets = Vec::new();
    for packet_index in 0..32u64 {
        packets.push(fake_audio_packet(
            audio_track.id,
            Duration::from_millis(packet_index * 20),
            Duration::from_millis(20),
        ));
    }
    packets.push(fake_video_packet_with_keyframe(
        video_track.id,
        target_position,
        PacketKeyframe::Keyframe,
    ));
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_time_budget: Duration::ZERO,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        1,
        "без catch-up deadline dropped audio preroll должен оставаться bounded обычным demux budget"
    );
    assert!(harness.sent_packets().is_empty());
    assert!(harness.session.pipeline.pending_audio_packet_is_empty());
}

#[test]
fn active_accurate_seek_sends_pre_target_video_packets_in_burst() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::ZERO,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    for frame_index in 0..10u64 {
        let packet_pts = Duration::from_millis(frame_index * 100);
        let packet_keyframe = if frame_index == 0 {
            PacketKeyframe::Keyframe
        } else {
            PacketKeyframe::NotKeyframe
        };
        harness.session.pipeline.enqueue_pending_video_packet(
            PendingVideoPacket::new_with_decode_timestamps(
                TrackId::new(1),
                packet_pts,
                None,
                None,
                harness.session.pipeline.seek_generation(),
                Bytes::from_static(b"seek-preroll-video"),
                packet_keyframe,
            ),
        );
    }
    harness.session.pipeline.enqueue_pending_video_packet(
        PendingVideoPacket::new_with_decode_timestamps(
            TrackId::new(1),
            target_position,
            None,
            None,
            harness.session.pipeline.seek_generation(),
            Bytes::from_static(b"seek-target-video"),
            PacketKeyframe::NotKeyframe,
        ),
    );

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            max_video_packets_sent_per_tick: 1,
            max_decoded_video_frames_drained_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 16,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        harness.sent_packets().len(),
        11,
        "active accurate seek должен быстро прокачать pre-target GOP до decoder-а"
    );
    assert_eq!(tick_result.demuxed_packets.len(), 0);
    assert!(harness.session.seek_commit().is_some());
    assert!(harness.session.pipeline.pending_video_packet_is_empty());

    let diagnostics = harness
        .session
        .active_seek_diagnostics(Instant::now(), &seek_regression_tick_config())
        .expect("active accurate seek должен оставаться открыт без decoded target frame");
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .video_preroll_packets_sent,
        10
    );
}

#[test]
fn accurate_seek_sets_decoder_output_floor_with_target_and_generation() {
    let target_position = Duration::from_secs(2);
    let actual_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let seek_commit = harness.aligned_seek_commit();

    assert_eq!(
        harness.decoder.preroll_floor_sets(),
        vec![video_core::VideoPrerollOutputFloor {
            generation: seek_commit.generation,
            floor_pts: target_position,
            retain_latest_before_floor: true,
        }]
    );
    assert!(
        harness
            .session
            .decoder_output_floor_applies_to_seek_preroll_packet(
                actual_position,
                seek_commit.generation
            )
    );
}

#[test]
fn accurate_seek_clears_decoder_output_floor_on_commit() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::from_millis(1_500),
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let seek_generation = harness.aligned_seek_commit().generation;
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            target_position,
            22,
        ));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness.decoder.preroll_floor_clears(),
        vec![video_core::VideoPrerollOutputFloorClear::MatchingGeneration(seek_generation)]
    );
}

#[test]
fn new_accurate_seek_clears_old_decoder_output_floor_generation() {
    let first_target = Duration::from_secs(2);
    let second_target = Duration::from_secs(4);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        first_target,
        Duration::from_millis(1_500),
        Vec::new(),
    )
    .with_seek_result(scripted_seek_result(
        second_target,
        Duration::from_millis(3_500),
    ));
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(first_target));
    let first_generation = harness.aligned_seek_commit().generation;
    harness.start_final_seek(MediaTime::from_duration(second_target));
    let second_generation = harness.aligned_seek_commit().generation;

    assert_ne!(first_generation, second_generation);
    assert_eq!(
        harness.decoder.preroll_floor_clears(),
        vec![video_core::VideoPrerollOutputFloorClear::MatchingGeneration(first_generation)]
    );
    assert_eq!(
        harness.decoder.preroll_floor_sets(),
        vec![
            video_core::VideoPrerollOutputFloor {
                generation: first_generation,
                floor_pts: first_target,
                retain_latest_before_floor: true,
            },
            video_core::VideoPrerollOutputFloor {
                generation: second_generation,
                floor_pts: second_target,
                retain_latest_before_floor: true,
            },
        ]
    );
}

#[test]
fn unsupported_decoder_output_floor_keeps_player_side_preroll_drop_path() {
    let target_position = Duration::from_secs(2);
    let actual_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .decoder
        .push_preroll_floor_result(video_core::VideoPrerollOutputFloorResult::Unsupported);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let seek_generation = harness.aligned_seek_commit().generation;
    assert!(
        !harness
            .session
            .decoder_output_floor_applies_to_seek_preroll_packet(actual_position, seek_generation)
    );

    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            actual_position,
            15,
        ));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    assert_eq!(tick_result.video_frames_presented, 0);
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame(),
        "unsupported decoder floor должен оставить старый player-side fallback/drop path"
    );
    assert!(harness.session.seek_commit().is_some());
}

#[test]
fn active_accurate_seek_uses_seek_specific_video_packet_burst() {
    let target_position = Duration::from_secs(4);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::ZERO,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    for frame_index in 0..20u64 {
        harness.session.pipeline.enqueue_pending_video_packet(
            PendingVideoPacket::new_with_decode_timestamps(
                TrackId::new(1),
                Duration::from_millis(frame_index * 100),
                None,
                None,
                harness.session.pipeline.seek_generation(),
                Bytes::from_static(b"seek-preroll-video"),
                if frame_index == 0 {
                    PacketKeyframe::Keyframe
                } else {
                    PacketKeyframe::NotKeyframe
                },
            ),
        );
    }
    harness.session.pipeline.enqueue_pending_video_packet(
        PendingVideoPacket::new_with_decode_timestamps(
            TrackId::new(1),
            target_position,
            None,
            None,
            harness.session.pipeline.seek_generation(),
            Bytes::from_static(b"seek-target-video"),
            PacketKeyframe::NotKeyframe,
        ),
    );

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            max_video_packets_sent_per_tick: 1,
            max_decoded_video_frames_drained_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_video_packet_burst: 32,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(harness.sent_packets().len(), 21);
    assert_eq!(harness.sent_packets()[20].pts, target_position);
    assert_eq!(tick_result.demuxed_packets.len(), 0);
    assert!(harness.session.pipeline.pending_video_packet_is_empty());
}

#[test]
fn final_seek_releases_stale_present_when_texture_pressure_blocks_decoder() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    fake_decoder.set_resource_snapshot(decoder_resource_snapshot_for_tests(1, 1));
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_secs(6),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"target-video"),
            true,
        ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            min_texture_slots_available_for_decode: 0,
            max_video_packets_sent_per_tick: 1,
            ..seek_admission_tick_config(1, 4)
        },
    ));

    assert_eq!(fake_decoder.sent_packets().len(), 1);
    assert_eq!(
        fake_decoder.released_handles(),
        vec![video_core::FrameResourceHandle(1)]
    );
    assert!(session.pipeline.present_video_frame().is_none());
    assert!(session.snapshot().timeline.stale_frame);
    assert!(session.seek_commit().is_some());
    assert!(!tick_result.pipeline_pauses.iter().any(|pause| {
        matches!(
            pause.reason,
            crate::PipelinePauseReason::WaitingForFreeSurface
                | crate::PipelinePauseReason::WaitingForGpuRelease
        )
    }));
}

#[test]
fn active_seek_long_preroll_drain_keeps_present_queue_bounded() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::ZERO,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));

    for frame_index in 0..32u64 {
        harness
            .decoder
            .push_decoded_frame(decoded_frame_for_current_seek_generation(
                &harness.session,
                Duration::from_millis(frame_index * 50),
                100 + frame_index,
            ));
    }
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            target_position,
            200,
        ));

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 64),
    ));

    assert_eq!(tick_result.decoded_video_frames, 33);
    assert!(harness.session.seek_commit().is_none());
    assert!(harness.session.pipeline.video_present_queue_len() <= 2);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(target_position)
    );
    assert_eq!(
        tick_result.dropped_video_frames.len(),
        32,
        "весь pre-target GOP должен остаться seek-preroll, а не playback"
    );
    assert!(
        !harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );
}

#[test]
fn final_seek_retains_latest_pre_target_frame_without_landing_preview() {
    let target_position = Duration::from_secs(2);
    let first_landing_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        first_landing_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            first_landing_position,
            15,
        ));
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(1_900),
            19,
        ));

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    assert_eq!(tick_result.decoded_video_frames, 2);
    // Pre-target кадр остаётся только EOF fallback candidate-ом: scheduler не показывает
    // его как preview, а ранний candidate релизится без texture leak.
    assert_eq!(tick_result.video_frames_presented, 0);
    assert_eq!(tick_result.dropped_video_frames.len(), 1);
    assert_eq!(
        harness.decoder.released_handles(),
        vec![video_core::FrameResourceHandle(15)]
    );
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        None
    );
    assert_eq!(harness.session.pipeline.video_present_queue_len(), 0);
    assert!(!harness.session.snapshot().timeline.stale_frame);
    assert!(harness.session.seek_commit().is_some());
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );
}

#[test]
fn accurate_seek_freezes_pre_seek_frame_then_commits_on_target_frame() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::from_millis(1_500),
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 10));
    // max_demux_packets_per_tick: 0 держит demuxer от EOF, чтобы проверить чистый preroll path
    // без near-EOF fallback. Кадры приходят прямо из fake decoder.
    let preroll_tick_config = || PlayerTickConfig {
        max_demux_packets_per_tick: 0,
        ..seek_admission_tick_config(2, 4)
    };

    harness.start_final_seek(MediaTime::from_duration(target_position));

    // Шаг 1: первый pre-target кадр не презентуется; экран держит pre-seek кадр.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(1_500),
            15,
        ));
    let first_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        preroll_tick_config(),
    ));
    assert_eq!(first_tick.video_frames_presented, 0);
    assert_eq!(first_tick.video_frames_repeated, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(1))
    );
    assert!(harness.session.seek_commit().is_some());
    assert!(harness.session.snapshot().timeline.stale_frame);
    // Позиция не сдвинута pre-target кадром: остаётся на user target до коммита.
    assert_eq!(harness.session.pipeline.media_clock_base(), target_position);
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );

    // Шаг 2: более близкий к target кадр заменяет fallback candidate, но не present frame.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(1_900),
            19,
        ));
    let closer_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        preroll_tick_config(),
    ));
    assert_eq!(closer_tick.video_frames_presented, 0);
    assert_eq!(closer_tick.video_frames_repeated, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(1))
    );
    assert!(
        harness
            .decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(15)),
        "замена fallback candidate должна релизить старый pre-target frame"
    );
    assert!(harness.session.seek_commit().is_some());

    // Шаг 3: точный target кадр заменяет pre-seek кадр, закрывает gate и коммитит target.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            target_position,
            22,
        ));
    let target_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        preroll_tick_config(),
    ));
    assert_eq!(target_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(target_position)
    );
    assert!(
        harness
            .decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(19)),
        "target кадр должен очистить fallback candidate без texture leak"
    );
    assert!(
        harness
            .decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(10)),
        "target кадр должен релизить pre-seek present frame"
    );
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.snapshot().current_position, target_position);
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.stale_frame);
}

#[test]
fn accurate_seek_keeps_audio_fully_gated_before_target_without_preview() {
    let target_position = Duration::from_secs(2);
    let actual_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
    let audio_handle = install_ready_audio_runtime(&mut harness.session, 0.0, None);

    // Play-intent seek: после прохождения gate-а аудио должно возобновиться, но НЕ до target.
    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness.start_final_seek(MediaTime::from_duration(target_position));
    assert_eq!(harness.session.pipeline.media_clock_base(), target_position);
    let play_count_before = audio_handle.play_count.load(Ordering::Relaxed);

    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            actual_position,
            15,
        ));
    let preroll_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    // Pre-target video не показывается и не двигает audio/playback state.
    assert_eq!(preroll_tick.video_frames_presented, 0);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        None
    );
    // Аудио полностью gated: output не запускался, clock base держится на target,
    // seek ещё активен и playback не возобновлён.
    assert_eq!(
        audio_handle.play_count.load(Ordering::Relaxed),
        play_count_before
    );
    assert_eq!(harness.session.pipeline.media_clock_base(), target_position);
    assert!(harness.session.seek_commit().is_some());
    assert_eq!(
        harness.session.snapshot().playback_state,
        PlaybackState::Seeking
    );
}

#[test]
fn keyframe_before_seek_does_not_set_accurate_output_floor() {
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
        .dispatch_command(PlayerCommand::Seek(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_duration(target_position)),
            mode: SeekMode::KeyframeBefore,
        }))
        .unwrap();
    assert_eq!(
        harness.aligned_seek_commit().seek_mode,
        SeekMode::KeyframeBefore
    );

    assert!(
        harness.decoder.preroll_floor_sets().is_empty(),
        "KeyframeBefore не должен включать Accurate output-floor"
    );

    // Pre-target кадр KeyframeBefore не маркируется как drop-preroll и потому не оседает в
    // fallback-слоте: его путь — обычная present queue.
    let pre_target = Duration::from_millis(7_000);
    assert!(
        !harness
            .session
            .should_drop_decoded_frame_for_seek(pre_target)
    );
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            pre_target,
            15,
        ));
    let _ = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));
    assert!(
        !harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame(),
        "KeyframeBefore не должен заполнять Accurate fallback-слот"
    );
}

#[test]
fn accurate_seek_eof_commits_on_fallback_frame_after_decoder_drain() {
    let target_position = Duration::from_millis(29_500);
    let landing_position = Duration::from_secs(29);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        landing_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));

    // Шаг 1 (без demux/EOF): pre-target кадр сохраняется только как EOF fallback.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            landing_position,
            29,
        ));
    let preroll_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));
    assert_eq!(preroll_tick.video_frames_presented, 0);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        None
    );
    assert!(harness.session.seek_commit().is_some());
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );

    // Шаг 2: demuxer доходит до EOF, decoder drain уже пуст, поэтому fallback презентуется
    // один раз и закрывает seek по near-EOF policy.
    let eof_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));
    assert_eq!(
        eof_tick.video_frames_presented, 1,
        "EOF fallback должен презентоваться только после decoder drain"
    );
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(landing_position)
    );
    assert_eq!(
        harness.session.snapshot().current_position,
        landing_position
    );
    assert_eq!(
        harness.session.pipeline.media_clock_base(),
        landing_position
    );
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.stale_frame);
}

#[test]
fn paused_video_seek_tick_presents_target_frame_and_stays_paused() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    fake_decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
        &session,
        Duration::from_secs(6),
        6,
    ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(6))
    );
    assert!(!session.snapshot().timeline.stale_frame);
    assert!(
        fake_decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(1))
    );
}

#[test]
fn final_seek_with_frozen_audio_clock_waits_for_audio_runtime_after_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());
    let frozen_clock = Arc::new(ScriptedAudioClock::new());
    session
        .pipeline
        .install_audio_clock(frozen_clock.as_player_clock());

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    fake_decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
        &session,
        Duration::from_millis(6_016),
        16,
    ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_millis(6_016))
    );
    let seek_commit = session
        .seek_commit()
        .expect("video+audio final seek должен ждать audio runtime после target frame");
    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Draining);
    assert!(session.snapshot().timeline.seeking);
    assert!(!session.snapshot().timeline.stale_frame);
}

#[test]
fn no_audio_seek_scheduler_uses_target_before_position_commit() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session.update_current_position(Duration::from_secs(5));

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(24),
        )))
        .unwrap();

    assert_eq!(session.snapshot().current_position, Duration::from_secs(5));
    assert_eq!(
        session.seek_presentation_clock_override(),
        Some(Duration::from_secs(24))
    );

    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(24),
            42,
        ));

    let tick_config = PlayerTickConfig {
        seek_resume_video_min_ready_frames: 1,
        ..PlayerTickConfig::default()
    };
    let tick_result = session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(24))
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(24));
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn no_audio_seek_ignores_decode_safe_frames_before_target_for_resume_budget() {
    let target_position = Duration::from_secs(24);
    let actual_position = Duration::from_millis(23_900);
    let mut harness =
        playing_final_seek_harness_with_actual_position(target_position, actual_position);
    let seek_commit = harness.aligned_seek_commit();

    assert_eq!(
        seek_commit.actual_position,
        MediaTime::from_duration(actual_position)
    );

    present_frame_for_current_seek_generation(&mut harness.session, actual_position, 42);
    harness
        .session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(23_933),
            43,
        ));
    harness
        .session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(23_966),
            44,
        ));

    harness.session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(harness.session.seek_commit().is_some());
    assert_eq!(harness.session.snapshot().current_position, Duration::ZERO);
    assert_eq!(harness.session.pipeline.media_clock_base(), target_position);
    assert_eq!(harness.session.pipeline.video_present_queue_len(), 2);
    assert!(harness.session.snapshot().timeline.stale_frame);
}

#[test]
fn no_audio_seek_worker_wakeup_treats_target_frame_as_immediate() {
    let target_position = Duration::from_secs(24);
    let actual_position = Duration::from_millis(23_900);
    let mut harness =
        playing_final_seek_harness_with_actual_position(target_position, actual_position);

    harness
        .session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            target_position,
            42,
        ));

    let plan = harness.session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, crate::WorkerWakeupReason::FrameReady);
    assert_eq!(plan.delay, Some(Duration::ZERO));
}

#[test]
fn active_accurate_seek_decoder_inflight_preroll_requests_immediate_wakeup() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::ZERO,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    harness.session.pipeline.note_video_packet_sent_to_decoder();

    let plan = harness.session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, crate::WorkerWakeupReason::SeekOrPreroll);
    assert_eq!(plan.delay, Some(Duration::ZERO));
}

#[test]
fn active_seek_blocker_reports_demux_when_only_stale_present_frame_exists() {
    let target_position = Duration::from_secs(8);
    let harness = final_seek_harness_with_actual_position(target_position, Duration::from_secs(3));

    let diagnostics = harness
        .session
        .active_seek_diagnostics(Instant::now(), &PlayerTickConfig::default())
        .expect("active seek diagnostics available");

    assert_eq!(diagnostics.blocker, SeekProgressBlocker::WaitingForDemux);
    assert!(diagnostics.stale_frame);
    assert_eq!(diagnostics.queues.pending_video_packets, 0);
    assert_eq!(diagnostics.queues.present_queue_depth, 0);
}

#[test]
fn no_audio_seek_does_not_force_present_or_clear_stale_for_frame_before_actual() {
    let target_position = Duration::from_secs(24);
    let actual_position = Duration::from_millis(23_900);
    let too_early_position = Duration::from_millis(23_899);
    let mut harness =
        playing_final_seek_harness_with_actual_position(target_position, actual_position);

    harness
        .session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            too_early_position,
            42,
        ));

    let seek_commit = harness.aligned_seek_commit();
    assert!(
        !harness
            .session
            .active_seek_frame_ready_for_scheduler(too_early_position, seek_commit.generation)
    );

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            seek_resume_video_min_ready_frames: 1,
            ..PlayerTickConfig::default()
        },
    ));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert!(harness.session.snapshot().timeline.stale_frame);
    assert!(harness.session.seek_commit().is_some());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(too_early_position)
    );
}

#[test]
fn final_seek_worker_wakeup_treats_target_frame_as_immediate() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(24),
        )))
        .unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(24),
            42,
        ));

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, crate::WorkerWakeupReason::FrameReady);
    assert_eq!(plan.delay, Some(Duration::ZERO));
}

#[test]
fn final_seek_near_eof_presents_current_generation_preroll_fallback() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(30),
        )))
        .unwrap();

    session.enter_eof_drain();
    session.replace_seek_preroll_fallback_frame(decoded_frame_for_tests(
        Duration::from_millis(29_950),
        77,
    ));

    let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_millis(29_950))
    );
    assert!(!session.snapshot().timeline.stale_frame);
    assert!(session.seek_commit().is_none());
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(
        session.snapshot().current_position,
        Duration::from_millis(29_950)
    );
    assert_eq!(
        session.pipeline.media_clock_base(),
        Duration::from_millis(29_950)
    );

    let events = session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekCommitted(commit)
            if commit.target_position == Duration::from_secs(30)
                && commit.resume_intent == PlaybackResumeIntent::Play
    )));
}

#[test]
fn playing_seek_waits_for_configured_video_preroll_before_resume() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);
    assert!(session.snapshot().timeline.seeking);

    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_016),
            43,
        ));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_033),
            44,
        ));

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn playing_seek_resumes_without_waiting_for_configured_video_preroll() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(6),
            42,
        ));
    session.note_presented_frame_for_seek(Duration::from_secs(6));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_016),
            43,
        ));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_033),
            44,
        ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            seek_resume_video_min_ready_frames: 3,
            ..PlayerTickConfig::default()
        },
    ));

    assert_eq!(tick_result.video_frames_presented, 0);
    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(6))
    );
    assert_eq!(session.pipeline.video_present_queue_len(), 2);
}

#[test]
fn playing_video_seek_with_audio_waits_for_audio_runtime_after_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);

    let before_audio_gate_timeout = session
        .seek_commit()
        .expect("seek должен быть активен до проверки audio gate")
        .started_at
        + Duration::from_millis(249);
    session.finish_seek_commit_if_ready_for_tests(
        before_audio_gate_timeout,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    let seek_commit = session
        .seek_commit()
        .expect("video+audio final seek должен остаться открытым без audio runtime");
    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);
    assert!(session.snapshot().timeline.seeking);
}

#[test]
fn playing_final_seek_commits_after_target_frame_and_ready_audio() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    let audio_output = install_ready_audio_runtime(&mut session, 80.0, None);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    let _events_before_seek = session.take_events();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_millis(6_016), 42);
    let seek_commit = session
        .seek_commit()
        .expect("seek должен быть активен до ready audio commit");

    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::Ready
    );

    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(6));
    assert_eq!(session.pipeline.media_clock_base(), Duration::from_secs(6));
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 2);
    assert_eq!(audio_output.pause_count.load(Ordering::Relaxed), 1);
    assert_eq!(audio_output.clear_count.load(Ordering::Relaxed), 1);

    let events = session.take_events();
    let target_frame_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekTargetFramePresented(presentation)
                    if presentation.target_position == Duration::from_secs(6)
                        && presentation.frame_pts == Duration::from_millis(6_016)
            )
        },
        "target frame event должен быть опубликован",
    );
    let commit_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekCommitted(commit)
                    if commit.target_position == Duration::from_secs(6)
                        && commit.actual_position == Duration::from_secs(6)
                        && commit.resume_intent == PlaybackResumeIntent::Play
            )
        },
        "seek commit event должен быть опубликован",
    );
    let audio_resume_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::AudioResumedAfterSeek(info)
                    if info.target_position == Duration::from_secs(6)
            )
        },
        "audio resume event должен быть опубликован после успешного play",
    );

    assert!(target_frame_event_index < commit_event_index);
    assert!(commit_event_index < audio_resume_event_index);
}

#[test]
fn final_seek_audio_play_error_closes_seek_and_reports_visible_error() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.dispatch_command(PlayerCommand::Play).unwrap();
    let _events_before_seek = session.take_events();
    let audio_output =
        install_ready_audio_runtime(&mut session, 80.0, Some("fake audio play failed"));
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);
    let seek_commit = session
        .seek_commit()
        .expect("seek должен быть активен до audio play error");

    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::AudioDeviceUnavailable)
    ));

    let events = session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::AudioDeviceUnavailable
                && error.message.contains("fake audio play failed")
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::AudioResumedAfterSeek(_)))
    );
}

#[test]
fn audio_only_final_seek_commits_when_audio_ready_without_video_gate() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, 80.0, None);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    let _events_before_seek = session.take_events();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();
    let seek_commit = session
        .seek_commit()
        .expect("audio-only seek должен быть активен до audio gate");

    assert!(session.seek_video_gate_ready(seek_commit, 3));
    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::Ready
    );

    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at,
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(5));
    assert_eq!(session.pipeline.media_clock_base(), Duration::from_secs(5));
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 2);

    let events = session.take_events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekCommitted(commit)
            if commit.target_position == Duration::from_secs(5)
                && commit.resume_intent == PlaybackResumeIntent::Play
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PlayerEvent::AudioResumedAfterSeek(_)))
    );
}

#[test]
fn final_play_seek_with_audio_requires_only_presented_target_video_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("accepted seek должен открыть commit");

    assert_eq!(
        session.required_seek_resume_video_ready_frames(seek_commit, 3),
        1
    );
}

#[test]
fn active_seek_diagnostics_reports_audio_preroll_after_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    let gate_snapshot = SeekProgressGateSnapshot {
        target_frame_presented: true,
        video_gate_ready: true,
        audio_gate_status: SeekAudioGateStatus::WaitingForPreroll,
        ready_video_frames: 1,
        required_video_frames: 1,
    };

    let blocker = session.seek_progress_blocker(
        &PlayerTickConfig::default(),
        PipelineQueueDepthSnapshot::default(),
        gate_snapshot,
        SeekBootstrapDiagnosticsSnapshot::default(),
    );

    assert_eq!(blocker, SeekProgressBlocker::WaitingForAudioPreroll);
}

#[test]
fn playing_video_seek_with_audio_soft_fallback_commits_after_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);

    let seek_commit = session
        .seek_commit()
        .expect("seek должен ждать audio до soft fallback deadline");
    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at + Duration::from_millis(250),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
    assert!(session.seek_commit().is_none());
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::RuntimeError)
    ));

    let events = session.take_events();
    let target_frame_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekTargetFramePresented(presentation)
                    if presentation.target_position == Duration::from_secs(6)
                        && presentation.frame_pts == Duration::from_secs(6)
            )
        },
        "target frame event должен быть опубликован до soft fallback commit",
    );
    let commit_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekCommitted(commit)
                    if commit.target_position == Duration::from_secs(6)
                        && commit.actual_position == Duration::from_secs(6)
                        && commit.resume_intent == PlaybackResumeIntent::Play
            )
        },
        "seek commit event должен быть опубликован после soft fallback",
    );
    assert!(target_frame_event_index < commit_event_index);
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::RuntimeError
                && error.message.contains("blocker=audio_decoder")
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::AudioResumedAfterSeek(_)))
    );
}

#[test]
fn paused_video_audio_seek_does_not_wait_for_audio_runtime() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(6));
    assert!(!session.snapshot().timeline.seeking);

    let events = session.take_events();
    let target_frame_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekTargetFramePresented(presentation)
                    if presentation.target_position == Duration::from_secs(6)
                        && presentation.frame_pts == Duration::from_secs(6)
            )
        },
        "paused seek должен опубликовать target frame event",
    );
    let commit_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekCommitted(commit)
                    if commit.target_position == Duration::from_secs(6)
                        && commit.resume_intent == PlaybackResumeIntent::Pause
            )
        },
        "paused seek должен опубликовать commit event",
    );
    assert!(target_frame_event_index < commit_event_index);
}

#[test]
fn ordinary_seek_generation_drops_video_pre_roll_before_target() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();

    assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(5_999)));
    assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_secs(6)));
}

#[test]
fn ordinary_seek_generation_drops_complete_audio_pre_roll_packets() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();

    assert!(session.should_drop_demuxed_audio_packet_for_seek(
        Duration::from_millis(5_900),
        Some(Duration::from_millis(20)),
    ));
    assert!(!session.should_drop_demuxed_audio_packet_for_seek(
        Duration::from_millis(5_990),
        Some(Duration::from_millis(20)),
    ));
    assert!(
        !session.should_drop_demuxed_audio_packet_for_seek(Duration::from_millis(5_900), None,)
    );
    assert!(!session.should_drop_demuxed_audio_packet_for_seek(
        Duration::from_secs(6),
        Some(Duration::from_millis(20)),
    ));
}
