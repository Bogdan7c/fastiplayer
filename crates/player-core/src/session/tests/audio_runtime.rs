use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::test_support::*;
use super::*;
use crate::{
    AudioOutputWriteIntent, AudioTempoDecodedMedia, AudioTempoFrameCount,
    AudioTempoOutputProgressMapping, AudioTempoPcmFormat, AudioTempoProcessReport,
    AudioTempoProcessor, AudioTempoProcessorConfig, AudioTempoProcessorError,
    AudioTempoProcessorFactory, AudioTempoProcessorHandle, AudioTempoReportFrameCounts,
    AudioTempoSegment, AudioTempoSegmentId, AudioTempoStretchedOutput, PlaybackRate,
    PlaybackRateAudioTempoRejectReason, PlayerCommandReject,
};
use audio_core::{AudioTempoOutputSegmentSpan, AudioTempoOutputSegmentSpans};

#[derive(Clone)]
struct FakeTempoFactoryHandle {
    created_segments: Arc<Mutex<Vec<AudioTempoSegmentId>>>,
    set_segments: Arc<Mutex<Vec<AudioTempoSegmentId>>>,
    primed_inputs: Arc<Mutex<Vec<Vec<f32>>>>,
    processed_inputs: Arc<Mutex<Vec<Vec<f32>>>>,
    finish_call_count: Arc<AtomicUsize>,
    reject_segment_changes: Arc<AtomicBool>,
    reject_finish: Arc<AtomicBool>,
    reject_next_factory_creation: Arc<AtomicBool>,
    reject_next_prime: Arc<AtomicBool>,
}

impl FakeTempoFactoryHandle {
    fn new() -> Self {
        Self {
            created_segments: Arc::new(Mutex::new(Vec::new())),
            set_segments: Arc::new(Mutex::new(Vec::new())),
            primed_inputs: Arc::new(Mutex::new(Vec::new())),
            processed_inputs: Arc::new(Mutex::new(Vec::new())),
            finish_call_count: Arc::new(AtomicUsize::new(0)),
            reject_segment_changes: Arc::new(AtomicBool::new(false)),
            reject_finish: Arc::new(AtomicBool::new(false)),
            reject_next_factory_creation: Arc::new(AtomicBool::new(false)),
            reject_next_prime: Arc::new(AtomicBool::new(false)),
        }
    }

    fn primed_inputs(&self) -> Vec<Vec<f32>> {
        self.primed_inputs
            .lock()
            .expect("primed inputs mutex should not be poisoned")
            .clone()
    }

    fn processed_inputs(&self) -> Vec<Vec<f32>> {
        self.processed_inputs
            .lock()
            .expect("processed inputs mutex should not be poisoned")
            .clone()
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

    fn finish_call_count(&self) -> usize {
        self.finish_call_count.load(Ordering::SeqCst)
    }

    fn reject_future_segment_changes(&self) {
        self.reject_segment_changes.store(true, Ordering::SeqCst);
    }

    fn reject_finish(&self) {
        self.reject_finish.store(true, Ordering::SeqCst);
    }

    fn reject_next_factory_creation(&self) {
        self.reject_next_factory_creation
            .store(true, Ordering::SeqCst);
    }

    fn reject_next_prime(&self) {
        self.reject_next_prime.store(true, Ordering::SeqCst);
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
        if self
            .handle
            .reject_next_factory_creation
            .swap(false, Ordering::SeqCst)
        {
            anyhow::bail!("scripted tempo factory failure");
        }
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
    ) -> anyhow::Result<AudioTempoProcessReport> {
        let produced_segments = if produced_stretched_output == AudioTempoFrameCount::ZERO {
            AudioTempoOutputSegmentSpans::Empty
        } else {
            AudioTempoOutputSegmentSpans::One(AudioTempoOutputSegmentSpan::new(
                self.pcm_format,
                self.segment,
                produced_stretched_output,
            ))
        };

        AudioTempoProcessReport::from_frame_counts(
            self.pcm_format,
            self.segment,
            AudioTempoReportFrameCounts {
                consumed_decoded_media,
                produced_stretched_output,
                pending_processor_output: AudioTempoFrameCount::ZERO,
                input_latency: AudioTempoFrameCount::ZERO,
                output_latency: AudioTempoFrameCount::ZERO,
            },
            AudioTempoOutputProgressMapping::new(
                produced_segments,
                AudioTempoOutputSegmentSpans::Empty,
            ),
        )
    }

    fn ensure_pcm_format(&self, decoded_media: AudioTempoDecodedMedia<'_>) -> anyhow::Result<()> {
        if decoded_media.pcm_format() == self.pcm_format {
            return Ok(());
        }

        Err(AudioTempoProcessorError::PcmFormatMismatch {
            expected: self.pcm_format,
            actual: decoded_media.pcm_format(),
        }
        .into())
    }
}

impl AudioTempoProcessor for FakeTempoProcessor {
    fn pcm_format(&self) -> AudioTempoPcmFormat {
        self.pcm_format
    }

    fn prime_decoded_history(
        &mut self,
        decoded_history: AudioTempoDecodedMedia<'_>,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        self.ensure_pcm_format(decoded_history)?;
        if self.handle.reject_next_prime.swap(false, Ordering::SeqCst) {
            anyhow::bail!("scripted tempo prime failure");
        }
        self.handle
            .primed_inputs
            .lock()
            .expect("primed inputs mutex should not be poisoned")
            .push(decoded_history.interleaved_samples().to_vec());
        self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO)
    }

    fn set_segment(
        &mut self,
        segment: AudioTempoSegment,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        if self.handle.reject_segment_changes.load(Ordering::SeqCst) {
            return Err(AudioTempoProcessorError::BackendFailure {
                message: "scripted segment rejection".to_string(),
            }
            .into());
        }

        self.segment = segment;
        self.handle
            .set_segments
            .lock()
            .expect("set segments mutex should not be poisoned")
            .push(segment.segment_id());
        self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO)
    }

    fn process_decoded_media_into<'output>(
        &mut self,
        decoded_media: AudioTempoDecodedMedia<'_>,
        output_buffer: &'output mut Vec<f32>,
    ) -> anyhow::Result<AudioTempoStretchedOutput<'output>> {
        self.ensure_pcm_format(decoded_media)?;
        self.handle
            .processed_inputs
            .lock()
            .expect("processed inputs mutex should not be poisoned")
            .push(decoded_media.interleaved_samples().to_vec());
        let produced_stretched_output = AudioTempoFrameCount::new(1);
        let report = self.report(decoded_media.frame_count(), produced_stretched_output)?;
        output_buffer.clear();
        output_buffer.extend_from_slice(&[9.0, 10.0]);
        AudioTempoStretchedOutput::new(output_buffer.as_slice(), report, self.pcm_format)
    }

    fn finish_stream_into<'output>(
        &mut self,
        output_buffer: &'output mut Vec<f32>,
    ) -> anyhow::Result<AudioTempoStretchedOutput<'output>> {
        self.handle.finish_call_count.fetch_add(1, Ordering::SeqCst);
        if self.handle.reject_finish.load(Ordering::SeqCst) {
            anyhow::bail!("scripted tempo finish failure");
        }
        let produced_stretched_output = AudioTempoFrameCount::new(1);
        output_buffer.clear();
        output_buffer.extend_from_slice(&[7.0, 8.0]);
        AudioTempoStretchedOutput::new(
            output_buffer.as_slice(),
            self.report(AudioTempoFrameCount::ZERO, produced_stretched_output)?,
            self.pcm_format,
        )
    }

    fn reset(&mut self) -> anyhow::Result<AudioTempoProcessReport> {
        self.report(AudioTempoFrameCount::ZERO, AudioTempoFrameCount::ZERO)
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
    assert_eq!(
        output_handle.written_intents(),
        vec![AudioOutputWriteIntent::DirectDecodedPcm]
    );
    assert!(!session.pipeline.has_audio_tempo_processor());
    assert!(tempo_factory_handle.created_segments().is_empty());
}

#[test]
fn non_normal_rate_audio_runtime_writes_tempo_processor_output() {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session.pipeline.select_audio_track(TrackId::new(2));
    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");
    session
        .write_decoded_audio_samples_at_current_rate(&[0.25, 0.5], 48_000, 2)
        .expect("direct PCM should establish the format and warmup history");
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
        .write_decoded_audio_samples_at_current_rate(&[1.0, 2.0, 3.0, 4.0], 48_000, 2)
        .expect("non-normal-rate audio write should succeed");

    let output_handle = output_factory_handle
        .last_output_handle()
        .expect("output should be created");
    assert_eq!(output_handle.written_samples(), vec![0.25, 0.5, 9.0, 10.0]);
    assert_eq!(
        output_handle.written_intents(),
        vec![
            AudioOutputWriteIntent::DirectDecodedPcm,
            AudioOutputWriteIntent::TempoProcessed,
        ]
    );
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
    session
        .write_decoded_audio_samples_at_current_rate(&[5.0, 6.0], 48_000, 2)
        .expect("1x transition must keep old DSP tail continuous");
    assert_eq!(
        output_handle.written_intents(),
        vec![
            AudioOutputWriteIntent::DirectDecodedPcm,
            AudioOutputWriteIntent::TempoProcessed,
            AudioOutputWriteIntent::TempoProcessed,
        ]
    );
}

#[test]
fn selected_audio_without_output_rejects_rate_change_atomically() {
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let selected_audio_track = TrackId::new(2);
    let mut session = PlayerSession::new().with_audio_tempo_processor_factory(tempo_factory);

    session.pipeline.select_audio_track(selected_audio_track);
    session.set_playback_state(PlaybackState::Paused);

    let outcome = session
        .dispatch_command(PlayerCommand::SetPlaybackRate(
            PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
        ))
        .expect("missing audio output should be a semantic reject, not fatal error");

    assert_eq!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
            reason: PlaybackRateAudioTempoRejectReason::AudioOutputUnavailable,
        })
    );
    assert_eq!(session.snapshot().playback_rate, PlaybackRate::NORMAL);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(selected_audio_track)
    );
    assert!(!session.pipeline.has_audio_tempo_processor());
    assert!(!session.pipeline.has_audio_output());
    assert!(tempo_factory_handle.created_segments().is_empty());
}

#[test]
fn selected_audio_without_pcm_format_rejects_rate_change_atomically() {
    let (output_factory, _output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let selected_audio_track = TrackId::new(2);
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session.pipeline.select_audio_track(selected_audio_track);
    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should exist before the PCM-format preflight");
    session.set_playback_state(PlaybackState::Paused);

    let outcome = session
        .dispatch_command(PlayerCommand::SetPlaybackRate(
            PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
        ))
        .expect("missing PCM format should be a semantic reject, not fatal error");

    assert_eq!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
            reason: PlaybackRateAudioTempoRejectReason::PcmFormatNotReady,
        })
    );
    assert_eq!(session.snapshot().playback_rate, PlaybackRate::NORMAL);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(selected_audio_track)
    );
    assert!(!session.pipeline.has_audio_tempo_processor());
    assert!(session.pipeline.has_audio_output());
    assert!(tempo_factory_handle.created_segments().is_empty());
}

fn selected_audio_session_with_passthrough_history()
-> (PlayerSession, FakeTempoFactoryHandle, Vec<f32>) {
    let (output_factory, _output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let selected_audio_track = TrackId::new(2);
    let passthrough_history = vec![1.0, 2.0, 3.0, 4.0];
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session.pipeline.select_audio_track(selected_audio_track);
    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");
    session
        .write_decoded_audio_samples_at_current_rate(&passthrough_history, 48_000, 2)
        .expect("direct PCM should establish format and warmup history");
    session.set_playback_state(PlaybackState::Paused);

    (session, tempo_factory_handle, passthrough_history)
}

#[test]
fn fresh_tempo_factory_rejection_preserves_rate_and_warmup_history_for_retry() {
    let (mut session, tempo_factory_handle, passthrough_history) =
        selected_audio_session_with_passthrough_history();
    let requested_rate = PlaybackRate::new(2.0).expect("2x playback rate should be valid");
    tempo_factory_handle.reject_next_factory_creation();

    let rejected_outcome = session
        .dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate))
        .expect("factory rejection should be non-fatal");

    assert_eq!(
        rejected_outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
            reason: PlaybackRateAudioTempoRejectReason::BackendRejected,
        })
    );
    assert_eq!(session.snapshot().playback_rate, PlaybackRate::NORMAL);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert!(!session.pipeline.has_audio_tempo_processor());
    assert!(session.pipeline.has_audio_output());
    assert!(
        session
            .pipeline
            .passthrough_audio_history_pcm_format()
            .is_some()
    );
    assert!(tempo_factory_handle.created_segments().is_empty());

    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate))
            .expect("retry should create processor from preserved history"),
        PlayerCommandOutcome::Applied
    );
    assert_eq!(
        tempo_factory_handle.primed_inputs(),
        vec![passthrough_history]
    );
}

#[test]
fn fresh_tempo_prime_rejection_restores_history_and_segment_id_for_retry() {
    let (mut session, tempo_factory_handle, passthrough_history) =
        selected_audio_session_with_passthrough_history();
    let requested_rate = PlaybackRate::new(2.0).expect("2x playback rate should be valid");
    tempo_factory_handle.reject_next_prime();

    let rejected_outcome = session
        .dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate))
        .expect("prime rejection should be non-fatal");

    assert_eq!(
        rejected_outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
            reason: PlaybackRateAudioTempoRejectReason::BackendRejected,
        })
    );
    assert_eq!(session.snapshot().playback_rate, PlaybackRate::NORMAL);
    assert!(!session.pipeline.has_audio_tempo_processor());
    assert!(
        session
            .pipeline
            .passthrough_audio_history_pcm_format()
            .is_some()
    );
    assert!(tempo_factory_handle.primed_inputs().is_empty());
    assert_eq!(
        tempo_factory_handle.created_segments(),
        vec![AudioTempoSegmentId::new(1)]
    );

    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate))
            .expect("retry should reuse restored history and segment id"),
        PlayerCommandOutcome::Applied
    );
    assert_eq!(
        tempo_factory_handle.created_segments(),
        vec![AudioTempoSegmentId::new(1), AudioTempoSegmentId::new(1)]
    );
    assert_eq!(
        tempo_factory_handle.primed_inputs(),
        vec![passthrough_history]
    );
}

#[test]
fn active_tempo_processor_rejection_preserves_old_rate_and_audio_path() {
    let (output_factory, _output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let selected_audio_track = TrackId::new(2);
    let accepted_rate = PlaybackRate::new(2.0).expect("2x playback rate should be valid");
    let rejected_rate = PlaybackRate::new(0.5).expect("0.5x playback rate should be valid");
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session.pipeline.select_audio_track(selected_audio_track);
    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");
    session
        .write_decoded_audio_samples_at_current_rate(&[1.0, 2.0, 3.0, 4.0], 48_000, 2)
        .expect("direct PCM should establish the format and warmup history");
    session.set_playback_state(PlaybackState::Paused);
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(accepted_rate))
            .expect("initial processor creation should succeed"),
        PlayerCommandOutcome::Applied
    );
    assert!(session.pipeline.has_audio_tempo_processor());

    tempo_factory_handle.reject_future_segment_changes();
    let outcome = session
        .dispatch_command(PlayerCommand::SetPlaybackRate(rejected_rate))
        .expect("backend rejection should stay a non-fatal command outcome");

    assert_eq!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
            reason: PlaybackRateAudioTempoRejectReason::BackendRejected,
        })
    );
    assert_eq!(session.snapshot().playback_rate, accepted_rate);
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(
        session.pipeline.selected_audio_track_id(),
        Some(selected_audio_track)
    );
    assert!(session.pipeline.has_audio_tempo_processor());
    assert!(session.pipeline.has_audio_output());
    assert!(tempo_factory_handle.set_segments().is_empty());
}

#[test]
fn fresh_tempo_processor_is_primed_with_passthrough_history_and_warmup_output_is_discarded() {
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");

    // Passthrough на 1.0x пишет PCM в output и в warmup историю.
    let passthrough_samples = vec![1.0, 2.0, 3.0, 4.0];
    session
        .write_decoded_audio_samples_at_current_rate(&passthrough_samples, 48_000, 2)
        .expect("passthrough write should succeed");

    session.set_playback_state(PlaybackState::Paused);
    assert_eq!(
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(
                PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
            ))
            .expect("rate command should not be fatal"),
        PlayerCommandOutcome::Applied
    );

    let packet_samples = vec![5.0, 6.0, 7.0, 8.0];
    session
        .write_decoded_audio_samples_at_current_rate(&packet_samples, 48_000, 2)
        .expect("non-normal-rate audio write should succeed");

    // Processor праймится position-free boundary, затем обрабатывает настоящий packet.
    assert_eq!(
        tempo_factory_handle.primed_inputs(),
        vec![passthrough_samples.clone()]
    );
    assert_eq!(
        tempo_factory_handle.processed_inputs(),
        vec![packet_samples]
    );

    // Warmup output отброшен: в output ушли passthrough PCM и один stretched блок.
    let output_handle = output_factory_handle
        .last_output_handle()
        .expect("output should be created");
    assert_eq!(
        output_handle.written_samples(),
        vec![1.0, 2.0, 3.0, 4.0, 9.0, 10.0]
    );
}

#[test]
fn passthrough_history_is_not_used_for_priming_after_seek_clear() {
    let (output_factory, _output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let (tempo_factory, tempo_factory_handle) = FakeTempoFactory::new();
    let mut session = PlayerSession::with_audio_output_factory(output_factory)
        .with_audio_tempo_processor_factory(tempo_factory);

    session
        .ensure_audio_output_for_decoded_spec(48_000, 2)
        .expect("audio output should be created");
    session
        .write_decoded_audio_samples_at_current_rate(&[1.0, 2.0, 3.0, 4.0], 48_000, 2)
        .expect("passthrough write should succeed");

    // Seek boundary очищает processor slot вместе с warmup историей.
    session.pipeline.clear_audio_output_for_seek(1);

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
        .write_decoded_audio_samples_at_current_rate(&[5.0, 6.0, 7.0, 8.0], 48_000, 2)
        .expect("non-normal-rate audio write should succeed");

    // PCM до discontinuity не должен праймить processor после неё.
    assert!(tempo_factory_handle.primed_inputs().is_empty());
    assert_eq!(
        tempo_factory_handle.processed_inputs(),
        vec![vec![5.0, 6.0, 7.0, 8.0]]
    );
}

#[test]
fn eof_flush_writes_tempo_processor_tail_once_and_clears_processor() {
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
    assert_eq!(
        output_handle.written_intents(),
        vec![
            AudioOutputWriteIntent::TempoProcessed,
            AudioOutputWriteIntent::TempoProcessed,
        ]
    );
    assert!(!session.pipeline.has_audio_tempo_processor());
    assert_eq!(tempo_factory_handle.finish_call_count(), 1);
}

#[test]
fn eof_tempo_finish_failure_is_fatal_and_never_falls_back_to_video_only() {
    let (output_factory, _output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
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
    tempo_factory_handle.reject_finish();

    session.enter_eof_drain();
    let drain_completed =
        session.finish_eof_drain_if_ready(Instant::now(), Duration::from_millis(250));

    assert!(!drain_completed);
    assert_eq!(session.playback_state(), PlaybackState::Failed);
    assert!(session.pipeline.has_audio_output());
    assert!(
        session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::FatalError(_)))
    );
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
