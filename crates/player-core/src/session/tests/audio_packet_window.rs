use audio_core::{
    AudioChannelLayout, AudioDecoder, AudioOutputWriteIntent, AudioPacketTimeBase,
    AudioPacketTiming, EncodedAudioPacket,
};
use bytes::Bytes;
use media_core::{
    ExactPresentationWindow, PacketPresentationWindow, TimeBase, TrackId, TrackTimestamp,
};

use super::test_support::{
    ScriptedAudioDecoderFactory, ScriptedAudioOutputFactory, fake_audio_track_with_codec,
};
use super::*;

/// Наблюдаемый результат одного transport-only прохода через audio pending pipeline.
#[derive(Debug, PartialEq)]
struct AudioWindowTransportResult {
    /// Encoded bytes, которые получил decoder.
    decoded_inputs: Vec<Vec<u8>>,

    /// PCM, который дошёл до output без packet-window clipping.
    written_samples: Vec<f32>,

    /// Output accounting intent должен оставаться прежним direct PCM.
    written_intents: Vec<AudioOutputWriteIntent>,

    /// После успешной обработки pending queue должна опустеть.
    pending_packets_after_decode: usize,
}

/// Fake decoder возвращает одинаковый PCM для bounded и unbounded metadata.
pub(super) struct RecordingPcmDecoder {
    /// Shared журнал encoded inputs.
    decoded_inputs: Arc<Mutex<Vec<Vec<u8>>>>,

    /// PCM block, который fake возвращает для каждого packet-а.
    decoded_samples: Vec<f32>,

    /// Decoded sample rate.
    sample_rate: u32,

    /// Число interleaved каналов.
    channels: u32,
}

impl RecordingPcmDecoder {
    /// Создаёт configurable PCM decoder для sibling session tests.
    pub(super) fn new(
        decoded_inputs: Arc<Mutex<Vec<Vec<u8>>>>,
        decoded_samples: Vec<f32>,
        sample_rate: u32,
        channels: u32,
    ) -> Self {
        Self {
            decoded_inputs,
            decoded_samples,
            sample_rate,
            channels,
        }
    }
}

impl AudioDecoder for RecordingPcmDecoder {
    /// Запоминает exact bytes и возвращает стабильный stereo PCM block.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        self.decoded_inputs
            .lock()
            .expect("decoded input log lock")
            .push(packet.data().to_vec());
        Ok(self.decoded_samples.clone())
    }

    /// Stateless fake не хранит codec lifecycle state.
    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Возвращает exact audio clock тестового packet-а.
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает число каналов stable stereo PCM.
    fn channels(&self) -> u32 {
        self.channels
    }

    /// Связывает PCM с явным stereo layout.
    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        (self.channels == 2).then(AudioChannelLayout::stereo)
    }
}

/// Строит exact bounded window для выбранного audio track.
fn bounded_audio_window(track_id: TrackId) -> PacketPresentationWindow {
    bounded_audio_window_in_clock(track_id, 480, 1_440, 1, 48_000)
}

/// Строит bounded window с exact test clock/границами.
fn bounded_audio_window_in_clock(
    track_id: TrackId,
    start: i64,
    end_exclusive: i64,
    numer: u32,
    denom: u32,
) -> PacketPresentationWindow {
    let time_base = TimeBase::new(numer, denom).expect("test time base должна быть валидной");
    PacketPresentationWindow::Bounded(
        ExactPresentationWindow::new(
            TrackTimestamp::new(track_id, start, time_base),
            TrackTimestamp::new(track_id, end_exclusive, time_base),
        )
        .expect("test presentation window должно быть валидным"),
    )
}

/// Строит raw packet timing для exact clipping.
fn packet_timing(pts_units: i64, numer: u32, denom: u32) -> AudioPacketTiming {
    AudioPacketTiming::from_track_units(
        AudioPacketTimeBase::new(numer, denom).expect("valid test audio time base"),
        pts_units,
        None,
        None,
    )
}

/// Прогоняет один packet через pending queue, decoder и output.
fn run_audio_window_transport(
    presentation_window: PacketPresentationWindow,
) -> AudioWindowTransportResult {
    let track_id = TrackId::new(2);
    let decoded_inputs = Arc::new(Mutex::new(Vec::new()));
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory);
    session.pipeline.select_audio_track(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(RecordingPcmDecoder::new(
            Arc::clone(&decoded_inputs),
            vec![0.25, -0.5, 0.75, -1.0],
            48_000,
            2,
        )));
    let packet_timing = match presentation_window {
        PacketPresentationWindow::Unbounded => AudioPacketTiming::unknown(),
        PacketPresentationWindow::Bounded(_) => packet_timing(480, 1, 48_000),
    };
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::with_timing(
            track_id,
            Duration::from_millis(10),
            packet_timing,
            presentation_window,
            session.pipeline.seek_generation(),
            Bytes::from_static(b"encoded-audio"),
        ));

    session.process_pending_audio_packets_with_buffer_limit(200.0);

    let output_handle = output_factory_handle
        .last_output_handle()
        .expect("decoded PCM должен создать audio output");
    let recorded_inputs = decoded_inputs
        .lock()
        .expect("decoded input log lock")
        .clone();
    AudioWindowTransportResult {
        decoded_inputs: recorded_inputs,
        written_samples: output_handle.written_samples(),
        written_intents: output_handle.written_intents(),
        pending_packets_after_decode: session.pipeline.pending_audio_packet_len(),
    }
}

#[test]
fn bounded_and_unbounded_audio_windows_keep_bytes_pcm_and_accounting_identical() {
    let unbounded_result = run_audio_window_transport(PacketPresentationWindow::Unbounded);
    let bounded_result = run_audio_window_transport(bounded_audio_window(TrackId::new(2)));

    assert_eq!(bounded_result, unbounded_result);
    assert_eq!(
        bounded_result.decoded_inputs,
        vec![b"encoded-audio".to_vec()]
    );
    assert_eq!(bounded_result.written_samples, vec![0.25, -0.5, 0.75, -1.0]);
    assert_eq!(
        bounded_result.written_intents,
        vec![AudioOutputWriteIntent::DirectDecodedPcm]
    );
    assert_eq!(bounded_result.pending_packets_after_decode, 0);
}

#[test]
fn stale_bounded_audio_packet_is_dropped_before_decoder_creation() {
    let (decoder_factory, decoder_factory_handle) = ScriptedAudioDecoderFactory::success();
    let mut session = PlayerSession::with_audio_decoder_factory(decoder_factory);
    let track_id = TrackId::new(2);
    session.init_audio_pipeline(&[fake_audio_track_with_codec(2, "A_OPUS")]);
    let stale_generation = session.pipeline.seek_generation().saturating_add(1);
    session.pipeline.enqueue_pending_audio_packet(
        PendingAudioPacket::new_with_presentation_window(
            track_id,
            Duration::from_millis(10),
            bounded_audio_window(track_id),
            stale_generation,
            Bytes::from_static(b"stale-audio"),
        ),
    );

    session.process_pending_audio_packets_with_buffer_limit(200.0);

    assert!(decoder_factory_handle.created_configs().is_empty());
    assert!(session.pipeline.pending_audio_packet_is_empty());
    assert!(session.pipeline.has_deferred_audio_decoder_config());
}

#[test]
fn partial_bounded_packet_writes_and_accounts_only_retained_frames() {
    let track_id = TrackId::new(2);
    let decoded_inputs = Arc::new(Mutex::new(Vec::new()));
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory);
    session.pipeline.select_audio_track(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(RecordingPcmDecoder::new(
            Arc::clone(&decoded_inputs),
            (0..8).map(|sample| sample as f32).collect(),
            4,
            2,
        )));
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::with_timing(
            track_id,
            Duration::ZERO,
            packet_timing(0, 1, 4),
            bounded_audio_window_in_clock(track_id, 1, 3, 1, 4),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"partial"),
        ));

    session.process_pending_audio_packets_with_buffer_limit(200.0);

    let output_handle = output_factory_handle
        .last_output_handle()
        .expect("retained PCM должен создать output");
    assert_eq!(output_handle.written_samples(), vec![2.0, 3.0, 4.0, 5.0]);
    assert_eq!(
        output_handle.written_intents(),
        vec![AudioOutputWriteIntent::DirectDecodedPcm]
    );
    let output_spec = AudioOutputSpec::new(4, AudioChannelLayout::stereo());
    assert_eq!(
        session
            .pipeline
            .take_passthrough_audio_history_for_priming(output_spec),
        vec![2.0, 3.0, 4.0, 5.0]
    );
    assert_eq!(
        decoded_inputs.lock().expect("decoder log").as_slice(),
        &[b"partial".to_vec()]
    );
}

#[test]
fn fully_dropped_bounded_packet_decodes_without_output_mutation() {
    let track_id = TrackId::new(2);
    let decoded_inputs = Arc::new(Mutex::new(Vec::new()));
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory);
    session.pipeline.select_audio_track(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(RecordingPcmDecoder::new(
            Arc::clone(&decoded_inputs),
            vec![1.0, 2.0, 3.0, 4.0],
            4,
            2,
        )));
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::with_timing(
            track_id,
            Duration::ZERO,
            packet_timing(10, 1, 4),
            bounded_audio_window_in_clock(track_id, 1, 2, 1, 4),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"fully-dropped"),
        ));

    session.process_pending_audio_packets_with_buffer_limit(200.0);

    assert_eq!(output_factory_handle.create_count(), 0);
    assert!(output_factory_handle.last_output_handle().is_none());
    assert!(
        session
            .pipeline
            .passthrough_audio_history_output_spec()
            .is_none()
    );
    assert_eq!(
        decoded_inputs.lock().expect("decoder log").as_slice(),
        &[b"fully-dropped".to_vec()]
    );
    assert!(session.pipeline.pending_audio_packet_is_empty());
}

#[test]
fn invalid_bounded_metadata_is_fatal_before_output_creation() {
    let track_id = TrackId::new(2);
    let decoded_inputs = Arc::new(Mutex::new(Vec::new()));
    let (output_factory, output_factory_handle) = ScriptedAudioOutputFactory::success(0.0, None);
    let mut session = PlayerSession::with_audio_output_factory(output_factory);
    session.pipeline.select_audio_track(track_id);
    session
        .pipeline
        .install_audio_decoder(Box::new(RecordingPcmDecoder::new(
            Arc::clone(&decoded_inputs),
            vec![1.0, 2.0, 3.0, 4.0],
            48_000,
            2,
        )));
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::with_timing(
            track_id,
            Duration::ZERO,
            AudioPacketTiming::unknown(),
            bounded_audio_window_in_clock(track_id, 0, 480, 1, 48_000),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"invalid-window"),
        ));

    session.process_pending_audio_packets_with_buffer_limit(200.0);

    assert_eq!(output_factory_handle.create_count(), 0);
    let error = session
        .snapshot
        .last_error
        .as_ref()
        .expect("invalid bounded metadata должна быть fatal");
    assert_eq!(error.kind, PlayerErrorKind::RuntimeError);
    assert!(error.message.contains("Exact audio packet clipping failed"));
    assert_eq!(
        decoded_inputs.lock().expect("decoder log").as_slice(),
        &[b"invalid-window".to_vec()]
    );
}
