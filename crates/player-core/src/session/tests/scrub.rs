use super::test_support::*;
use super::*;

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

    session.end_scrub().unwrap();

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

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
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
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
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

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();

    assert_eq!(session.snapshot().playback_state, PlaybackState::Scrubbing);
    assert_eq!(audio_output_handle.pause_count.load(Ordering::Relaxed), 1);

    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
        .unwrap();

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(audio_output_handle.play_count.load(Ordering::Relaxed), 2);
}

/// Play во время active scrub сначала отменяет scrub, но не коммитит latest target.
#[test]
fn play_during_scrub_cancels_without_hidden_seek_commit() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(SeekRequest::absolute(
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
    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
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
    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    let _ = session.take_events();

    session.dispatch_command(PlayerCommand::Pause).unwrap();

    assert!(!session.simple_scrub_active_for_tests());
    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
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
        .dispatch_command(PlayerCommand::BeginScrub)
        .unwrap();
    playing_session
        .dispatch_command(PlayerCommand::PreviewScrub(SeekRequest::absolute(
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
        .dispatch_command(PlayerCommand::BeginScrub)
        .unwrap();
    paused_session
        .dispatch_command(PlayerCommand::PreviewScrub(SeekRequest::absolute(
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

/// Ordinary Seek во время scrub отменяет latest preview target и идёт по обычному seek route.
#[test]
fn seek_during_scrub_cancels_latest_target_before_normal_seek_route() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let preview_request = SeekRequest::absolute(MediaTime::from_secs(6));
    let external_seek_request = SeekRequest::absolute(MediaTime::from_secs(2));

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(preview_request))
        .unwrap();
    let _ = session.take_events();

    session
        .dispatch_command(PlayerCommand::Seek(external_seek_request))
        .unwrap();

    assert!(!session.simple_scrub_active_for_tests());
    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(2));

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

/// Проверяет, что release после simple UpdateScrub запускает обычный final seek.
#[test]
fn default_release_after_update_commits_latest_target_without_visible_preview() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let _ = session.take_events();
    let request = SeekRequest::absolute(MediaTime::from_secs(7));

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
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
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(7));
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    let seek_commit = session
        .seek_commit()
        .expect("EndScrub должен открыть ordinary final seek");
    assert_eq!(seek_commit.target_position, MediaTime::from_secs(7));
    assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(6_900)));
    assert!(!session.snapshot().timeline.scrubbing);
    assert!(session.snapshot().timeline.seeking);

    let events = session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekRequested(seek_request) if *seek_request == request
    )));
}

/// PreviewScrub временно является latest-target update и не стартует demux preview.
#[test]
fn preview_scrub_is_latest_target_update_without_demux_seek() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let _ = session.take_events();

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .unwrap();

    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
    assert!(session.seek_commit().is_none());
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

/// EndScrub берёт target последнего PreviewScrub так же, как последнего UpdateScrub.
#[test]
fn preview_scrub_target_is_committed_by_end_scrub() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let latest_request = SeekRequest::absolute(MediaTime::from_secs(9));

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(4),
        )))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(latest_request))
        .unwrap();
    let _ = session.take_events();
    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::CommitVisiblePreview,
        })
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, Duration::from_secs(9));
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    let seek_commit = session
        .seek_commit()
        .expect("PreviewScrub latest target должен стать final seek");
    assert_eq!(seek_commit.target_position, MediaTime::from_secs(9));
    let events = session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekRequested(seek_request) if *seek_request == latest_request
    )));
    assert!(events.iter().any(|event| matches!(
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

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
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

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(3),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(latest_request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub { policy })
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

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
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

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(SeekRequest::absolute(
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

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(8)),
            mode: SeekMode::KeyframeBefore,
        }))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
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

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));

    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
    assert_eq!(tick_result.decoded_video_frames, 0);
    assert_eq!(tick_result.video_frames_presented, 0);
    assert!(tick_result.dropped_video_frames.is_empty());
    assert!(session.pipeline.present_video_frame().is_none());
    assert!(!session.snapshot().timeline.stale_frame);
    assert!(session.seek_commit().is_none());
}
