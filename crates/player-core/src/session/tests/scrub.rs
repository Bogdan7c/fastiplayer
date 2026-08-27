use super::test_support::*;
use super::*;

const SCRUB_COMMAND_CORRELATION_TRACE_TEST_NAME: &str = concat!(
    "session::tests::scrub::",
    "scrub_dispatch_tracing_pairs_exact_monotonic_ids_without_consuming_non_scrub_ids"
);
const INACTIVE_END_SCRUB_TRACE_TEST_NAME: &str = concat!(
    "session::tests::scrub::",
    "inactive_end_scrub_public_dispatch_emits_exact_info_pair_and_outcome"
);
const FAILED_SCRUB_TRACE_TEST_NAME: &str = concat!(
    "session::tests::scrub::",
    "failed_scrub_public_dispatch_emits_pairs_without_reusing_command_id"
);

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
        landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        started_at: Instant::now(),
        public_accepted_at: Instant::now(),
        resume_intent: PlaybackResumeIntent::Pause,
        target_retention: crate::seek_state::SeekTargetRetention::ExactPublicRange,
    }));

    session
        .end_scrub(ScrubCommitPolicy::CommitLatestTarget, None)
        .unwrap();

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

    let outcome = session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .unwrap();
    assert_eq!(
        outcome,
        crate::PlayerCommandOutcome::ScrubCommit(
            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget {
                target: MediaTime::from_secs(3),
                reason: VisibleScrubPreviewUnavailableReason::Missing,
            }
        )
    );
    assert!(!session.snapshot().timeline.scrubbing);
}

#[test]
fn scrub_dispatch_tracing_pairs_exact_monotonic_ids_without_consuming_non_scrub_ids() {
    match super::tracing_capture::isolate_tracing_capture_test(
        SCRUB_COMMAND_CORRELATION_TRACE_TEST_NAME,
    ) {
        super::tracing_capture::IsolatedTracingTestProcess::ParentCompleted => return,
        super::tracing_capture::IsolatedTracingTestProcess::ChildRunsBody => {}
    }
    let (captured_tracing, _tracing_guard) = super::tracing_capture::install_info_tracing_capture();
    let mut session = PlayerSession::new();
    let absolute_request = SeekRequest::absolute(MediaTime::from_millis(3_550));
    let relative_request = SeekRequest::relative(Duration::from_millis(750));

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::SetVolume(0.75))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(absolute_request))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub {
            request: absolute_request,
            live_scrub: None,
        })
        .unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub {
            request: absolute_request,
            live_scrub: None,
        })
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(relative_request))
        .unwrap();
    let _end_outcome = session.dispatch_command(PlayerCommand::end_scrub(
        ScrubCommitPolicy::CommitLatestTarget,
    ));

    let trace = captured_tracing.contents();
    let scrub_rows = trace
        .lines()
        .filter(|line| line.contains("scrub_schema_version=1"))
        .collect::<Vec<_>>();
    assert_eq!(
        scrub_rows.len(),
        12,
        "шесть real scrub commands обязаны дать две INFO forms под default filter: {trace}"
    );

    for command_id in 1..=6 {
        let identity = format!("scrub_command_id={command_id}");
        let identity_rows = scrub_rows
            .iter()
            .filter(|line| line.split_whitespace().any(|field| field == identity))
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            identity_rows.len(),
            2,
            "каждый ID обязан встретиться ровно в двух forms: {trace}"
        );
        assert!(identity_rows.iter().any(|line| {
            line.contains("level=INFO") && line.contains("scrub_command_form=dispatch")
        }));
        assert!(identity_rows.iter().any(|line| {
            line.contains("level=INFO")
                && line.contains("kind=seek_acceptance")
                && line.contains("scrub_command_form=acceptance")
        }));
    }

    let identical_preview_rows = scrub_rows
        .iter()
        .filter(|line| {
            line.contains("scrub_stage=preview")
                && line.contains("scrub_target_kind=absolute")
                && line.contains("scrub_requested_target_ms=3550")
        })
        .count();
    assert_eq!(
        identical_preview_rows, 4,
        "две одинаковые preview commands обязаны сохранить два разных exact ID"
    );
    assert_eq!(
        scrub_rows
            .iter()
            .filter(|line| {
                line.contains("scrub_stage=update")
                    && line.contains("scrub_target_kind=relative")
                    && line.contains("scrub_requested_target_ms=750")
            })
            .count(),
        2,
        "relative target identity должна совпадать в двух INFO forms"
    );
    assert!(
        trace.lines().all(|line| !line.contains("level=DEBUG")),
        "INFO subscriber не должен захватывать full DEBUG command: {trace}"
    );
}

#[test]
fn inactive_end_scrub_public_dispatch_emits_exact_info_pair_and_outcome() {
    match super::tracing_capture::isolate_tracing_capture_test(INACTIVE_END_SCRUB_TRACE_TEST_NAME) {
        super::tracing_capture::IsolatedTracingTestProcess::ParentCompleted => return,
        super::tracing_capture::IsolatedTracingTestProcess::ChildRunsBody => {}
    }
    let (captured_tracing, _tracing_guard) = super::tracing_capture::install_info_tracing_capture();
    let mut session = PlayerSession::new();

    let outcome = session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitLatestTarget,
        ))
        .expect("inactive EndScrub должен вернуть typed public outcome");

    assert_eq!(
        outcome,
        PlayerCommandOutcome::ScrubCommit(ScrubCommitOutcome::NoActiveGesture)
    );
    let trace = captured_tracing.contents();
    let correlation_rows = trace
        .lines()
        .filter(|line| line.contains("scrub_command_id=1"))
        .collect::<Vec<_>>();
    assert_eq!(correlation_rows.len(), 2, "inactive command pair: {trace}");
    assert!(correlation_rows.iter().all(|line| {
        line.contains("level=INFO")
            && line.contains("scrub_stage=end")
            && line.contains("scrub_target_kind=none")
    }));
    assert!(
        correlation_rows
            .iter()
            .any(|line| line.contains("scrub_command_form=dispatch"))
    );
    assert!(correlation_rows.iter().any(|line| {
        line.contains("scrub_command_form=acceptance") && line.contains("kind=seek_acceptance")
    }));
}

#[test]
fn failed_scrub_public_dispatch_emits_pairs_without_reusing_command_id() {
    match super::tracing_capture::isolate_tracing_capture_test(FAILED_SCRUB_TRACE_TEST_NAME) {
        super::tracing_capture::IsolatedTracingTestProcess::ParentCompleted => return,
        super::tracing_capture::IsolatedTracingTestProcess::ChildRunsBody => {}
    }
    let (captured_tracing, _tracing_guard) = super::tracing_capture::install_info_tracing_capture();
    let mut session = PlayerSession::new();
    session
        .dispatch_command(PlayerCommand::Shutdown)
        .expect("fixture shutdown должен примениться");

    for _attempt in 0..2 {
        let error = session
            .dispatch_command(PlayerCommand::begin_scrub())
            .expect_err("scrub command после shutdown должна вернуть typed error");
        assert_eq!(error.kind, PlayerErrorKind::InvalidCommand);
    }

    let trace = captured_tracing.contents();
    let correlation_rows = trace
        .lines()
        .filter(|line| line.contains("scrub_schema_version=1"))
        .collect::<Vec<_>>();
    assert_eq!(
        correlation_rows.len(),
        4,
        "две failed commands обязаны сохранить две exact INFO pairs: {trace}"
    );
    for command_id in 1..=2 {
        let identity = format!("scrub_command_id={command_id}");
        let identity_rows = correlation_rows
            .iter()
            .filter(|line| line.split_whitespace().any(|field| field == identity))
            .collect::<Vec<_>>();
        assert_eq!(identity_rows.len(), 2, "failed command ID pair: {trace}");
        assert!(identity_rows.iter().any(|line| {
            line.contains("scrub_command_form=dispatch") && line.contains("scrub_stage=begin")
        }));
        assert!(identity_rows.iter().any(|line| {
            line.contains("scrub_command_form=acceptance") && line.contains("kind=seek_acceptance")
        }));
    }
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

/// Fallback сохраняет typed policy resolution и существующую decoder error semantics.
#[test]
fn visible_preview_fallback_flush_error_does_not_hide_driver_failure() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session
        .pipeline
        .set_video_decoder_thread(FailingFlushVideoDecoderThread::new("flush failed"));
    let initial_generation = session.pipeline.seek_generation();
    let target = MediaTime::from_secs(5);

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(target)))
        .unwrap();
    let outcome = session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();

    assert_eq!(
        outcome,
        crate::PlayerCommandOutcome::ScrubCommit(
            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget {
                target,
                reason: VisibleScrubPreviewUnavailableReason::Missing,
            }
        )
    );
    assert!(
        seek_log
            .lock()
            .expect("seek log mutex should not be poisoned")
            .is_empty()
    );
    assert_eq!(session.pipeline.seek_generation(), initial_generation);
    assert!(session.seek_commit().is_none());
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

/// Missing visible preview использует active latest route без второго demux seek.
#[test]
fn end_scrub_missing_visible_preview_falls_back_without_second_demux_seek() {
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
    let outcome = session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();
    assert_eq!(
        outcome,
        crate::PlayerCommandOutcome::ScrubCommit(
            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget {
                target: MediaTime::from_secs(9),
                reason: VisibleScrubPreviewUnavailableReason::Missing,
            }
        )
    );

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

/// Simple route различает latest policy и typed visible-preview fallback.
#[test]
fn simple_scrub_reports_policy_specific_resolution() {
    for (policy, expected_outcome) in [
        (
            ScrubCommitPolicy::CommitVisiblePreview,
            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget {
                target: MediaTime::from_secs(6),
                reason: VisibleScrubPreviewUnavailableReason::Missing,
            },
        ),
        (
            ScrubCommitPolicy::CommitLatestTarget,
            ScrubCommitOutcome::LatestTarget {
                target: MediaTime::from_secs(6),
            },
        ),
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
            .dispatch_command(PlayerCommand::UpdateScrub(latest_request))
            .unwrap();
        let outcome = session
            .dispatch_command(PlayerCommand::end_scrub(policy))
            .unwrap();

        assert_eq!(
            outcome,
            crate::PlayerCommandOutcome::ScrubCommit(expected_outcome)
        );
        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(6));
        assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
        let seek_commit = session
            .seek_commit()
            .expect("policy resolution должен открыть exact final seek");
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

/// Строит active live-scrub route и показывает pre-target frame через real scheduler path.
fn live_scrub_harness_with_visible_preview() -> SeekRegressionHarness {
    let video_track = fake_track(1, TrackKind::Video);
    let target = Duration::from_secs(8);
    let actual = Duration::from_millis(7_700);
    let visible = Duration::from_millis(7_900);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target,
        actual,
        vec![
            fake_video_packet_with_keyframe(video_track.id, actual, PacketKeyframe::Keyframe),
            fake_video_packet_with_keyframe(video_track.id, visible, PacketKeyframe::NotKeyframe),
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
    harness.push_decoded_frame(visible, 120, 1);
    let presented_tick = harness.tick_once_fast_preroll();
    assert_eq!(presented_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(visible)
    );

    harness
}

/// Переводит готовый scrub harness в live mode через session-owned install boundary.
fn install_live_timeline_on_scrub_harness(
    harness: &mut SeekRegressionHarness,
    generation: u64,
    range_start: MediaTime,
    range_end: MediaTime,
) {
    let non_zero_generation =
        std::num::NonZeroU64::new(generation).expect("test generation must be non-zero");
    let media_instance_id = crate::MediaInstanceId::from_non_zero(non_zero_generation);
    let port_generation = media_core::DynamicMediaTimelinePortGeneration::new(non_zero_generation);
    let seekable_range =
        media_core::TimelineRange::new(range_start, range_end).expect("ordered live test range");
    let state = media_core::DynamicMediaTimelineState::with_dvr(range_end, seekable_range)
        .expect("valid live test state");
    let (port, _publisher) =
        media_core::dynamic_media_timeline(media_core::DynamicMediaTimelineInitial {
            port_generation,
            source_epoch: media_core::DynamicMediaTimelineEpoch::new(1),
            state,
        });

    harness.session.set_snapshot_duration(None);
    harness.session.snapshot.media_instance_id = Some(media_instance_id);
    harness.session.install_timeline_mode(
        media_instance_id,
        crate::PreparedMediaTimelineMode::Live { port },
    );
}

/// Valid visible preview коммитится по frame timing, а не по более новой pointer target.
#[test]
fn commit_visible_preview_and_latest_target_produce_different_exact_targets() {
    let latest_target = MediaTime::from_secs(10);

    let mut visible_harness = live_scrub_harness_with_visible_preview();
    visible_harness
        .session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            latest_target,
        )))
        .unwrap();
    let visible_identity = visible_harness
        .session
        .current_present_frame_identity()
        .expect("scheduler уже показал stable live-scrub frame");
    let visible_outcome = visible_harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();

    assert!(matches!(
        visible_outcome,
        crate::PlayerCommandOutcome::ScrubCommit(ScrubCommitOutcome::VisiblePreview {
            timing,
            frame_identity,
        }) if timing.media_time == MediaTime::from_millis(7_900)
            && frame_identity == visible_identity
    ));
    assert_eq!(
        visible_harness
            .session
            .seek_commit()
            .expect("visible preview должен открыть exact SeekLanding")
            .target_position,
        MediaTime::from_millis(7_900)
    );

    let mut latest_harness = live_scrub_harness_with_visible_preview();
    latest_harness
        .session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            latest_target,
        )))
        .unwrap();
    let latest_outcome = latest_harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitLatestTarget,
        ))
        .unwrap();

    assert_eq!(
        latest_outcome,
        crate::PlayerCommandOutcome::ScrubCommit(ScrubCommitOutcome::LatestTarget {
            target: latest_target,
        })
    );
    assert_eq!(
        latest_harness
            .session
            .seek_commit()
            .expect("latest policy должен открыть exact SeekLanding")
            .target_position,
        latest_target
    );
}

#[test]
fn sliding_live_window_expires_active_scrub_route_even_if_latest_pointer_remains_inside() {
    let mut harness = live_scrub_harness_with_visible_preview();
    harness
        .session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(10),
        )))
        .expect("latest pointer remains inside the future DVR range");

    install_live_timeline_on_scrub_harness(
        &mut harness,
        71,
        MediaTime::from_secs(9),
        MediaTime::from_secs(12),
    );

    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness
            .session
            .snapshot()
            .last_error
            .as_ref()
            .expect("expired active scrub route records a typed recoverable error")
            .kind,
        PlayerErrorKind::SeekTargetExpired
    );
}

#[test]
fn visible_preview_outside_latest_live_range_falls_back_to_valid_pointer_target() {
    let latest_target = MediaTime::from_secs(10);
    let mut harness = live_scrub_harness_with_visible_preview();
    harness
        .session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            latest_target,
        )))
        .expect("latest pointer remains inside the future DVR range");
    install_live_timeline_on_scrub_harness(
        &mut harness,
        72,
        MediaTime::from_secs(8),
        MediaTime::from_secs(12),
    );

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .expect("expired visible preview falls back to the valid pointer target");

    assert!(matches!(
        outcome,
        crate::PlayerCommandOutcome::ScrubCommit(
            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget {
                target,
                reason:
                    VisibleScrubPreviewUnavailableReason::OutsideLatestLiveRange {
                        preview_position,
                        available_range: Some(_),
                    },
            }
        ) if target == latest_target && preview_position == MediaTime::from_millis(7_900)
    ));
    assert_eq!(
        harness
            .session
            .seek_commit()
            .expect("fallback opens the existing exact SeekLanding route")
            .target_position,
        latest_target
    );
}

/// Superseded nested generation запрещает commit ранее показанного frame-а.
#[test]
fn stale_visible_preview_generation_falls_back_to_latest_target() {
    let mut harness = live_scrub_harness_with_visible_preview();
    let latest_target = MediaTime::from_secs(12);
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            latest_target,
        )))
        .unwrap();

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();

    assert!(matches!(
        outcome,
        crate::PlayerCommandOutcome::ScrubCommit(
            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget {
                target,
                reason: VisibleScrubPreviewUnavailableReason::StaleContext(_),
            }
        ) if target == latest_target
    ));
    assert_eq!(
        harness
            .session
            .seek_commit()
            .expect("stale preview fallback должен сохранить active exact latest route")
            .target_position,
        latest_target
    );
}

/// Source/backend/track switch guards отклоняют stable identity до commit-а.
#[test]
fn visible_preview_rejects_source_backend_and_track_switches() {
    #[derive(Debug, Clone, Copy)]
    enum GuardMutation {
        Source,
        Backend,
        Track,
    }

    for mutation in [
        GuardMutation::Source,
        GuardMutation::Backend,
        GuardMutation::Track,
    ] {
        let mut harness = live_scrub_harness_with_visible_preview();
        let seek_commit = harness
            .session
            .seek_commit()
            .expect("live scrub должен держать active commit");
        let current_context = harness
            .session
            .active_seek_landing_context(seek_commit)
            .expect("live scrub должен иметь active context");
        let visible_preview = harness
            .session
            .seek_runtime
            .visible_scrub_preview()
            .expect("scheduler должен сохранить visible preview");

        let (source_revision, backend_revision, track_selection) = match mutation {
            GuardMutation::Source => (
                frame_server_core::SourceRevision::new(1),
                current_context.backend_revision(),
                current_context.track_selection(),
            ),
            GuardMutation::Backend => (
                current_context.source_revision(),
                frame_server_core::BackendRevision::new(1),
                current_context.track_selection(),
            ),
            GuardMutation::Track => (
                current_context.source_revision(),
                current_context.backend_revision(),
                frame_server_core::ScrubTrackSelection::video_only(TrackId::new(99)),
            ),
        };
        let mutated_context = frame_server_core::ScrubTargetContext::new(
            source_revision,
            backend_revision,
            track_selection,
            current_context.target(),
            current_context.exactness_policy(),
            current_context.request_kind(),
            current_context.generation(),
        );
        harness.session.seek_runtime.note_visible_scrub_preview(
            crate::seek_state::VisibleScrubPreview {
                context: mutated_context,
                ..visible_preview
            },
        );

        let outcome = harness
            .session
            .dispatch_command(PlayerCommand::end_scrub(
                ScrubCommitPolicy::CommitVisiblePreview,
            ))
            .unwrap();

        let crate::PlayerCommandOutcome::ScrubCommit(
            ScrubCommitOutcome::VisiblePreviewUnavailableFallbackToLatestTarget { target, reason },
        ) = outcome
        else {
            panic!("{mutation:?} должен вернуть typed latest-target fallback: {outcome:?}");
        };
        assert_eq!(target, MediaTime::from_secs(8));
        match mutation {
            GuardMutation::Source => assert!(matches!(
                reason,
                VisibleScrubPreviewUnavailableReason::StaleContext(
                    frame_server_core::ScrubStaleReason::SourceRevisionMismatch { .. }
                )
            )),
            GuardMutation::Backend => assert!(matches!(
                reason,
                VisibleScrubPreviewUnavailableReason::StaleContext(
                    frame_server_core::ScrubStaleReason::BackendRevisionMismatch { .. }
                )
            )),
            GuardMutation::Track => assert!(matches!(
                reason,
                VisibleScrubPreviewUnavailableReason::TrackSelectionChanged { .. }
            )),
        }
        assert_eq!(
            harness
                .session
                .seek_commit()
                .expect("guard fallback должен сохранить exact latest route")
                .target_position,
            MediaTime::from_secs(8)
        );
    }
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
