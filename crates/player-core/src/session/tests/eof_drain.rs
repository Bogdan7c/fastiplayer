use super::test_support::*;
use super::*;

#[test]
fn eof_during_seek_schedules_bounded_progress_instead_of_idle_sleep() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session.enter_eof_drain();

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, crate::WorkerWakeupReason::SeekOrPreroll);
    assert_eq!(plan.delay, Some(Duration::from_millis(250)));
}

#[test]
fn eof_drain_state_is_visible_in_snapshot() {
    let mut session = PlayerSession::new();

    session.enter_eof_drain();

    assert_eq!(session.playback_state(), PlaybackState::Draining);
    assert!(session.can_present_video());
}

#[test]
fn eof_drain_tick_does_not_read_new_demux_packets() {
    let mut session = PlayerSession::new();
    let tracks = vec![fake_track(1, TrackKind::Video)];
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let event_read_count = Arc::new(AtomicUsize::new(0));
    let mut demuxer = FakeDemuxer::new(
        tracks.clone(),
        Some(Duration::from_secs(30)),
        Arc::clone(&seek_log),
    )
    .with_event_read_count(Arc::clone(&event_read_count));
    demuxer.push_packet(fake_video_packet(
        TrackId::new(1),
        Duration::from_millis(100),
    ));

    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    session
        .select_default_video_track(&tracks, "fake EOF drain media содержит video track")
        .expect("fake EOF drain video track должен получить fresh decode requirement");
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.apply_demux_seekability(DemuxSeekability::Seekable);
    session.set_playback_state(PlaybackState::Playing);
    session.enter_eof_drain();

    let _tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 4,
            ..PlayerTickConfig::default()
        },
    ));

    assert_eq!(event_read_count.load(Ordering::Relaxed), 0);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert_eq!(session.playback_state(), PlaybackState::Ended);
}

#[test]
fn audio_only_eof_after_buffered_samples_keeps_bounded_wakeup_until_drain() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, 35.0, None);
    session.begin_autoplay_preroll().unwrap();

    session.enter_eof_drain();

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(2),
        Duration::from_millis(250),
    );
    assert_eq!(plan.reason, crate::WorkerWakeupReason::CoarseProgress);
    assert_eq!(plan.delay, Some(Duration::from_millis(250)));
    assert_eq!(session.playback_state(), PlaybackState::Draining);

    audio_output.set_buffer_level_ms(0.0);
    let _tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert!(session.pipeline.has_demuxer());
}

#[test]
fn eof_during_buffering_starts_buffered_audio_tail_once() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, 20.0, None);
    session.begin_autoplay_preroll().unwrap();
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 0);

    session.enter_eof_drain();
    session.start_eof_audio_tail_if_needed();

    assert_eq!(session.playback_state(), PlaybackState::Draining);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn eof_audio_tail_play_error_does_not_leave_drain_stuck() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, 20.0, Some("play failed"));
    session.begin_autoplay_preroll().unwrap();

    session.enter_eof_drain();
    let _tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
    assert!(!session.pipeline.has_audio_output());
    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert!(session.snapshot().last_error.is_some());
}

#[test]
fn eof_drain_transitions_to_ended_after_audio_tail_drains() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, 15.0, None);
    session.dispatch_command(PlayerCommand::Play).unwrap();

    session.enter_eof_drain();
    let _tick_result = session.tick(PlayerTickContext::new(Instant::now()));
    assert_eq!(session.playback_state(), PlaybackState::Draining);

    audio_output.set_buffer_level_ms(0.0);
    let _tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert!(session.pipeline.has_demuxer());
}

#[test]
fn eof_drain_keeps_moving_audio_tail_until_buffer_drains() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, 25.0, None);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();
    assert_eq!(session.playback_state(), PlaybackState::Draining);

    let now = Instant::now();
    let last_audio_progress_at = now
        .checked_sub(Duration::from_millis(500))
        .expect("test clock must have enough monotonic headroom");
    session
        .pipeline
        .reset_audio_clock_sample(Duration::ZERO, last_audio_progress_at);
    audio_output.clock.record_played(4_800);
    let tick_config = PlayerTickConfig {
        audio_stall_timeout: Duration::from_millis(250),
        ..PlayerTickConfig::default()
    };

    let _tick_result = session.tick(PlayerTickContext::with_config(now, tick_config));

    assert_eq!(session.playback_state(), PlaybackState::Draining);
    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
}

#[test]
fn eof_drain_finishes_when_audio_output_stalls_after_media_tail() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    let audio_output = install_ready_audio_runtime(&mut session, 25.0, None);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();
    assert_eq!(session.playback_state(), PlaybackState::Draining);

    let now = Instant::now();
    let last_audio_progress_at = now
        .checked_sub(Duration::from_millis(500))
        .expect("test clock must have enough monotonic headroom");
    session
        .pipeline
        .reset_audio_clock_sample(Duration::ZERO, last_audio_progress_at);
    let tick_config = PlayerTickConfig {
        audio_stall_timeout: Duration::from_millis(250),
        ..PlayerTickConfig::default()
    };

    let _tick_result = session.tick(PlayerTickContext::with_config(now, tick_config));

    assert_eq!(audio_output.play_count.load(Ordering::Relaxed), 1);
    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(30));
    assert!(session.pipeline.has_demuxer());

    session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("toggle after stalled EOF drain should start replay seek");
    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::ZERO]
    );
    assert_eq!(seek_commit.target_position, MediaTime::ZERO);
    assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
}

#[test]
fn video_eof_drain_decodes_pending_video_tail_before_ending() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let video_track_id = TrackId::new(1);
    let video_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420)
        .with_color(codec_core::VideoColorMetadata::sdr_bt709_limited());
    let decoder = SharedFakeVideoDecoderThread::new();
    decoder.decode_next_packet_as_frame(Duration::from_secs(30), 41);

    session
        .pipeline
        .select_video_track(video_track_id, video_requirement);
    session.pipeline.set_video_decoder_thread(decoder.clone());
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            video_track_id,
            Duration::from_secs(30),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"video-tail"),
            PacketKeyframe::Keyframe,
        ));
    session.update_current_position(Duration::from_secs(30));
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();

    let tick_started_at = Instant::now();
    for tick_index in 0..3 {
        let tick_now = tick_started_at + Duration::from_millis(tick_index);
        let _tick_result = session.tick(PlayerTickContext::new(tick_now));
    }

    assert_eq!(decoder.sent_packets().len(), 1);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert_eq!(session.pipeline.video_decode_in_flight_packets(), 0);
    assert!(session.pipeline.video_present_queue_is_empty());
    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(30));

    session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("toggle after video EOF should start replay seek");
    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::ZERO]
    );
    assert_eq!(seek_commit.target_position, MediaTime::ZERO);
    assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
}

#[test]
fn video_only_eof_drain_advances_monotonic_clock_for_tail_frames() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let video_track_id = TrackId::new(1);
    let video_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420)
        .with_color(codec_core::VideoColorMetadata::sdr_bt709_limited());

    session
        .pipeline
        .select_video_track(video_track_id, video_requirement);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(Duration::from_millis(9_250));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(9_500), 50));
    session.enter_eof_drain();

    let tick_now = Instant::now() + Duration::from_millis(500);
    let tick_result = session.tick(PlayerTickContext::new(tick_now));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert!(session.pipeline.video_present_queue_is_empty());
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_millis(9_500))
    );
    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(30));
}

#[test]
fn eof_drain_keeps_demuxer_open_for_seek() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.enter_eof_drain();

    assert!(session.pipeline.has_demuxer());
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::from_secs(5)]
    );
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
    assert!(!session.is_eof_draining());
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn replay_and_seek_from_ended_keep_demuxer_contract() {
    let mut seek_session = PlayerSession::new();
    let seek_log = install_fake_media(
        &mut seek_session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    seek_session.enter_eof_drain();
    let _tick_result = seek_session.tick(PlayerTickContext::new(Instant::now()));
    assert_eq!(seek_session.playback_state(), PlaybackState::Ended);

    seek_session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::from_secs(5)]
    );
    assert_eq!(seek_session.playback_state(), PlaybackState::Seeking);
    assert!(!seek_session.is_eof_draining());
    assert!(seek_session.snapshot().last_error.is_none());

    let mut replay_session = PlayerSession::new();
    let replay_log = install_fake_media(
        &mut replay_session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    replay_session.update_current_position(Duration::from_secs(30));
    replay_session.enter_eof_drain();
    let _tick_result = replay_session.tick(PlayerTickContext::new(Instant::now()));
    assert_eq!(replay_session.playback_state(), PlaybackState::Ended);

    replay_session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();

    let seek_commit = replay_session
        .seek_commit()
        .expect("play after Ended should start a seek transaction");
    assert_eq!(
        replay_log.lock().expect("seek log lock").as_slice(),
        &[Duration::ZERO]
    );
    assert_eq!(seek_commit.target_position, MediaTime::ZERO);
    assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(replay_session.playback_state(), PlaybackState::Seeking);
    assert!(!replay_session.is_eof_draining());
    assert!(replay_session.snapshot().last_error.is_none());
}

#[test]
fn play_pause_toggle_pauses_eof_drain_audio_tail_without_replay_seek() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, 25.0, None);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();
    assert_eq!(session.playback_state(), PlaybackState::Draining);

    session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .unwrap();

    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(audio_output.pause_count.load(Ordering::Relaxed), 1);
    assert!(session.seek_commit().is_none());
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(seek_log.lock().expect("seek log lock").as_slice(), &[]);
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn play_pause_toggle_from_ended_after_eof_replays_with_seek_to_zero() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    session.update_current_position(Duration::from_secs(30));
    session.enter_eof_drain();
    let _tick_result = session.tick(PlayerTickContext::new(Instant::now()));
    assert_eq!(session.playback_state(), PlaybackState::Ended);

    session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("toggle after Ended should start replay seek");
    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::ZERO]
    );
    assert_eq!(seek_commit.target_position, MediaTime::ZERO);
    assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
    assert!(!session.is_eof_draining());
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn play_from_eof_drain_rewinds_to_start_and_resumes_playback() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.update_current_position(Duration::from_secs(30));
    session.enter_eof_drain();

    session.dispatch_command(PlayerCommand::Play).unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("play after EOF should start a seek transaction");
    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::ZERO]
    );
    assert_eq!(seek_commit.target_position, MediaTime::ZERO);
    assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
    assert!(!session.is_eof_draining());
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn audio_only_eof_drain_processes_pending_audio_before_ending() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    install_ready_audio_runtime(&mut session, 0.0, None);
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new(
            TrackId::new(2),
            Duration::ZERO,
            None,
            Some(Duration::from_millis(20)),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"audio-tail"),
        ));

    session.enter_eof_drain();
    let _tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert!(session.pipeline.pending_audio_packet_is_empty());
    assert_eq!(session.playback_state(), PlaybackState::Ended);
}
