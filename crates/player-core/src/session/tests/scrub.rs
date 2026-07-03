use super::test_support::*;
use super::*;

fn live_scrub_settings_for_tests(
    decode_mode: frame_server_core::LiveScrubDecodeMode,
    max_hz: u16,
) -> frame_server_core::LiveScrubSettingsSnapshot {
    frame_server_core::LiveScrubSettingsSnapshot {
        decode_mode,
        max_hz,
    }
}

fn live_scrub_diagnostics_for_tests() -> frame_server_core::LiveScrubDiagnostics {
    frame_server_core::LiveScrubDiagnostics::from_settings_snapshot(live_scrub_settings_for_tests(
        frame_server_core::LiveScrubDecodeMode::ThrottledLatest,
        60,
    ))
}

#[test]
fn inactive_end_scrub_clears_simple_state_without_resetting_unrelated_seek_state() {
    let mut session = PlayerSession::new();

    session.set_seek_eof_fallback_video_position_for_tests(Some(MediaTime::from_secs(29)));
    session.set_simple_scrub_state_for_tests(
        false,
        Some(session_scrub_request(17, SeekMode::Accurate)),
    );
    session.set_seek_commit_for_tests(Some(SeekCommitState {
        generation: 77,
        seek_mode: SeekMode::Accurate,
        target_position: MediaTime::from_secs(17),
        actual_position: MediaTime::from_secs(16),
        started_at: Instant::now(),
        resume_intent: PlaybackResumeIntent::Pause,
    }));

    session.end_scrub(None).unwrap();

    assert!(!session.simple_scrub_active_for_tests());
    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert_eq!(
        session.seek_eof_fallback_video_position_for_tests(),
        Some(MediaTime::from_secs(29))
    );
    assert!(session.seek_commit().is_some());
}

#[test]
fn direct_dispatch_scrub_api_remains_session_compatibility_path() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, Vec::new());
    let _ = session.take_events();
    let request = SeekRequest::absolute(MediaTime::from_secs(3));

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    assert!(session.simple_scrub_active_for_tests());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);
    assert!(!PlaybackState::Scrubbing.is_playback_active());
    assert!(!session.is_demuxing_active());
    assert!(!session.can_present_video());
    assert!(
        session
            .take_events()
            .contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Scrubbing))
    );

    session
        .dispatch_command(PlayerCommand::UpdateScrub(request))
        .unwrap();
    assert!(session.snapshot().timeline.scrubbing);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(3))
    );

    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .unwrap();
    assert!(!session.snapshot().timeline.scrubbing);
}

/// Вход в public Scrubbing замораживает audio output, а release без target восстанавливает Playing.
#[test]
fn scrubbing_freezes_audio_and_release_without_target_resumes_playing_output() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Audio)]);
    let audio_output_handle = install_ready_audio_runtime(&mut session, 20.0, None);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    assert_eq!(audio_output_handle.play_count.load(Ordering::Relaxed), 1);

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();

    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);
    assert_eq!(audio_output_handle.pause_count.load(Ordering::Relaxed), 1);

    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .unwrap();

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(audio_output_handle.play_count.load(Ordering::Relaxed), 2);
}

/// Выход из active Scrubbing инвалидирует in-flight scrub output через playback generation.
#[test]
fn end_scrub_without_target_advances_generation_for_resume_safety() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let generation_before_scrub = session.pipeline.seek_generation();
    let queued_frame =
        decoded_frame_for_current_seek_generation(&session, Duration::from_millis(40), 40);
    session.pipeline.enqueue_queued_video_frame(queued_frame);

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .unwrap();

    assert_eq!(
        session.pipeline.seek_generation(),
        generation_before_scrub + 1
    );
    assert!(
        !session
            .pipeline
            .packet_generation_is_current(generation_before_scrub)
    );
    assert!(session.pipeline.video_present_queue_is_empty());
    assert!(!session.snapshot().timeline.scrubbing);
    assert!(session.seek_commit().is_none());
}

/// Play во время active scrub сначала отменяет scrub, но не коммитит latest target.
#[test]
fn play_during_scrub_cancels_without_hidden_seek_commit() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    let _ = session.take_events();

    session.dispatch_command(PlayerCommand::Play).unwrap();

    assert!(!session.simple_scrub_active_for_tests());
    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.snapshot().timeline.target_position, None);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .len(),
        1
    );
    let events = session.take_events();
    assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)));
    assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekRequested(_)))
    );
}

/// Pause во время active scrub тоже закрывает scrub без release-seek-а.
#[test]
fn pause_during_scrub_cancels_without_hidden_seek_commit() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    let _ = session.take_events();

    session.dispatch_command(PlayerCommand::Pause).unwrap();

    assert!(!session.simple_scrub_active_for_tests());
    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .len(),
        1
    );
    let events = session.take_events();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekRequested(_)))
    );
}

/// Toggle во время scrub считается от последнего подтверждённого state до drag-а.
#[test]
fn toggle_during_scrub_uses_last_confirmed_playback_state() {
    let mut playing_session = PlayerSession::new();
    install_fake_media(&mut playing_session, vec![fake_track(1, TrackKind::Video)]);
    playing_session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    playing_session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    playing_session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    playing_session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .unwrap();

    assert!(!playing_session.simple_scrub_active_for_tests());
    assert_eq!(
        playing_session.snapshot().playback_state,
        PlaybackState::Paused
    );

    let mut paused_session = PlayerSession::new();
    install_fake_media(&mut paused_session, vec![fake_track(1, TrackKind::Video)]);
    paused_session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    paused_session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    paused_session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .unwrap();

    assert!(!paused_session.simple_scrub_active_for_tests());
    assert_eq!(
        paused_session.snapshot().playback_state,
        PlaybackState::Playing
    );
}

/// Ordinary Seek во время scrub отменяет latest preview target и идёт по SeekLanding route.
#[test]
fn seek_during_scrub_cancels_latest_target_before_one_shot_landing_route() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let preview_request = SeekRequest::absolute(MediaTime::from_secs(6));
    let external_seek_request = SeekRequest::absolute(MediaTime::from_secs(2));

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(preview_request))
        .unwrap();
    let _ = session.take_events();

    session
        .dispatch_command(PlayerCommand::Seek(external_seek_request))
        .unwrap();

    assert!(!session.simple_scrub_active_for_tests());
    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(
        session.snapshot().timeline.preview_state,
        media_core::TimelinePreviewState::Pending
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);
    assert!(session.seek_commit().is_some());

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].timestamp, Duration::from_secs(6));
    assert_eq!(requests[1].timestamp, Duration::from_secs(2));

    let events = session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekRequested(seek_request) if *seek_request == external_seek_request
    )));
    assert!(events.iter().all(|event| !matches!(
        event,
        PlayerEvent::SeekRequested(seek_request) if *seek_request == preview_request
    )));
}

#[test]
fn default_timeline_release_remains_commit_visible_preview() {
    assert_eq!(
        ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ScrubCommitPolicy::CommitVisiblePreview
    );
}

/// Проверяет, что release после simple UpdateScrub запускает тот же SeekLanding route.
#[test]
fn default_release_after_update_commits_latest_target_without_visible_preview() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let _ = session.take_events();
    let request = SeekRequest::absolute(MediaTime::from_secs(7));

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(request))
        .unwrap();

    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.stale_frame);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(7))
    );
    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );

    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(7));
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    let seek_commit = session
        .seek_commit()
        .expect("EndScrub должен открыть one-shot SeekLanding fallback");
    assert_eq!(seek_commit.target_position, MediaTime::from_secs(7));
    assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(6_900)));
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(
        session.snapshot().timeline.preview_state,
        media_core::TimelinePreviewState::Pending
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);

    let events = session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekRequested(seek_request) if *seek_request == request
    )));
}

/// Cold SeekLanding не очищает последний подтверждённый кадр, пока exact frame декодируется.
#[test]
fn cold_seek_landing_keeps_old_confirmed_frame_visible_while_pending() {
    let mut session = PlayerSession::new();
    let _seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(3), 300));
    session
        .pipeline
        .set_media_clock_base(Duration::from_secs(3));
    session
        .snapshot
        .set_timeline_position(MediaTime::from_secs(3));
    let request = SeekRequest::absolute(MediaTime::from_secs(11));

    session
        .dispatch_command(PlayerCommand::Seek(request))
        .unwrap();

    assert_eq!(
        session.pipeline.present_video_frame_pts(),
        Some(Duration::from_secs(3))
    );
    assert!(session.snapshot().timeline.scrubbing);
    assert!(session.snapshot().timeline.stale_frame);
    assert_eq!(
        session.snapshot().timeline.preview_state,
        media_core::TimelinePreviewState::Pending
    );
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(11))
    );
    assert!(session.seek_commit().is_some());
    let diagnostics = session
        .snapshot_with_frame_counters(FrameCounters::default())
        .diagnostics
        .frame_server_scrub;
    assert_eq!(diagnostics.outcomes.decode_point_seeked, 1);
    let scrub_events = session.take_scrub_events();
    assert!(
        scrub_events
            .iter()
            .any(|event| matches!(event, frame_server_core::ScrubEvent::Started(_)))
    );
    assert!(scrub_events.iter().any(|event| matches!(
        event,
        frame_server_core::ScrubEvent::Progress(progress)
            if progress.progress.target_status
                == frame_server_core::ScrubTargetReachStatus::BeforeTarget
    )));
    assert!(
        scrub_events
            .iter()
            .any(|event| matches!(event, frame_server_core::ScrubEvent::ResumePending(_)))
    );
}

/// PreviewScrub запускает live reused-decoder route, но не ordinary Seek command.
#[test]
fn preview_scrub_starts_live_route_without_ordinary_seek_event() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let _ = session.take_events();

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(8));
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    drop(requests);
    assert!(session.seek_commit().is_some());
    assert!(session.snapshot().timeline.scrubbing);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(8))
    );
    assert!(!session.snapshot().timeline.stale_frame);
    let events = session.take_events();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekRequested(_)))
    );
}

/// Live scrub diagnostics проходят через command boundary и переживают supersede.
#[test]
fn live_scrub_command_diagnostics_are_attached_to_events_and_supersede_cancel() {
    let mut session = PlayerSession::new();
    install_fake_media_with_seek_request_log(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let first_diagnostics = live_scrub_diagnostics_for_tests();
    let changed_settings =
        live_scrub_settings_for_tests(frame_server_core::LiveScrubDecodeMode::EveryDragEvent, 120);
    let mut latest_diagnostics = first_diagnostics;
    latest_diagnostics.record_throttled_latest_skip();
    latest_diagnostics.record_deferred_settings_change(
        frame_server_core::DeferredLiveScrubSettingsChange {
            old_snapshot: first_diagnostics.settings_snapshot,
            new_snapshot: changed_settings,
        },
    );

    session
        .dispatch_command(PlayerCommand::begin_live_scrub(first_diagnostics))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_live_scrub(
            SeekRequest::absolute(MediaTime::from_secs(8)),
            first_diagnostics,
        ))
        .unwrap();
    let _ = session.take_scrub_events();

    // Цель назад: forward extension неприменим, replacement идёт cold-маршрутом
    // с diagnostics-only supersede cancel.
    session
        .dispatch_command(PlayerCommand::preview_live_scrub(
            SeekRequest::absolute(MediaTime::from_secs(5)),
            latest_diagnostics,
        ))
        .unwrap();
    let events = session.take_scrub_events();

    let supersede_cancel = events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::Cancelled(cancelled)
                if cancelled.reason
                    == frame_server_core::CancelScrubReason::SupersededByNewTarget =>
            {
                Some(cancelled)
            }
            _ => None,
        })
        .expect("replacement target must publish diagnostics-only supersede cancel");
    assert_eq!(
        supersede_cancel.diagnostics.live_scrub,
        Some(first_diagnostics)
    );

    let replacement_started = events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::Started(started)
                if started.context.target().media_time == MediaTime::from_secs(5) =>
            {
                Some(started)
            }
            _ => None,
        })
        .expect("replacement target must publish started event");
    assert_eq!(
        replacement_started.diagnostics.live_scrub,
        Some(latest_diagnostics)
    );
    assert_eq!(latest_diagnostics.throttled_latest_skip_count, 1);
    assert_eq!(
        latest_diagnostics.deferred_live_scrub_settings_change_count,
        1
    );
}

/// EndScrub release разрешает commit active live route-а без второго demux seek.
#[test]
fn end_scrub_commits_active_live_preview_without_second_demux_seek() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let latest_request = SeekRequest::absolute(MediaTime::from_secs(9));

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(4),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(latest_request))
        .unwrap();
    let _ = session.take_events();
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(9));
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    let seek_commit = session
        .seek_commit()
        .expect("live preview target должен остаться active commit до gates");
    assert_eq!(seek_commit.target_position, MediaTime::from_secs(9));
    let events = session.take_events();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekRequested(_)))
    );
    assert!(events.iter().all(|event| !matches!(
        event,
        PlayerEvent::PlaybackStateChanged(PlaybackState::Seeking)
    )));
    assert!(events.iter().all(|event| !matches!(
        event,
        PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)
    )));
}

/// EndScrub без latest target только закрывает lightweight scrub state.
#[test]
fn end_scrub_without_latest_target_clears_simple_state_without_seek() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let _ = session.take_events();

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .unwrap();

    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
    assert!(session.seek_commit().is_none());
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.snapshot().timeline.target_position, None);
    let events = session.take_events();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekRequested(_)))
    );
}

/// ScrubCommitPolicy сохранён в API, но simple fallback временно его игнорирует.
#[test]
fn scrub_commit_policy_is_ignored_for_simple_fallback() {
    for policy in [
        ScrubCommitPolicy::CommitVisiblePreview,
        ScrubCommitPolicy::CommitLatestTarget,
    ] {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let latest_request = SeekRequest::absolute(MediaTime::from_secs(6));

        session
            .dispatch_command(PlayerCommand::begin_scrub())
            .unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(3),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::preview_scrub(latest_request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::end_scrub(policy))
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(6));
        assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
        let seek_commit = session
            .seek_commit()
            .expect("оба policy должны запускать одинаковый final seek");
        assert_eq!(seek_commit.target_position, MediaTime::from_secs(6));
        let _events = session.take_events();
    }
}

/// Media reset очищает сохранённый simple scrub target.
#[test]
fn reset_media_state_clears_simple_scrub_latest_request() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    assert!(session.simple_scrub_latest_request_for_tests().is_some());

    session.reset_media_state();

    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.snapshot().timeline.target_position, None);
}

/// Stop идёт через media reset и тоже закрывает compatibility scrub.
#[test]
fn stop_clears_simple_scrub_latest_request() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    assert!(session.simple_scrub_latest_request_for_tests().is_some());

    session.dispatch_command(PlayerCommand::Stop).unwrap();

    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Stopped);
    assert!(!session.snapshot().timeline.scrubbing);
}

/// Закрытие final seek очищает simple scrub target даже после внешней мутации test state.
#[test]
fn final_seek_completion_clears_simple_scrub_latest_request() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, Vec::new());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(7),
        )))
        .unwrap();
    assert!(session.seek_commit().is_some());
    session.set_simple_scrub_state_for_tests(
        false,
        Some(SeekRequest::absolute(MediaTime::from_secs(3))),
    );

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(7));
}

#[test]
fn end_scrub_preserves_latest_request_seek_mode() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(2, TrackKind::Audio)],
    );

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(8)),
            mode: SeekMode::KeyframeBefore,
        }))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
}

#[test]
fn preview_scrub_does_not_feed_scheduler_preroll_frame() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(6));
    drop(requests);
    assert_eq!(tick_result.decoded_video_frames, 0);
    assert_eq!(tick_result.video_frames_presented, 0);
    assert!(tick_result.dropped_video_frames.is_empty());
    assert!(session.pipeline.present_video_frame().is_none());
    assert!(!session.snapshot().timeline.stale_frame);
    assert!(session.seek_commit().is_some());
}

/// Прокат live scrub: pre-target кадры показываются latest-wins, не открывая commit gates.
#[test]
fn live_scrub_preroll_frames_present_progressively_without_opening_gates() {
    let video_track = fake_track(1, TrackKind::Video);
    let target = Duration::from_secs(8);
    let actual = Duration::from_millis(7_800);
    let mid_roll = Duration::from_millis(7_900);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target,
        actual,
        vec![
            fake_video_packet_with_keyframe(video_track.id, actual, PacketKeyframe::Keyframe),
            fake_video_packet_with_keyframe(video_track.id, mid_roll, PacketKeyframe::NotKeyframe),
            fake_video_packet_with_keyframe(video_track.id, target, PacketKeyframe::NotKeyframe),
        ],
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(target),
        )))
        .unwrap();
    let _ = harness.session.take_events();

    // Прокат активен только для LiveScrub route до первого landing frame.
    assert!(harness.session.active_seek_presents_preroll_progressively());
    // Pre-target кадры в прокате не считаются подавляемым preroll-ом.
    assert!(!harness.session.should_drop_decoded_frame_for_seek(actual));

    let first_tick = harness.tick_once_fast_preroll();
    assert_eq!(first_tick.demuxed_packets.len(), 3);

    // Первый декодированный кадр прохода (keyframe) показывается немедленно.
    harness.push_decoded_frame(actual, 79, 1);
    let keyframe_tick = harness.tick_once_fast_preroll();
    assert_eq!(keyframe_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    // Гейты закрыты: commit жив, публичное состояние остаётся Scrubbing.
    assert!(harness.session.seek_commit().is_some());
    assert!(harness.session.snapshot().timeline.scrubbing);
    assert!(!harness.session.snapshot().timeline.stale_frame);
    // Landing событие ещё не публиковалось: pre-target кадр не считается landing frame.
    let events = harness.session.take_events();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );

    // Следующий кадр прохода заменяет предыдущий (latest-wins).
    harness.push_decoded_frame(mid_roll, 80, 1);
    let roll_tick = harness.tick_once_fast_preroll();
    assert_eq!(roll_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(mid_roll)
    );
    assert!(harness.session.seek_commit().is_some());

    // Landing frame выключает прокат и идёт через обычный seek-путь.
    harness.push_decoded_frame(target, 81, 1);
    let landing_tick = harness.tick_once_fast_preroll();
    assert_eq!(landing_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(target)
    );
    assert!(!harness.session.active_seek_presents_preroll_progressively());
    // LiveScrub route не коммитится без EndScrub: состояние остаётся Scrubbing.
    assert!(harness.session.seek_commit().is_some());
    assert!(harness.session.snapshot().timeline.scrubbing);
    let events = harness.session.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );
}

/// Прокат latest-wins: несколько pre-target кадров в очереди показывают новейший,
/// старшие release-ятся как SeekPreroll и возвращают текстуры в пул.
#[test]
fn live_scrub_roll_presents_newest_queued_frame_and_releases_older() {
    let video_track = fake_track(1, TrackKind::Video);
    let target = Duration::from_secs(8);
    let actual = Duration::from_millis(7_700);
    let mid_roll = Duration::from_millis(7_800);
    let late_roll = Duration::from_millis(7_900);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target,
        actual,
        vec![
            fake_video_packet_with_keyframe(video_track.id, actual, PacketKeyframe::Keyframe),
            fake_video_packet_with_keyframe(video_track.id, mid_roll, PacketKeyframe::NotKeyframe),
            fake_video_packet_with_keyframe(video_track.id, late_roll, PacketKeyframe::NotKeyframe),
        ],
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(target),
        )))
        .unwrap();

    let _ = harness.tick_once_fast_preroll();

    // Три кадра прохода приходят до следующего tick-а: показан должен быть новейший.
    harness.push_decoded_frame(actual, 90, 1);
    harness.push_decoded_frame(mid_roll, 91, 1);
    harness.push_decoded_frame(late_roll, 92, 1);
    let roll_tick = harness.tick_once_fast_preroll();

    assert_eq!(roll_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(late_roll)
    );
    // Старшие кадры прохода released как seek preroll, а не застряли в очереди.
    assert!(
        roll_tick
            .dropped_video_frames
            .iter()
            .any(|drop| drop.reason == PlayerVideoDropReason::SeekPreroll)
    );
    assert!(harness.session.pipeline.video_present_queue_is_empty());
    assert!(harness.session.seek_commit().is_some());
}

/// Forward extension: цель вперёд в пределах капа продолжает активный decode-проход
/// без второго demux seek, сохраняя generation и pending video packets.
#[test]
fn live_scrub_forward_target_extends_active_pass_without_second_demux_seek() {
    let video_track = fake_track(1, TrackKind::Video);
    let first_target = Duration::from_secs(8);
    let extended_target = Duration::from_millis(8_500);
    let actual = Duration::from_millis(7_800);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        first_target,
        actual,
        vec![
            fake_video_packet_with_keyframe(video_track.id, actual, PacketKeyframe::Keyframe),
            fake_video_packet_with_keyframe(
                video_track.id,
                first_target,
                PacketKeyframe::NotKeyframe,
            ),
            fake_video_packet_with_keyframe(
                video_track.id,
                extended_target,
                PacketKeyframe::NotKeyframe,
            ),
        ],
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(first_target),
        )))
        .unwrap();
    let first_commit = harness.aligned_seek_commit();
    let _ = harness.tick_once_fast_preroll();
    let sent_after_first = harness.sent_packets().len();
    assert!(sent_after_first > 0);

    // Цель на +500мс вперёд: extension вместо supersede.
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(extended_target),
        )))
        .unwrap();

    // Demux seek остался один: extension не делает второй cold seek.
    assert_eq!(harness.seek_requests().len(), 1);
    let extended_commit = harness.aligned_seek_commit();
    assert_eq!(extended_commit.generation, first_commit.generation);
    assert_eq!(
        extended_commit.target_position,
        MediaTime::from_duration(extended_target)
    );
    assert_eq!(
        harness.session.snapshot().timeline.target_position,
        Some(MediaTime::from_duration(extended_target))
    );
    assert!(harness.session.snapshot().timeline.scrubbing);

    // Кадр старой цели теперь pre-target кадр проката: показывается, но не landing.
    harness.push_decoded_frame(first_target, 70, 1);
    let roll_tick = harness.tick_once_fast_preroll();
    assert_eq!(roll_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(first_target)
    );
    assert!(harness.session.seek_commit().is_some());
    let events = harness.session.take_events();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );

    // Landing расширенной цели снова публикует SeekTargetFramePresented для UI gate.
    harness.push_decoded_frame(extended_target, 71, 1);
    let landing_tick = harness.tick_once_fast_preroll();
    assert_eq!(landing_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(extended_target)
    );
    let events = harness.session.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekTargetFramePresented(_)))
    );
}

/// Forward extension не применяется к цели назад и к прыжку дальше капа.
#[test]
fn live_scrub_backward_or_far_forward_target_takes_cold_route() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(10),
        )))
        .unwrap();

    // Назад: cold route со вторым demux seek.
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .len(),
        2
    );

    // Далеко вперёд (за капом): тоже cold route.
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(20),
        )))
        .unwrap();
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .len(),
        3
    );

    // Вперёд в пределах капа: extension, без нового demux seek.
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(21),
        )))
        .unwrap();
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .len(),
        3
    );
}
