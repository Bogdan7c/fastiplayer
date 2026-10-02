//! Коммит финального seek: timeout, готовность, pause/resume намерение.

use super::*;

/// Pending seek сохраняет typed timeout, даже если demux ждёт будущий retry deadline.
#[test]
fn commit_timeout_pauses_and_reports_recoverable_seek_error() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session.snapshot.media_instance_id = Some(crate::MediaInstanceId::new_unique());

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();
    let retry_hint = media_core::DemuxRetryHint::new(Duration::from_secs(1))
        .expect("seek timeout fixture использует допустимый bounded retry");
    session.schedule_installed_demux_retry(Instant::now(), retry_hint);
    assert!(session.installed_demux_read_is_blocked(Instant::now()));
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

/// Готовые downstream gates завершают pending seek, пока новый demux read ещё запрещён.
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
    session.snapshot.media_instance_id = Some(crate::MediaInstanceId::new_unique());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    let retry_hint = media_core::DemuxRetryHint::new(Duration::from_secs(1))
        .expect("seek recovery fixture использует допустимый bounded retry");
    session.schedule_installed_demux_retry(Instant::now(), retry_hint);
    assert!(session.installed_demux_read_is_blocked(Instant::now()));
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

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
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
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session.dispatch_command(PlayerCommand::Pause).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
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
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session.dispatch_command(PlayerCommand::Pause).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
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
