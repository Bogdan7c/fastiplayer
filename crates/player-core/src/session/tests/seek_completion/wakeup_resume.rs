//! Пробуждение worker-а и resume после seek с аудио и без.

use super::*;

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
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
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

    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);

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
    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
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

    assert!(target_frame_event_index < audio_resume_event_index);
    assert!(audio_resume_event_index < commit_event_index);
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
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
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
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::AudioPlaybackResumed))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekCommitted(_)))
    );
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
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
