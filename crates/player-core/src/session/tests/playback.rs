use super::test_support::*;
use super::*;

#[test]
fn default_session_starts_idle() {
    let session = PlayerSession::new();

    assert_eq!(session.snapshot().playback_state, PlaybackState::Idle);
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
    assert_eq!(
        session.snapshot().timeline.current_position,
        MediaTime::ZERO
    );
}

#[test]
fn play_pause_commands_update_snapshot_and_events() {
    let mut session = PlayerSession::new();

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.dispatch_command(PlayerCommand::Pause).unwrap();

    let events = session.take_events();
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)));
    assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)));
}

#[test]
fn invalid_volume_is_reported_and_preserves_previous_value() {
    let mut session = PlayerSession::new();

    let result = session.dispatch_command(PlayerCommand::SetVolume(1.5));

    assert!(result.is_err());
    assert_eq!(session.snapshot().volume, 1.0);
    assert!(session.snapshot().last_error.is_some());
    assert!(
        session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::RecoverableError(_)))
    );
}

#[test]
fn reload_config_command_preserves_compatibility_event_behavior() {
    let mut session = PlayerSession::new();

    session
        .dispatch_command(PlayerCommand::ReloadConfig)
        .unwrap();

    assert_eq!(
        session.take_events(),
        vec![PlayerEvent::ConfigReloadRequested]
    );
}

#[test]
fn autoplay_audio_readiness_policy_preserves_slot_blockers() {
    assert_eq!(
        classify_autoplay_audio_readiness(AudioSeekRuntimeState::NoSelectedAudio, None, 50.0),
        AudioAutoplayReadiness::NoSelectedAudio
    );
    assert!(AudioAutoplayReadiness::NoSelectedAudio.is_ready());
    assert_eq!(
        classify_autoplay_audio_readiness(AudioSeekRuntimeState::WaitingForDecoder, None, 50.0),
        AudioAutoplayReadiness::WaitingForDecoder
    );
    assert_eq!(
        classify_autoplay_audio_readiness(AudioSeekRuntimeState::WaitingForOutput, None, 50.0),
        AudioAutoplayReadiness::WaitingForOutput
    );
    assert_eq!(
        classify_autoplay_audio_readiness(AudioSeekRuntimeState::Ready, Some(49.0), 50.0),
        AudioAutoplayReadiness::WaitingForPreroll
    );
    assert_eq!(
        classify_autoplay_audio_readiness(AudioSeekRuntimeState::Ready, Some(50.0), 50.0),
        AudioAutoplayReadiness::Ready
    );
}

#[test]
fn audio_only_autoplay_waits_for_decoder_output_and_buffer() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);

    session.pipeline.select_audio_track(audio_track_id);
    session.snapshot.selected_tracks.audio_track = Some(audio_track_id);
    session.begin_autoplay_preroll().unwrap();

    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::WaitingForDecoder
    );
    assert!(!session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);

    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(Arc::new(AtomicUsize::new(0))));

    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::WaitingForOutput
    );
    assert!(!session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);

    let (audio_output, audio_output_handle) = ScriptedAudioOutput::new(10.0, None).into_parts();
    session
        .pipeline
        .install_audio_output_for_tests(audio_output);
    session
        .pipeline
        .install_audio_clock(audio_output_handle.clock());

    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::WaitingForPreroll
    );
    assert!(!session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);

    audio_output_handle.set_buffer_level_ms(50.0);

    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::Ready
    );
    assert!(session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(audio_output_handle.play_count.load(Ordering::Relaxed), 1);
}

#[test]
fn unsupported_audio_path_does_not_block_autoplay_after_disable() {
    let (decoder_factory, _decoder_factory_handle) =
        ScriptedAudioDecoderFactory::unsupported_codec();
    let mut session = PlayerSession::with_audio_decoder_factory(decoder_factory);
    let audio_track_id = TrackId::new(2);
    let tracks = vec![fake_audio_track_with_codec(
        audio_track_id.get(),
        "A_NOT_REAL",
    )];

    session.init_audio_pipeline(&tracks);
    session.begin_autoplay_preroll().unwrap();

    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::WaitingForDecoder
    );

    session.process_audio_packet(audio_track_id, Duration::ZERO, None, None, 0, b"encoded");

    assert_eq!(session.pipeline.selected_audio_track_id(), None);
    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::NoSelectedAudio
    );
    assert!(session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(session.take_events().iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::UnsupportedAudioCodec
    )));
}

#[test]
fn video_only_autoplay_keeps_present_frame_gate() {
    let mut session = PlayerSession::new();
    let video_track = fake_track(1, TrackKind::Video);
    let video_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);

    session
        .pipeline
        .select_video_track(video_track.id, video_requirement);
    session.snapshot.selected_tracks.video_track = Some(video_track.id);
    session.begin_autoplay_preroll().unwrap();

    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::NoSelectedAudio
    );
    assert!(!session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);

    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::ZERO, 7));

    assert!(session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
}

#[test]
fn video_only_autoplay_accepts_queued_preroll_frame() {
    let mut session = PlayerSession::new();
    let video_track = fake_track(1, TrackKind::Video);
    let video_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);

    session
        .pipeline
        .select_video_track(video_track.id, video_requirement);
    session.snapshot.selected_tracks.video_track = Some(video_track.id);
    session.begin_autoplay_preroll().unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 7));

    assert!(session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
}

#[test]
fn selected_audio_without_output_does_not_satisfy_autoplay() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);

    session.pipeline.select_audio_track(audio_track_id);
    session.snapshot.selected_tracks.audio_track = Some(audio_track_id);
    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(Arc::new(AtomicUsize::new(0))));
    session.begin_autoplay_preroll().unwrap();

    assert_eq!(
        session.autoplay_audio_readiness(50.0),
        AudioAutoplayReadiness::WaitingForOutput
    );
    assert!(!session.finish_autoplay_preroll_if_ready(50.0).unwrap());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);
}

#[test]
fn autoplay_open_media_enters_buffering_before_play() {
    let mut session = PlayerSession::new();
    let request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), true);

    session
        .dispatch_command(PlayerCommand::OpenMedia(request))
        .unwrap();
    assert_eq!(session.snapshot().playback_state, PlaybackState::Opening);

    session
        .mark_media_opened(MediaSummary {
            title: Some("Sample".into()),
            source_label: "sample".into(),
            duration: Some(Duration::from_secs(10)),
        })
        .unwrap();

    assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);
    assert!(session.is_demuxing_active());
    assert!(session.can_present_video());
    assert!(
        session
            .finish_autoplay_preroll_if_ready(50.0)
            .expect("preroll finish should not fail")
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(10)));
}

#[test]
fn old_generation_render_release_does_not_touch_current_generation() {
    let mut session = PlayerSession::new();
    let old_generation = session.pipeline.render_generation();
    let old_handle = video_core::FrameResourceHandle(5);

    assert!(session.register_render_lease(old_generation, old_handle));
    session.release_video_texture(old_handle);
    session.reset_media_state();
    let new_generation = session.pipeline.render_generation();

    session.release_render_lease(old_generation, old_handle);

    assert!(new_generation > old_generation);
    assert_eq!(session.render_lease_count(), 0);
    assert_eq!(session.deferred_video_texture_release_count(), 0);
}
