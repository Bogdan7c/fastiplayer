use std::sync::Arc;

use super::test_support::*;
use super::*;
use crate::{
    AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoPcmFormat, AudioTempoProcessReport,
    AudioTempoProcessor, AudioTempoProcessorConfig, AudioTempoProcessorFactory,
    AudioTempoProcessorHandle, AudioTempoReportFrameCounts, AudioTempoSegment, AudioTempoSegmentId,
    AudioTempoStretchedOutput, PlaybackRate,
};

#[derive(Clone)]
struct FakeTempoFactoryHandle {
    created_segments: Arc<Mutex<Vec<AudioTempoSegmentId>>>,
    set_segments: Arc<Mutex<Vec<AudioTempoSegmentId>>>,
}

impl FakeTempoFactoryHandle {
    fn new() -> Self {
        Self {
            created_segments: Arc::new(Mutex::new(Vec::new())),
            set_segments: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn created_segments(&self) -> Vec<AudioTempoSegmentId> {
        self.created_segments
            .lock()
            .expect("created segments mutex should not be poisoned")
            .clone()
    }

    fn set_segments(&self) -> Vec<AudioTempoSegmentId> {
        self.set_segments
            .lock()
            .expect("set segments mutex should not be poisoned")
            .clone()
    }
}

struct FakeTempoFactory {
    handle: FakeTempoFactoryHandle,
}

impl FakeTempoFactory {
    fn new() -> (Arc<Self>, FakeTempoFactoryHandle) {
        let handle = FakeTempoFactoryHandle::new();
        (
            Arc::new(Self {
                handle: handle.clone(),
            }),
            handle,
        )
    }
}

impl AudioTempoProcessorFactory for FakeTempoFactory {
    fn create_processor(
        &self,
        config: AudioTempoProcessorConfig,
    ) -> anyhow::Result<AudioTempoProcessorHandle> {
        self.handle
            .created_segments
            .lock()
            .expect("created segments mutex should not be poisoned")
            .push(config.initial_segment().segment_id());
        Ok(Box::new(FakeTempoProcessor {
            pcm_format: config.pcm_format(),
            segment: config.initial_segment(),
            handle: self.handle.clone(),
        }))
    }
}

struct FakeTempoProcessor {
    pcm_format: AudioTempoPcmFormat,
    segment: AudioTempoSegment,
    handle: FakeTempoFactoryHandle,
}

impl FakeTempoProcessor {
    fn report(
        &self,
        consumed_decoded_media: AudioTempoFrameCount,
        produced_stretched_output: AudioTempoFrameCount,
    ) -> AudioTempoProcessReport {
        AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            self.segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media,
                produced_stretched_output,
                pending_processor_output: AudioTempoFrameCount::ZERO,
                processor_latency: AudioTempoFrameCount::ZERO,
            },
        )
    }
}

impl AudioTempoProcessor for FakeTempoProcessor {
    fn set_segment(
        &mut self,
        segment: AudioTempoSegment,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        self.segment = segment;
        self.handle
            .set_segments
            .lock()
            .expect("set segments mutex should not be poisoned")
            .push(segment.segment_id());
        Ok(self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO))
    }

    fn process_decoded_media(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
    ) -> anyhow::Result<AudioTempoStretchedOutput> {
        let produced_stretched_output = AudioTempoFrameCount::new(1);
        let report = self.report(decoded_media.frame_count(), produced_stretched_output);
        AudioTempoStretchedOutput::new(vec![9.0, 10.0], report, self.pcm_format)
    }

    fn flush(&mut self) -> anyhow::Result<AudioTempoStretchedOutput> {
        let produced_stretched_output = AudioTempoFrameCount::new(1);
        AudioTempoStretchedOutput::new(
            vec![7.0, 8.0],
            self.report(AudioTempoFrameCount::ZERO, produced_stretched_output),
            self.pcm_format,
        )
    }

    fn reset(&mut self) -> anyhow::Result<AudioTempoProcessReport> {
        Ok(self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO))
    }
}

#[test]
fn audio_decoder_init_spec_maps_opus_track_without_cpal_device() {
    let tracks = vec![
        fake_track(1, TrackKind::Video),
        fake_audio_track_with_codec(2, "A_OPUS"),
    ];

    let init_spec = audio_decoder_init_spec_from_tracks(&tracks)
        .expect("Opus audio track should be accepted")
        .expect("audio init spec should exist");

    assert_eq!(
        init_spec,
        AudioDecoderInitSpec {
            track_id: TrackId::new(2),
            codec_id: "A_OPUS".to_string(),
            codec_private: None,
            initial_sample_rate: Some(48_000),
            initial_channels: Some(2),
        }
    );
}

#[test]
fn audio_decoder_init_spec_preserves_symphonia_candidate_codec_ids_without_cpal_device() {
    let codec_ids = ["A_AAC/MPEG4/LC", "A_VORBIS", "A_FLAC"];

    for codec_id in codec_ids {
        let tracks = vec![fake_audio_track_with_codec(2, codec_id)];
        let init_spec = audio_decoder_init_spec_from_tracks(&tracks)
            .expect("audio init spec should not create decoder/output resources")
            .expect("audio init spec should exist");

        assert_eq!(init_spec.codec_id, codec_id);
        assert_eq!(init_spec.codec_private, None);
        assert_eq!(init_spec.track_id, TrackId::new(2));
        assert_eq!(init_spec.initial_sample_rate, Some(48_000));
        assert_eq!(init_spec.initial_channels, Some(2));
    }
}

#[test]
fn audio_decoder_init_spec_accepts_audio_track_without_probe_parameters() {
    let mut incomplete_audio_track = fake_audio_track_with_codec(2, "A_OPUS");
    incomplete_audio_track.sample_rate = None;
    incomplete_audio_track.channels = None;

    let init_spec = audio_decoder_init_spec_from_tracks(&[
        fake_track(1, TrackKind::Video),
        incomplete_audio_track,
    ])
    .expect("missing audio parameters should not be a codec error");

    let init_spec = init_spec.expect("audio track should still be selected lazily");
    assert_eq!(init_spec.track_id, TrackId::new(2));
    assert_eq!(init_spec.initial_sample_rate, None);
    assert_eq!(init_spec.initial_channels, None);
}

#[test]
fn audio_decoder_init_spec_returns_none_when_audio_track_is_absent() {
    let init_spec = audio_decoder_init_spec_from_tracks(&[fake_track(1, TrackKind::Video)])
        .expect("video-only media should not be a codec error");

    assert!(init_spec.is_none());
}

#[test]
fn absent_audio_track_does_not_create_decoder_or_output() {
    let (decoder_factory, decoder_factory_handle) = ScriptedAudioDecoderFactory::success();
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_factories(decoder_factory, output_factory);

    session.init_audio_pipeline(&[fake_track(1, TrackKind::Video)]);

    assert_eq!(decoder_factory_handle.created_configs(), Vec::new());
    assert_eq!(output_factory_handle.create_count(), 0);
    assert!(session.pipeline.selected_audio_track_id().is_none());
    assert!(!session.pipeline.has_deferred_audio_decoder_config());
    assert!(!session.pipeline.has_audio_decoder());
    assert!(!session.pipeline.has_audio_output());
}

#[test]
fn audio_decoder_init_spec_rejects_zero_probe_parameters() {
    let mut invalid_audio_track = fake_audio_track_with_codec(2, "A_OPUS");
    invalid_audio_track.sample_rate = Some(0);

    let error = audio_decoder_init_spec_from_tracks(&[invalid_audio_track])
        .expect_err("zero sample_rate should stay a planning error");

    assert_eq!(error.kind, PlayerErrorKind::RuntimeError);
}

#[test]
fn audio_decoder_init_spec_preserves_unknown_codec_for_factory_typed_error() {
    let tracks = vec![fake_audio_track_with_codec(2, "A_NOT_REAL")];

    let init_spec = audio_decoder_init_spec_from_tracks(&tracks)
        .expect("unknown audio codec should be deferred to audio factory")
        .expect("audio init spec should exist");

    assert_eq!(init_spec.codec_id, "A_NOT_REAL");
}

#[test]
fn init_audio_pipeline_defers_unsupported_factory_codec_until_first_packet() {
    let (decoder_factory, _decoder_factory_handle) =
        ScriptedAudioDecoderFactory::unsupported_codec();
    let mut session = PlayerSession::with_audio_decoder_factory(decoder_factory);
    let tracks = vec![fake_audio_track_with_codec(2, "A_NOT_REAL")];

    session.init_audio_pipeline(&tracks);

    assert!(!session.pipeline.has_audio_decoder());
    assert!(!session.pipeline.has_audio_output());
    assert!(session.pipeline.has_deferred_audio_decoder_config());
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(TrackId::new(2))
    );
    assert!(session.take_events().is_empty());

    session.process_audio_packet(TrackId::new(2), Duration::ZERO, None, None, 0, b"encoded");

    assert!(!session.pipeline.has_audio_decoder());
    assert!(!session.pipeline.has_audio_output());
    assert!(!session.pipeline.has_deferred_audio_decoder_config());
    assert!(session.pipeline.selected_audio_track_id().is_none());
    assert!(session.take_events().iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::UnsupportedAudioCodec
    )));
}

#[test]
fn lazy_audio_decoder_factory_is_called_only_for_selected_track() {
    let (decoder_factory, decoder_factory_handle) = ScriptedAudioDecoderFactory::success();
    let mut session = PlayerSession::with_audio_decoder_factory(decoder_factory);
    let tracks = vec![fake_audio_track_with_codec(2, "A_OPUS")];

    session.init_audio_pipeline(&tracks);
    session.process_audio_packet(TrackId::new(3), Duration::ZERO, None, None, 0, b"other");

    assert_eq!(decoder_factory_handle.created_configs(), Vec::new());
    assert!(session.pipeline.has_deferred_audio_decoder_config());
    assert!(!session.pipeline.has_audio_decoder());

    session.process_audio_packet(TrackId::new(2), Duration::ZERO, None, None, 0, b"selected");

    assert!(session.pipeline.has_audio_decoder());
    assert!(!session.pipeline.has_deferred_audio_decoder_config());
    assert_eq!(
        decoder_factory_handle.created_configs(),
        vec![audio_core::AudioDecoderConfig::from_track_metadata(
            2,
            "A_OPUS",
            Some(48_000),
            Some(2),
        )]
    );
}

#[test]
fn audio_decoder_factory_generic_error_stays_recoverable_runtime_error() {
    let (decoder_factory, _decoder_factory_handle) =
        ScriptedAudioDecoderFactory::error("decoder backend failed");
    let mut session = PlayerSession::with_audio_decoder_factory(decoder_factory);
    let tracks = vec![fake_audio_track_with_codec(2, "A_OPUS")];

    session.init_audio_pipeline(&tracks);
    session.process_audio_packet(TrackId::new(2), Duration::ZERO, None, None, 0, b"encoded");

    assert!(!session.pipeline.has_audio_decoder());
    assert!(!session.pipeline.has_deferred_audio_decoder_config());
    assert!(session.pipeline.selected_audio_track_id().is_none());
    assert!(session.take_events().iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::RuntimeError
                && error.message.contains("decoder backend failed")
    )));
}

#[test]
fn audio_output_factory_is_not_called_before_decoded_spec() {
    let (factory, factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(factory);
    let audio_track_id = TrackId::new(2);

    session.pipeline.select_audio_track(audio_track_id);
    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(Arc::new(AtomicUsize::new(0))));
    session.process_audio_packet(audio_track_id, Duration::ZERO, None, None, 0, b"encoded");

    assert_eq!(factory_handle.create_count(), 0);
    assert!(!session.pipeline.has_audio_output());
    assert!(!session.pipeline.has_audio_clock());
}

#[test]
fn audio_output_factory_success_installs_output_and_clock() {
    let (factory, factory_handle) = ScriptedAudioOutputFactory::success(25.0, None);
    let mut session = PlayerSession::with_audio_output_factory(factory);

    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("successful factory должен установить output");

    assert!(session.pipeline.has_audio_output());
    assert!(session.pipeline.has_audio_clock());
    assert_eq!(
        factory_handle.created_specs(),
        vec![AudioOutputSpec {
            sample_rate: 48_000,
            channels: 2,
        }]
    );
    assert_eq!(session.audio_buffer_level_ms(), Some(25.0));
}

#[test]
fn normal_rate_audio_runtime_writes_decoded_pcm_without_tempo_processor() {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);
    let samples = vec![1.0, 2.0, 3.0, 4.0];

    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");
    session
        .write_decoded_audio_samples_at_current_rate(&samples, 48_000, 2)
        .expect("normal-rate audio write should succeed");

    let output_handle = output_factory_handle
        .last_output_handle()
        .expect("output should be created");
    assert_eq!(output_handle.written_samples(), samples);
    assert!(!session.pipeline.has_audio_tempo_processor());
    assert!(tempo_factory_handle.created_segments().is_empty());
}

#[test]
fn non_normal_rate_audio_runtime_writes_tempo_processor_output() {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session.set_playback_state(PlaybackState::Paused);
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(
                PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
            ))
            .expect("rate command should not be fatal"),
        PlayerCommandOutcome::Applied
    );
    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");
    session
        .write_decoded_audio_samples_at_current_rate(&[1.0, 2.0, 3.0, 4.0], 48_000, 2)
        .expect("non-normal-rate audio write should succeed");

    let output_handle = output_factory_handle
        .last_output_handle()
        .expect("output should be created");
    assert_eq!(output_handle.written_samples(), vec![9.0, 10.0]);
    assert!(session.pipeline.has_audio_tempo_processor());
    assert_eq!(
        tempo_factory_handle.created_segments(),
        vec![AudioTempoSegmentId::new(1)]
    );
    assert!(tempo_factory_handle.set_segments().is_empty());

    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(
                PlaybackRate::new(0.5).expect("0.5x playback rate should be valid"),
            ))
            .expect("rate command should not be fatal"),
        PlayerCommandOutcome::Applied
    );
    assert_eq!(
        tempo_factory_handle.created_segments(),
        vec![AudioTempoSegmentId::new(1)]
    );
    assert_eq!(
        tempo_factory_handle.set_segments(),
        vec![AudioTempoSegmentId::new(2)]
    );

    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(PlaybackRate::NORMAL))
            .expect("rate command should not be fatal"),
        PlayerCommandOutcome::Applied
    );
    assert!(session.pipeline.has_audio_tempo_processor());
    assert_eq!(
        tempo_factory_handle.set_segments(),
        vec![AudioTempoSegmentId::new(2), AudioTempoSegmentId::new(3)]
    );
}

#[test]
fn eof_flush_writes_tempo_processor_tail_once_and_clears_processor() {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, _tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session.set_playback_state(PlaybackState::Paused);
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(
                PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
            ))
            .expect("rate command should not be fatal"),
        PlayerCommandOutcome::Applied
    );
    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");
    session
        .write_decoded_audio_samples_at_current_rate(&[1.0, 2.0, 3.0, 4.0], 48_000, 2)
        .expect("non-normal-rate audio write should succeed");

    session
        .flush_audio_tempo_processor_for_eof()
        .expect("tempo EOF flush should succeed");
    session
        .flush_audio_tempo_processor_for_eof()
        .expect("second tempo EOF flush should be no-op");

    let output_handle = output_factory_handle
        .last_output_handle()
        .expect("output should be created");
    assert_eq!(output_handle.written_samples(), vec![9.0, 10.0, 7.0, 8.0]);
    assert!(!session.pipeline.has_audio_tempo_processor());
}

#[test]
fn audio_output_factory_creation_error_becomes_audio_device_unavailable() {
    let (factory, factory_handle) = ScriptedAudioOutputFactory::failure("device missing");
    let mut session = PlayerSession::with_audio_output_factory(factory);

    let error = session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect_err("factory error должен стать player error");

    assert_eq!(error.kind, PlayerErrorKind::AudioDeviceUnavailable);
    assert!(error.message.contains("device missing"));
    assert_eq!(factory_handle.create_count(), 1);
    assert!(!session.pipeline.has_audio_output());
    assert!(!session.pipeline.has_audio_clock());
}

#[test]
fn lazy_audio_output_init_while_playing_calls_play() {
    let (factory, factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(factory);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("lazy init during Playing должен запустить output");

    let output_handle = factory_handle
        .last_output_handle()
        .expect("factory должен создать output handle");
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(output_handle.play_count.load(Ordering::Relaxed), 1);
}

#[test]
fn lazy_audio_output_play_error_stays_audio_device_unavailable() {
    let (factory, factory_handle) =
        ScriptedAudioOutputFactory::success(0.0, Some("fake audio play failed"));
    let mut session = PlayerSession::with_audio_output_factory(factory);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    let error = session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect_err("play error после lazy init должен быть видимым");

    let output_handle = factory_handle
        .last_output_handle()
        .expect("factory должен успеть установить output до play error");
    assert_eq!(error.kind, PlayerErrorKind::AudioDeviceUnavailable);
    assert!(error.message.contains("fake audio play failed"));
    assert_eq!(output_handle.play_count.load(Ordering::Relaxed), 1);
}

#[test]
fn demux_track_list_update_replans_audio_runtime_without_fatal_error() {
    let mut session = PlayerSession::default();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let reset_count = Arc::new(AtomicUsize::new(0));
    session
        .pipeline
        .install_audio_decoder(counting_audio_decoder_handle(Arc::clone(&reset_count)));
    session.pipeline.install_deferred_audio_decoder_config(
        audio_core::AudioDecoderConfig::from_track_metadata(2, "A_OPUS", Some(48_000), Some(2)),
    );
    let initial_generation = session.pipeline.seek_generation();

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_audio_track_with_codec(3, "A_AAC")],
        Some(Duration::from_secs(42)),
    ));

    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(session.snapshot.duration, Some(Duration::from_secs(42)));
    assert_eq!(session.pipeline.seek_generation(), initial_generation + 1);
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(TrackId::new(3))
    );
    assert!(session.pipeline.has_deferred_audio_decoder_config());
    assert!(!session.pipeline.has_audio_decoder());
    assert!(!session.pipeline.has_audio_output());
    assert!(session.snapshot.last_error.is_none());
    assert_eq!(reset_count.load(Ordering::Relaxed), 0);
}
