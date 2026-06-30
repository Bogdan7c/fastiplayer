use super::test_support::*;
use super::*;

use super::super::prepared_seek::{
    PreparedSeekBranchResumePendingReason, PreparedSeekBranchToken,
    PreparedSeekLandingOverrideHandoff, PreparedSeekLandingPromotionKind,
    PreparedSeekLandingReleaseOutcome, VideoResumeRunwayState,
};
use super::super::timeline_hover_prepare_handoff::PlayerTimelineHoverPrepareHandoff;

struct RecordingPreparedReleaseSink {
    released: Arc<Mutex<Vec<video_core::FrameResourceHandle>>>,
}

impl video_present_core::VideoFrameReleaseSink for RecordingPreparedReleaseSink {
    fn release_frame(
        &self,
        release: video_present_core::VideoFrameRelease,
    ) -> video_present_core::VideoFrameReleaseOutcome {
        self.released
            .lock()
            .expect("prepared release storage mutex must not be poisoned")
            .push(release.resource_handle());
        video_present_core::VideoFrameReleaseOutcome::Accepted
    }
}

fn prepared_timestamp(millis: u64) -> media_core::TrackTimestamp {
    media_core::TrackTimestamp::new(
        media_core::TrackId::new(1),
        i64::try_from(millis).expect("test timestamp fits into i64"),
        media_core::TimeBase::new(1, 1_000).expect("valid millisecond timebase"),
    )
}

fn prepared_media_time(millis: u64) -> MediaTime {
    MediaTime::from_millis(millis)
}

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

fn prepared_lease_for_tests(
    session: &PlayerSession,
    pts_millis: u64,
    resource_handle: u64,
    released: Arc<Mutex<Vec<video_core::FrameResourceHandle>>>,
) -> video_present_core::VideoFrameLease {
    let mut frame = decoded_frame_for_tests(Duration::from_millis(pts_millis), resource_handle);
    frame.generation = session.pipeline.seek_generation();

    video_present_core::VideoFrameLease::new(video_present_core::VideoFrameLeaseConfig::new(
        session.pipeline.render_generation(),
        frame,
        Arc::new(RecordingPreparedReleaseSink { released }),
    ))
}

fn insert_prepared_seek_frame_for_tests(
    session: &mut PlayerSession,
    target_millis: u64,
    actual_millis: u64,
    resource_handle: u64,
    branch_token: Option<PreparedSeekBranchToken>,
    released: Arc<Mutex<Vec<video_core::FrameResourceHandle>>>,
) {
    let lease = prepared_lease_for_tests(session, actual_millis, resource_handle, released);
    session.insert_prepared_seek_landing_frame_for_tests(
        prepared_media_time(target_millis),
        prepared_timestamp(actual_millis),
        lease,
        branch_token,
    );
}

fn take_prepared_override_lease(
    session: &mut PlayerSession,
) -> video_present_core::VideoFrameLease {
    match session.take_prepared_seek_landing_override_handoff() {
        Some(PreparedSeekLandingOverrideHandoff::Publish(lease)) => lease,
        Some(PreparedSeekLandingOverrideHandoff::Clear) => {
            panic!("prepared hit must publish an override lease before clear")
        }
        None => panic!("prepared hit must publish an override lease"),
    }
}

fn take_prepared_override_clear(session: &mut PlayerSession) {
    match session.take_prepared_seek_landing_override_handoff() {
        Some(PreparedSeekLandingOverrideHandoff::Clear) => {}
        Some(PreparedSeekLandingOverrideHandoff::Publish(_lease)) => {
            panic!("prepared replacement must clear the old override before publishing a new one")
        }
        None => panic!("prepared replacement must clear the old override"),
    }
}

fn release_count(
    released: &Arc<Mutex<Vec<video_core::FrameResourceHandle>>>,
    resource_handle: u64,
) -> usize {
    released
        .lock()
        .expect("prepared release storage mutex must not be poisoned")
        .iter()
        .filter(|handle| **handle == video_core::FrameResourceHandle(resource_handle))
        .count()
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

#[test]
fn prepared_frame_only_seek_landing_publishes_override_without_cold_decode() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        11_000,
        11_000,
        80,
        None,
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();

    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        0
    );
    assert!(!session.seek_landing_decode_active());
    assert!(session.seek_commit().is_some());
    assert_eq!(
        session.active_prepared_seek_landing_kind_for_tests(),
        Some(
            PreparedSeekLandingPromotionKind::VisualOverrideResumePending {
                reason: PreparedSeekBranchResumePendingReason::FrameOnly,
            }
        )
    );
    assert_eq!(session.prepared_seek_landing_working_set_len_for_tests(), 0);

    let scrub_events = session.take_scrub_events();
    assert!(matches!(
        scrub_events.as_slice(),
        [
            frame_server_core::ScrubEvent::Started(_),
            frame_server_core::ScrubEvent::PreviewFrameReady(_),
            frame_server_core::ScrubEvent::ResumePending(_),
        ]
    ));
    let preview = scrub_events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::PreviewFrameReady(preview) => Some(preview),
            _ => None,
        })
        .expect("prepared hit must publish PreviewFrameReady");
    assert_eq!(preview.frame.timing.pts, prepared_timestamp(11_000));
    assert_eq!(
        preview.frame.resource.resource_handle(),
        video_core::FrameResourceHandle(80)
    );

    let override_lease = take_prepared_override_lease(&mut session);
    assert_eq!(
        override_lease.resource_handle(),
        video_core::FrameResourceHandle(80)
    );
    assert!(
        session
            .take_prepared_seek_landing_override_handoff()
            .is_none()
    );
    assert_eq!(release_count(&released, 80), 0);

    drop(override_lease);
    assert_eq!(release_count(&released, 80), 0);
    session.set_seek_commit_for_tests(None);
    assert_eq!(release_count(&released, 80), 1);
}

#[test]
fn timeline_hover_prepare_snapshot_marks_one_shot_resume_pending() {
    let mut session = PlayerSession::new();
    install_fake_media_with_seek_request_log(&mut session, vec![fake_track(1, TrackKind::Video)]);
    insert_prepared_seek_frame_for_tests(
        &mut session,
        11_000,
        11_000,
        81,
        None,
        Arc::new(Mutex::new(Vec::new())),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();

    let snapshot = session.snapshot_with_frame_counters(FrameCounters::default());
    let prepare_snapshot = snapshot
        .timeline_hover_prepare
        .expect("loaded video media must expose hover prepare guards");

    assert_eq!(
        prepare_snapshot.interaction(),
        crate::TimelineHoverPrepareInteraction::OneShotSeekLandingResumePending {
            spare_capacity_available: false,
        }
    );
}

#[test]
fn timeline_hover_prepare_snapshot_marks_live_scrub_on_pointer_down() {
    let mut session = PlayerSession::new();
    install_fake_media_with_seek_request_log(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();

    let snapshot = session.snapshot_with_frame_counters(FrameCounters::default());
    let prepare_snapshot = snapshot
        .timeline_hover_prepare
        .expect("loaded video media must expose hover prepare guards");

    assert_eq!(
        prepare_snapshot.interaction(),
        crate::TimelineHoverPrepareInteraction::LiveScrubActive
    );
}

#[test]
fn prepared_timing_rejection_falls_back_to_cold_decode_without_promoting() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        11_000,
        10_900,
        81,
        Some(PreparedSeekBranchToken::resume_ready_for_tests()),
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();

    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        1
    );
    assert!(session.seek_landing_decode_active());
    assert_eq!(session.active_prepared_seek_landing_kind_for_tests(), None);
    assert_eq!(session.prepared_seek_landing_working_set_len_for_tests(), 1);
    assert!(
        session
            .take_prepared_seek_landing_override_handoff()
            .is_none()
    );
    assert_eq!(release_count(&released, 81), 0);
}

#[test]
fn prepared_progress_runway_states_are_fail_closed_resume_pending() {
    for (runway, target_millis, handle) in [
        (VideoResumeRunwayState::Pending, 12_000, 82_u64),
        (VideoResumeRunwayState::Repositioned, 13_000, 83_u64),
        (
            VideoResumeRunwayState::PostTargetPacketAccepted,
            14_000,
            84_u64,
        ),
    ] {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let released = Arc::new(Mutex::new(Vec::new()));
        insert_prepared_seek_frame_for_tests(
            &mut session,
            target_millis,
            target_millis,
            handle,
            Some(PreparedSeekBranchToken::with_video_runway_for_tests(runway)),
            Arc::clone(&released),
        );
        let _events_before_seek = session.take_events();

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                prepared_media_time(target_millis),
            )))
            .unwrap();

        assert_eq!(
            seek_request_log
                .lock()
                .expect("seek request log mutex must not be poisoned")
                .len(),
            0
        );
        assert_eq!(
            session.active_prepared_seek_landing_kind_for_tests(),
            Some(
                PreparedSeekLandingPromotionKind::VisualOverrideResumePending {
                    reason: PreparedSeekBranchResumePendingReason::RunwayPending,
                }
            )
        );
        assert!(
            session
                .take_scrub_events()
                .iter()
                .any(|event| matches!(event, frame_server_core::ScrubEvent::ResumePending(_)))
        );
        assert_eq!(release_count(&released, handle), 0);
    }
}

#[test]
fn prepared_commit_ready_video_runway_without_audio_commits_atomically() {
    for (runway, target_millis, handle) in [
        (
            VideoResumeRunwayState::DisplayableFrameQueued,
            15_000,
            85_u64,
        ),
        (VideoResumeRunwayState::NextFrameAlmostReady, 16_000, 86_u64),
    ] {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let released = Arc::new(Mutex::new(Vec::new()));
        insert_prepared_seek_frame_for_tests(
            &mut session,
            target_millis,
            target_millis,
            handle,
            Some(PreparedSeekBranchToken::with_video_runway_for_tests(runway)),
            Arc::clone(&released),
        );
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                prepared_media_time(target_millis),
            )))
            .unwrap();

        assert_eq!(
            seek_request_log
                .lock()
                .expect("seek request log mutex must not be poisoned")
                .len(),
            0
        );
        assert_eq!(session.active_prepared_seek_landing_kind_for_tests(), None);
        assert!(!session.snapshot().timeline.scrubbing);
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(session.seek_commit().is_none());
        assert!(!session.seek_runtime.seek_landing_active());
        assert!(!session.take_events().iter().any(|event| matches!(
            event,
            PlayerEvent::PlaybackStateChanged(PlaybackState::Scrubbing)
        )));
        let scrub_events = session.take_scrub_events();
        assert!(matches!(
            scrub_events.as_slice(),
            [
                frame_server_core::ScrubEvent::Started(_),
                frame_server_core::ScrubEvent::PreviewFrameReady(_),
                frame_server_core::ScrubEvent::Committed(_),
            ]
        ));
        assert_eq!(release_count(&released, handle), 1);
    }
}

#[test]
fn prepared_commit_ready_video_runway_waits_for_active_audio() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        13_000,
        13_000,
        87,
        Some(PreparedSeekBranchToken::with_video_runway_for_tests(
            VideoResumeRunwayState::DisplayableFrameQueued,
        )),
        Arc::clone(&released),
    );
    session.dispatch_command(PlayerCommand::Play).unwrap();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(13_000),
        )))
        .unwrap();

    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        0
    );
    assert_eq!(
        session.active_prepared_seek_landing_kind_for_tests(),
        Some(PreparedSeekLandingPromotionKind::ResumeReadyBranch)
    );
    assert!(session.snapshot().timeline.scrubbing);
    assert!(session.seek_commit().is_some());
    let scrub_events = session.take_scrub_events();
    assert!(
        scrub_events
            .iter()
            .any(|event| matches!(event, frame_server_core::ScrubEvent::ResumePending(_)))
    );
    assert!(
        scrub_events
            .iter()
            .all(|event| !matches!(event, frame_server_core::ScrubEvent::Committed(_)))
    );
    assert_eq!(release_count(&released, 87), 0);
}

#[test]
fn shared_timeline_hover_handoff_promotes_same_prepared_branch() {
    let handoff = PlayerTimelineHoverPrepareHandoff::default();
    let mut session = PlayerSession::with_audio_factories_and_timeline_hover_prepare_handoff(
        crate::audio_boundary::missing_audio_decoder_factory(),
        crate::audio_boundary::missing_audio_output_factory(),
        handoff.clone(),
        frame_server_core::FrameServerConfig::default()
            .validate()
            .expect("default frame-server config must validate"),
    );
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let released = Arc::new(Mutex::new(Vec::new()));
    let lease = prepared_lease_for_tests(&session, 14_000, 114, Arc::clone(&released));
    let target_position = prepared_media_time(14_000);
    let key = session.prepared_seek_landing_frame_key_for_tests(target_position);

    handoff.insert_prepared_frame_for_tests(
        key,
        lease,
        prepared_timestamp(14_000),
        Some(PreparedSeekBranchToken::resume_ready_for_tests()),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(target_position)))
        .unwrap();

    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        0
    );
    let scrub_events = session.take_scrub_events();
    assert!(matches!(
        scrub_events.as_slice(),
        [
            frame_server_core::ScrubEvent::Started(_),
            frame_server_core::ScrubEvent::PreviewFrameReady(_),
            frame_server_core::ScrubEvent::Committed(_),
        ]
    ));
    assert_eq!(session.active_prepared_seek_landing_kind_for_tests(), None);
    assert_eq!(session.prepared_seek_landing_working_set_len_for_tests(), 0);
    assert_eq!(release_count(&released, 114), 1);
}

#[test]
fn prepared_seek_supersede_promotes_new_target_immediately() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        11_000,
        11_000,
        91,
        None,
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();

    let old_events = session.take_scrub_events();
    let old_resume_pending = old_events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::ResumePending(pending) => Some(pending),
            _ => None,
        })
        .expect("old prepared target must enter resume_pending");
    assert_eq!(
        old_resume_pending.context.target().media_time,
        prepared_media_time(11_000)
    );
    assert_eq!(
        session.prepared_seek_landing_recent_superseded_len_for_tests(),
        0
    );

    insert_prepared_seek_frame_for_tests(
        &mut session,
        12_000,
        12_000,
        92,
        Some(PreparedSeekBranchToken::resume_ready_for_tests()),
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(12_000),
        )))
        .unwrap();

    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        0
    );
    assert!(session.seek_commit().is_none());
    assert_eq!(session.active_prepared_seek_landing_kind_for_tests(), None);
    assert_eq!(
        session.prepared_seek_landing_recent_superseded_len_for_tests(),
        1
    );
    assert_eq!(release_count(&released, 91), 0);
    assert_eq!(release_count(&released, 92), 1);

    let replacement_events = session.take_scrub_events();
    let cancelled = replacement_events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::Cancelled(cancelled) => Some(cancelled),
            _ => None,
        })
        .expect("supersede must publish diagnostics-only cancellation");
    let new_preview = replacement_events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::PreviewFrameReady(preview) => Some(preview),
            _ => None,
        })
        .expect("new prepared target must publish preview immediately");

    assert_eq!(
        cancelled.reason,
        frame_server_core::CancelScrubReason::SupersededByNewTarget
    );
    assert_eq!(
        cancelled.context.target().media_time,
        prepared_media_time(11_000)
    );
    assert_eq!(
        new_preview.context.target().media_time,
        prepared_media_time(12_000)
    );
    assert_eq!(
        new_preview.context.generation().scrub_generation.get(),
        old_resume_pending
            .context
            .generation()
            .scrub_generation
            .get()
            + 1
    );
    assert_eq!(
        new_preview.context.generation().playback_generation,
        old_resume_pending.context.generation().playback_generation
    );
    assert!(replacement_events.iter().any(|event| matches!(
        event,
        frame_server_core::ScrubEvent::Committed(committed)
            if committed.context.target().media_time == prepared_media_time(12_000)
    )));
    assert!(replacement_events.iter().all(|event| !matches!(
        event,
        frame_server_core::ScrubEvent::Committed(committed)
            if committed.context.target().media_time == prepared_media_time(11_000)
    )));
    assert!(
        replacement_events
            .iter()
            .all(|event| !matches!(event, frame_server_core::ScrubEvent::Failed(_)))
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();

    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        0
    );
    assert_eq!(
        session.active_prepared_seek_landing_kind_for_tests(),
        Some(
            PreparedSeekLandingPromotionKind::VisualOverrideResumePending {
                reason: PreparedSeekBranchResumePendingReason::FrameOnly,
            }
        )
    );
    assert_eq!(
        session.prepared_seek_landing_recent_superseded_len_for_tests(),
        0
    );
    assert_eq!(release_count(&released, 91), 0);
    let click_back_events = session.take_scrub_events();
    let click_back_preview = click_back_events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::PreviewFrameReady(preview) => Some(preview),
            _ => None,
        })
        .expect("click-back target must promote from recent-superseded");
    assert_eq!(
        click_back_preview.context.target().media_time,
        prepared_media_time(11_000)
    );
    assert_eq!(
        click_back_preview.context.generation().playback_generation,
        old_resume_pending.context.generation().playback_generation
    );
}

#[test]
fn cold_seek_supersede_clears_old_override_and_keeps_confirmed_frame() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
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
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        11_000,
        11_000,
        93,
        None,
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();
    let old_override = take_prepared_override_lease(&mut session);
    let old_events = session.take_scrub_events();
    let old_resume_pending = old_events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::ResumePending(pending) => Some(pending),
            _ => None,
        })
        .expect("old prepared target must enter resume_pending");

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(12_000),
        )))
        .unwrap();

    take_prepared_override_clear(&mut session);
    assert_eq!(
        session.pipeline.present_video_frame_pts(),
        Some(Duration::from_secs(3))
    );
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(prepared_media_time(12_000))
    );
    assert!(session.snapshot().timeline.scrubbing);
    assert!(session.snapshot().timeline.stale_frame);
    assert!(session.seek_landing_decode_active());
    assert_eq!(session.active_prepared_seek_landing_kind_for_tests(), None);
    assert_eq!(
        session.prepared_seek_landing_recent_superseded_len_for_tests(),
        1
    );
    assert_eq!(release_count(&released, 93), 0);
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        1
    );

    let replacement_events = session.take_scrub_events();
    assert!(replacement_events.iter().any(|event| matches!(
        event,
        frame_server_core::ScrubEvent::Cancelled(cancelled)
            if cancelled.reason == frame_server_core::CancelScrubReason::SupersededByNewTarget
    )));
    let new_resume_pending = replacement_events
        .iter()
        .find_map(|event| match event {
            frame_server_core::ScrubEvent::ResumePending(pending)
                if pending.context.target().media_time == prepared_media_time(12_000) =>
            {
                Some(pending)
            }
            _ => None,
        })
        .expect("cold replacement target must enter resume_pending");
    assert_eq!(
        new_resume_pending.context.generation().playback_generation,
        old_resume_pending.context.generation().playback_generation
    );
    assert_eq!(
        new_resume_pending
            .context
            .generation()
            .scrub_generation
            .get(),
        old_resume_pending
            .context
            .generation()
            .scrub_generation
            .get()
            + 1
    );

    drop(old_override);
    assert_eq!(release_count(&released, 93), 0);
}

#[test]
fn prepared_demote_release_reports_typed_accept_and_reject() {
    let mut accepted_session = PlayerSession::new();
    install_fake_media_with_seek_request_log(
        &mut accepted_session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let accepted_released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut accepted_session,
        11_000,
        11_000,
        94,
        None,
        Arc::clone(&accepted_released),
    );
    accepted_session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();
    let accepted_commit = accepted_session
        .seek_commit()
        .expect("prepared hit must create a pending commit");
    let accepted_context = accepted_session
        .active_seek_landing_context(accepted_commit)
        .expect("prepared hit must expose active context");

    let accepted_outcome = accepted_session.release_prepared_seek_landing_for_cancel(
        frame_server_core::CancelScrubReason::SupersededByNewTarget,
        Some(accepted_context),
    );

    assert_eq!(
        accepted_outcome,
        PreparedSeekLandingReleaseOutcome::DemotedToRecentSuperseded
    );
    assert_eq!(
        accepted_session.prepared_seek_landing_recent_superseded_len_for_tests(),
        1
    );
    assert_eq!(release_count(&accepted_released, 94), 0);

    let mut rejected_session = PlayerSession::new();
    install_fake_media_with_seek_request_log(
        &mut rejected_session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let rejected_released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut rejected_session,
        13_000,
        13_000,
        95,
        None,
        Arc::clone(&rejected_released),
    );
    rejected_session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(13_000),
        )))
        .unwrap();
    let rejected_commit = rejected_session
        .seek_commit()
        .expect("prepared hit must create a pending commit");
    let rejected_context = rejected_session
        .active_seek_landing_context(rejected_commit)
        .expect("prepared hit must expose active context");
    let mismatched_context = frame_server_core::ScrubTargetContext::new(
        rejected_context.source_revision(),
        rejected_context.backend_revision(),
        rejected_context.track_selection(),
        frame_server_core::ScrubTarget::new(
            prepared_media_time(14_000),
            prepared_timestamp(14_000),
        ),
        rejected_context.exactness_policy(),
        rejected_context.request_kind(),
        rejected_context.generation(),
    );

    let rejected_outcome = rejected_session.release_prepared_seek_landing_for_cancel(
        frame_server_core::CancelScrubReason::SupersededByNewTarget,
        Some(mismatched_context),
    );

    assert!(matches!(
        rejected_outcome,
        PreparedSeekLandingReleaseOutcome::DemoteRejected {
            reason:
                frame_server_core::TimelineHoverPrepareDemoteBackRejection::PromotedKeyNotCurrent { .. },
        }
    ));
    assert_eq!(
        rejected_session.prepared_seek_landing_recent_superseded_len_for_tests(),
        0
    );
    assert_eq!(release_count(&rejected_released, 95), 1);
}

#[test]
fn prepared_audio_timeout_fails_closed_without_video_only_fallback() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(3), 300));
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        13_000,
        13_000,
        88,
        Some(PreparedSeekBranchToken::with_video_runway_for_tests(
            VideoResumeRunwayState::DisplayableFrameQueued,
        )),
        Arc::clone(&released),
    );
    session.dispatch_command(PlayerCommand::Play).unwrap();
    let _events_before_seek = session.take_events();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(13_000),
        )))
        .unwrap();

    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log mutex must not be poisoned")
            .len(),
        0
    );
    let seek_commit = session
        .seek_commit()
        .expect("prepared hit должен ждать active audio gate");
    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);
    assert_eq!(release_count(&released, 88), 0);

    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at + Duration::from_millis(250),
        Duration::from_millis(250),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.active_prepared_seek_landing_kind_for_tests(), None);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.snapshot().timeline.target_position, None);
    assert_eq!(
        session.snapshot().timeline.preview_state,
        media_core::TimelinePreviewState::Failed
    );
    assert_eq!(session.pipeline.media_clock_base(), Duration::from_secs(3));
    assert_eq!(
        session.pipeline.present_video_frame_pts(),
        Some(Duration::from_secs(3))
    );
    assert_eq!(release_count(&released, 88), 1);

    let scrub_events = session.take_scrub_events();
    assert!(scrub_events.iter().any(|event| matches!(
        event,
        frame_server_core::ScrubEvent::Failed(failed)
            if failed.reason == frame_server_core::ScrubFailureReason::AudioResumeTimedOut
                && failed.diagnostics.driver_outcome
                    == frame_server_core::ScrubDriverOutcomeKind::AudioResumeTimedOut
    )));
    assert!(scrub_events.iter().all(|event| !matches!(
        event,
        frame_server_core::ScrubEvent::Failed(failed)
            if failed.diagnostics.driver_outcome
                == frame_server_core::ScrubDriverOutcomeKind::AudioResumeFailed
    )));
    assert!(session.take_events().iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::SeekTimeout
                && error.message.contains("audio_timing_unknown_fallback=true")
    )));
}

#[test]
fn promoted_prepared_resource_leaves_hover_cleanup_and_releases_once() {
    let mut session = PlayerSession::new();
    install_fake_media_with_seek_request_log(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        14_000,
        14_000,
        84,
        None,
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            prepared_media_time(14_000),
        )))
        .unwrap();

    assert_eq!(session.prepared_seek_landing_working_set_len_for_tests(), 0);
    insert_prepared_seek_frame_for_tests(
        &mut session,
        15_000,
        15_000,
        85,
        None,
        Arc::clone(&released),
    );
    insert_prepared_seek_frame_for_tests(
        &mut session,
        16_000,
        16_000,
        86,
        None,
        Arc::clone(&released),
    );

    assert_eq!(release_count(&released, 84), 0);
    assert_eq!(release_count(&released, 85), 1);
    assert_eq!(release_count(&released, 86), 0);

    session.set_seek_commit_for_tests(None);
    assert_eq!(release_count(&released, 84), 1);
    session.set_seek_commit_for_tests(None);
    assert_eq!(release_count(&released, 84), 1);
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
            SeekRequest::absolute(MediaTime::from_secs(5)),
            first_diagnostics,
        ))
        .unwrap();
    let _ = session.take_scrub_events();

    session
        .dispatch_command(PlayerCommand::preview_live_scrub(
            SeekRequest::absolute(MediaTime::from_secs(8)),
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
                if started.context.target().media_time == MediaTime::from_secs(8) =>
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

/// Live scrub переносит prepared branch в transaction ownership без cold decode.
#[test]
fn live_scrub_prepared_transfer_waits_for_release_and_retarget_releases_without_demote() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        11_000,
        11_000,
        140,
        Some(PreparedSeekBranchToken::resume_ready_for_tests()),
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            prepared_media_time(11_000),
        )))
        .unwrap();

    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
    assert_eq!(
        session.active_prepared_seek_landing_kind_for_tests(),
        Some(PreparedSeekLandingPromotionKind::ResumeReadyBranch)
    );
    assert!(session.seek_commit().is_some());
    assert_ne!(
        session.snapshot().current_position,
        prepared_media_time(11_000).as_duration()
    );
    assert!(
        session
            .take_scrub_events()
            .iter()
            .all(|event| !matches!(event, frame_server_core::ScrubEvent::Committed(_)))
    );

    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitVisiblePreview,
        ))
        .unwrap();
    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );
    assert_eq!(
        session.snapshot().current_position,
        prepared_media_time(11_000).as_duration()
    );

    insert_prepared_seek_frame_for_tests(
        &mut session,
        12_000,
        12_000,
        141,
        Some(PreparedSeekBranchToken::resume_ready_for_tests()),
        Arc::clone(&released),
    );
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            prepared_media_time(12_000),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            prepared_media_time(13_000),
        )))
        .unwrap();

    assert_eq!(release_count(&released, 141), 1);
    assert_eq!(
        session.prepared_seek_landing_recent_superseded_len_for_tests(),
        0
    );
}

/// Frame-only prepared entry не transfer-ится в LiveScrub как branch.
#[test]
fn live_scrub_frame_only_prepared_entry_releases_and_uses_cold_decode_path() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let released = Arc::new(Mutex::new(Vec::new()));
    insert_prepared_seek_frame_for_tests(
        &mut session,
        14_000,
        14_000,
        142,
        None,
        Arc::clone(&released),
    );

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            prepared_media_time(14_000),
        )))
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(14));
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    drop(requests);

    assert_eq!(session.active_prepared_seek_landing_kind_for_tests(), None);
    assert_eq!(release_count(&released, 142), 1);
    assert_eq!(
        session.prepared_seek_landing_recent_superseded_len_for_tests(),
        0
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
