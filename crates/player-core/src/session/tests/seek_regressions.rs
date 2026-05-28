use super::test_support::*;
use super::*;

#[test]
fn regression_ordinary_final_seek_closes_after_first_decode_safe_frame_from_scripted_demux() {
    let video_track = fake_track(1, TrackKind::Video);
    let target = Duration::from_secs(8);
    let actual = Duration::from_millis(7_900);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target,
        actual,
        vec![
            fake_video_packet_with_keyframe(video_track.id, actual, PacketKeyframe::Keyframe),
            fake_video_packet_with_keyframe(video_track.id, target, PacketKeyframe::NotKeyframe),
        ],
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target));
    let accepted_seek = harness.aligned_seek_commit();
    assert_eq!(
        accepted_seek.target_position,
        MediaTime::from_duration(target)
    );
    assert_eq!(
        accepted_seek.actual_position,
        MediaTime::from_duration(actual)
    );
    assert!(harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.scrubbing);

    let first_tick = harness.tick_once();
    assert_eq!(first_tick.demuxed_packets.len(), 1);
    assert_eq!(harness.sent_packets().len(), 1);
    assert_eq!(harness.sent_packets()[0].pts, actual);
    assert_eq!(
        harness.sent_packets()[0].generation,
        accepted_seek.generation
    );

    harness.push_decoded_frame(actual, 79, 1);
    let instant_tick = harness.tick_once();
    assert_eq!(instant_tick.demuxed_packets.len(), 1);
    assert_eq!(instant_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.snapshot().current_position, actual);
    assert_eq!(
        harness.session.snapshot().timeline.current_position,
        MediaTime::from_duration(actual)
    );
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.scrubbing);
    assert!(!harness.session.snapshot().timeline.stale_frame);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    assert_eq!(
        harness.session.snapshot().playback_state,
        PlaybackState::Paused
    );
    assert_eq!(harness.seek_requests()[0].timestamp, target);
    assert_eq!(
        harness.seek_requests()[0].mode,
        DemuxSeekMode::DecodePointBefore
    );
}

#[test]
fn ordinary_final_seek_reanchors_audio_clock_base_to_actual_demux_point() {
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let target = Duration::from_secs(6);
    let actual = Duration::from_secs(5);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target,
        actual,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
    let _audio_handle = install_ready_audio_runtime(&mut harness.session, 0.0, None);

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness.start_final_seek(MediaTime::from_duration(target));

    let accepted_seek = harness.aligned_seek_commit();
    assert_eq!(
        accepted_seek.actual_position,
        MediaTime::from_duration(actual)
    );
    assert_eq!(harness.session.pipeline.media_clock_base(), actual);
    assert_eq!(
        harness
            .session
            .presentation_clock_position_at(Instant::now()),
        actual
    );
}

#[test]
fn final_seek_without_audio_clock_uses_actual_override_before_audio_gate() {
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let target = Duration::from_secs(6);
    let actual = Duration::from_secs(5);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target,
        actual,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
    let tick_config = PlayerTickConfig {
        max_demux_packets_per_tick: 0,
        max_decoded_video_frames_drained_per_tick: 3,
        seek_resume_audio_gate_timeout: Duration::from_secs(10),
        ..seek_admission_tick_config(4, 4)
    };

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness.start_final_seek(MediaTime::from_duration(target));

    assert!(!harness.session.pipeline.has_audio_clock());
    assert_eq!(
        harness.session.seek_presentation_clock_override(),
        Some(actual)
    );
    assert_eq!(
        harness
            .session
            .presentation_clock_position_at(Instant::now()),
        actual
    );

    harness.push_decoded_frame(actual, 500, 0);
    harness.push_decoded_frame(actual + Duration::from_millis(16), 516, 0);
    harness.push_decoded_frame(actual + Duration::from_millis(33), 533, 0);
    let first_tick = harness
        .session
        .tick(PlayerTickContext::with_config(Instant::now(), tick_config));

    assert_eq!(first_tick.video_frames_presented, 1);
    assert!(first_tick.dropped_video_frames.is_empty());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    assert_eq!(harness.session.pipeline.video_present_queue_len(), 2);
    assert!(harness.session.seek_commit().is_some());
}

#[test]
fn final_seek_audio_gate_wait_keeps_preroll_frames_on_actual_clock_base() {
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let target = Duration::from_secs(6);
    let actual = Duration::from_secs(5);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target,
        actual,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
    let audio_handle = install_ready_audio_runtime(&mut harness.session, 0.0, None);
    let tick_config = PlayerTickConfig {
        max_demux_packets_per_tick: 0,
        max_decoded_video_frames_drained_per_tick: 3,
        seek_resume_audio_gate_timeout: Duration::from_secs(10),
        ..seek_admission_tick_config(4, 4)
    };

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness.start_final_seek(MediaTime::from_duration(target));
    assert_eq!(harness.session.pipeline.media_clock_base(), actual);

    harness.push_decoded_frame(actual, 500, 0);
    harness.push_decoded_frame(actual + Duration::from_millis(16), 516, 0);
    harness.push_decoded_frame(actual + Duration::from_millis(33), 533, 0);
    let first_tick = harness
        .session
        .tick(PlayerTickContext::with_config(Instant::now(), tick_config));

    assert_eq!(first_tick.video_frames_presented, 1);
    assert!(first_tick.dropped_video_frames.is_empty());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    assert_eq!(harness.session.pipeline.video_present_queue_len(), 2);
    assert!(harness.session.seek_commit().is_some());

    audio_handle.clock.record_played(48_000 * 2 * 20 / 1_000);
    let second_tick = harness
        .session
        .tick(PlayerTickContext::with_config(Instant::now(), tick_config));

    assert_eq!(second_tick.video_frames_presented, 1);
    assert!(second_tick.dropped_video_frames.is_empty());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual + Duration::from_millis(16))
    );
    assert_eq!(harness.session.pipeline.video_present_queue_len(), 1);

    let soft_fallback_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            seek_resume_audio_gate_timeout: Duration::ZERO,
            ..tick_config
        },
    ));

    assert_eq!(soft_fallback_tick.video_frames_presented, 0);
    assert!(soft_fallback_tick.dropped_video_frames.is_empty());
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.pipeline.media_clock_base(), actual);
    assert_eq!(harness.session.snapshot().current_position, actual);
}

#[test]
fn regression_symphonia_reset_required_event_rebases_generation_and_seek_completes() {
    let initial_track = fake_track(1, TrackKind::Video);
    let reset_track = fake_track(3, TrackKind::Video);
    let target = Duration::from_secs(8);
    let actual = Duration::from_millis(7_900);
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(
        vec![initial_track.clone()],
        Some(Duration::from_secs(30)),
        seek_log,
    )
    .with_seek_request_log(Arc::clone(&seek_request_log))
    .with_seek_result(scripted_seek_result(target, actual));
    demuxer.push_event(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
        vec![reset_track.clone()],
        Some(Duration::from_secs(30)),
    )));
    demuxer.push_event(DemuxReadEvent::Packet(fake_video_packet_with_keyframe(
        reset_track.id,
        actual,
        PacketKeyframe::Keyframe,
    )));
    demuxer.push_event(DemuxReadEvent::Packet(fake_video_packet_with_keyframe(
        reset_track.id,
        target,
        PacketKeyframe::NotKeyframe,
    )));
    let mut harness = SeekRegressionHarness::new(vec![initial_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target));
    let generation_before_reset = harness.aligned_seek_commit().generation;

    let reset_tick = harness.tick_once();
    let seek_after_reset = harness.aligned_seek_commit();
    assert_eq!(reset_tick.demuxed_packets.len(), 0);
    assert_ne!(seek_after_reset.generation, generation_before_reset);
    assert!(harness.sent_packets().is_empty());
    assert_eq!(
        seek_after_reset.target_position,
        MediaTime::from_duration(target)
    );
    assert_eq!(
        seek_after_reset.actual_position,
        MediaTime::from_duration(actual)
    );

    let post_reset_packet_tick = harness.tick_once();
    let sent_after_reset = harness.sent_packets();
    assert_eq!(post_reset_packet_tick.demuxed_packets.len(), 1);
    assert_eq!(sent_after_reset.len(), 1);
    assert_eq!(sent_after_reset[0].track_id, reset_track.id);
    assert_eq!(sent_after_reset[0].generation, seek_after_reset.generation);

    harness.push_decoded_frame(actual, 179, 1);
    let instant_tick = harness.tick_once();

    assert_eq!(instant_tick.video_frames_presented, 1);
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.snapshot().current_position, actual);
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.stale_frame);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    assert_eq!(
        seek_request_log
            .lock()
            .expect("reset request log lock")
            .as_slice(),
        &[DemuxSeekRequest::decode_point_before(target)]
    );
}

#[test]
fn regression_seek_near_eof_commits_first_decode_safe_frame_without_eof_wait() {
    let video_track = fake_track(1, TrackKind::Video);
    let target = Duration::from_millis(29_500);
    let actual = Duration::from_secs(29);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target,
        actual,
        vec![fake_video_packet_with_keyframe(
            video_track.id,
            actual,
            PacketKeyframe::Keyframe,
        )],
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target));
    harness.tick_once();
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            actual,
            290,
        ));

    let instant_tick = harness.tick_once();
    assert_eq!(instant_tick.video_frames_presented, 1);
    assert!(
        !harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    assert_eq!(harness.session.snapshot().current_position, actual);
    assert_eq!(harness.session.pipeline.media_clock_base(), actual);
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.stale_frame);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(actual)
    );
    let events = harness.session.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::SeekCommitted(commit)
            if commit.target_position == target
                && commit.actual_position == actual
                && commit.resume_intent == PlaybackResumeIntent::Pause
    )));
}

#[test]
fn regression_preview_scrub_does_not_mark_target_ready_without_seek() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let request = SeekRequest::absolute(MediaTime::from_secs(8));

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(request))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(request))
        .unwrap();
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(8), 790));
    session.note_presented_frame_for_seek(Duration::from_secs(8));

    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
    assert!(session.seek_commit().is_none());
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
}

#[test]
fn regression_drag_release_uses_ordinary_final_seek_even_with_visible_frame() {
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
    );
    let request = SeekRequest::absolute(MediaTime::from_secs(8));

    session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
    session
        .dispatch_command(PlayerCommand::UpdateScrub(request))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::PreviewScrub(request))
        .unwrap();
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(8), 790));

    session
        .dispatch_command(PlayerCommand::EndScrub {
            policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        })
        .unwrap();

    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(
        requests.as_slice(),
        &[DemuxSeekRequest::decode_point_before(Duration::from_secs(
            8
        ))]
    );
    assert!(session.seek_commit().is_some());
    assert!(!session.snapshot().timeline.scrubbing);
    assert!(session.snapshot().timeline.seeking);
}

#[test]
fn regression_audio_gate_blockers_keep_distinct_status_and_diagnostics() {
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);
    let gate_cases = [
        (
            AudioSeekRuntimeState::NoSelectedAudio,
            seek_commit.generation,
            Some(0.0),
            SeekAudioGateStatus::Ready,
            None,
        ),
        (
            AudioSeekRuntimeState::WaitingForDecoder,
            seek_commit.generation,
            Some(100.0),
            SeekAudioGateStatus::WaitingForDecoder,
            Some(SeekProgressBlocker::WaitingForAudioDecoder),
        ),
        (
            AudioSeekRuntimeState::WaitingForOutput,
            seek_commit.generation,
            Some(100.0),
            SeekAudioGateStatus::WaitingForOutput,
            Some(SeekProgressBlocker::WaitingForAudioOutput),
        ),
        (
            AudioSeekRuntimeState::Ready,
            seek_commit.generation,
            Some(49.0),
            SeekAudioGateStatus::WaitingForPreroll,
            Some(SeekProgressBlocker::WaitingForAudioPreroll),
        ),
        (
            AudioSeekRuntimeState::Ready,
            seek_commit.generation - 1,
            Some(100.0),
            SeekAudioGateStatus::WaitingForClear,
            Some(SeekProgressBlocker::WaitingForAudioClear),
        ),
    ];

    for (runtime_state, clear_generation, buffered_ms, expected_status, expected_blocker) in
        gate_cases
    {
        let gate_status = classify_seek_audio_gate(
            seek_commit,
            runtime_state,
            clear_generation,
            buffered_ms,
            50.0,
        );
        assert_eq!(gate_status, expected_status);
        assert_eq!(gate_status.blocker(), expected_blocker);
    }

    let disabled_after_error_status = classify_seek_audio_gate(
        seek_commit,
        AudioSeekRuntimeState::NoSelectedAudio,
        seek_commit.generation,
        None,
        50.0,
    );
    assert_eq!(disabled_after_error_status, SeekAudioGateStatus::Ready);
    assert_eq!(disabled_after_error_status.blocker(), None);

    let mut disabled_audio_session = PlayerSession::new();
    install_fake_media(
        &mut disabled_audio_session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    disabled_audio_session.disable_selected_audio_path();

    assert_eq!(
        disabled_audio_session.pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::NoSelectedAudio
    );
    assert_eq!(
        disabled_audio_session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::Ready
    );
}

#[test]
fn regression_active_seek_diagnostics_reports_audio_blocker_when_video_is_ready() {
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
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(5),
            500,
        ));
    session.note_presented_frame_for_seek(Duration::from_secs(5));
    let seek_commit = session
        .seek_commit()
        .expect("audio gate должен удерживать seek после target frame");

    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at + Duration::from_millis(1),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );
    let diagnostics = session
        .active_seek_diagnostics(Instant::now(), &seek_regression_tick_config())
        .expect("active seek diagnostics должны быть доступны пока audio gate блокирует");

    assert!(session.seek_commit().is_some());
    assert_eq!(
        diagnostics.blocker,
        SeekProgressBlocker::WaitingForAudioDecoder
    );
    assert_eq!(diagnostics.generation, session.pipeline.seek_generation());
    assert!(diagnostics.target_frame_presented);
    assert!(diagnostics.video_gate_ready);
    assert!(!diagnostics.audio_gate_ready);
}
