use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use super::*;
use audio_core::{AudioOutputClockTiming, AudioOutputWriteIntent};
use codec_core::VideoCodec;
use media_core::{MediaTime, TrackKind};

/// Создаёт decoded frame без реальных GPU resources для проверки pipeline storage.
fn decoded_frame_for_tests(pts: Duration, resource_handle: u64) -> video_core::DecodedFrame {
    video_core::DecodedFrame {
        generation: 0,
        pts,
        frame_contract: video_frame_contract::VideoFrameContract::dma_buf_nv12(
            video_frame_contract::DmaBufImageLayout::SeparateLayers,
        ),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: video_core::FrameResourceHandle(resource_handle),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

/// Управляемый fake decoder для проверки audio boundary без CPAL и codec side effects.
struct FakeAudioDecoder {
    /// Результат, который fake вернёт из `decode`.
    decode_outcome: FakeAudioDecodeOutcome,

    /// Ошибка, которую fake вернёт из `reset`, если она задана.
    reset_error: Option<&'static str>,

    /// Sample rate, который boundary должен вернуть вместе с decoded samples.
    sample_rate: u32,

    /// Channel count, который boundary должен вернуть вместе с decoded samples.
    channels: u32,
}

impl FakeAudioDecoder {
    /// Создаёт fake decoder, который успешно возвращает заданные PCM samples.
    fn with_samples(samples: Vec<f32>, sample_rate: u32, channels: u32) -> Self {
        Self {
            decode_outcome: FakeAudioDecodeOutcome::Samples(samples),
            reset_error: None,
            sample_rate,
            channels,
        }
    }

    /// Создаёт fake decoder, который падает на decode и успешно reset-ится.
    fn with_decode_error(error: &'static str) -> Self {
        Self {
            decode_outcome: FakeAudioDecodeOutcome::Error(error),
            reset_error: None,
            sample_rate: 48_000,
            channels: 2,
        }
    }

    /// Создаёт fake decoder, который decode-ит пустой packet и падает на reset.
    fn with_reset_error(error: &'static str) -> Self {
        Self {
            decode_outcome: FakeAudioDecodeOutcome::Samples(Vec::new()),
            reset_error: Some(error),
            sample_rate: 48_000,
            channels: 2,
        }
    }
}

/// Явный сценарий fake decode, чтобы тесты не полагались на magic flags.
enum FakeAudioDecodeOutcome {
    /// Успешный decode с предсказуемыми samples.
    Samples(Vec<f32>),

    /// Ошибка decode с предсказуемым текстом.
    Error(&'static str),
}

impl audio_core::AudioDecoder for FakeAudioDecoder {
    /// Возвращает заранее заданный результат decode.
    fn decode(&mut self, _packet: &audio_core::EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        match &self.decode_outcome {
            FakeAudioDecodeOutcome::Samples(samples) => Ok(samples.clone()),
            FakeAudioDecodeOutcome::Error(error) => Err(anyhow::anyhow!(*error)),
        }
    }

    /// Возвращает reset error только если тест явно его сконфигурировал.
    fn reset(&mut self) -> anyhow::Result<()> {
        match self.reset_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(()),
        }
    }

    /// Возвращает sample rate fake decoder-а.
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает channel count fake decoder-а.
    fn channels(&self) -> u32 {
        self.channels
    }
}

/// Fake clock для проверки нейтрального audio clock boundary без concrete audio crate clock.
struct FixedAudioClock {
    /// Текущее значение, которое clock отдаёт pipeline.
    now: Mutex<Duration>,

    /// Конец всего PCM, принятого fake output-ом.
    submitted_output_end_position: Mutex<Duration>,

    /// Счётчик reset-вызовов, чтобы тест видел side effect boundary.
    reset_count: AtomicUsize,

    /// Scripted счётчик underrun callbacks.
    underrun_callbacks: AtomicU64,
}

impl FixedAudioClock {
    /// Создаёт clock с заданными observable значениями.
    fn new(now: Duration, underrun_callbacks: u64) -> Self {
        Self {
            now: Mutex::new(now),
            submitted_output_end_position: Mutex::new(now),
            reset_count: AtomicUsize::new(0),
            underrun_callbacks: AtomicU64::new(underrun_callbacks),
        }
    }

    /// Возвращает количество reset-вызовов.
    fn reset_count(&self) -> usize {
        self.reset_count.load(Ordering::Relaxed)
    }

    /// Меняет scripted playback позицию без обращения к concrete audio backend-у.
    fn set_now(&self, now: Duration) {
        *self
            .now
            .lock()
            .expect("fake clock mutex не должен ломаться") = now;
        *self
            .submitted_output_end_position
            .lock()
            .expect("fake submitted position mutex не должен ломаться") = now;
    }

    /// Задаёт отдельные audible/submitted позиции для tail-aware тестов.
    fn set_output_timing(
        &self,
        audible_output_position: Duration,
        submitted_output_end_position: Duration,
    ) {
        *self
            .now
            .lock()
            .expect("fake clock mutex не должен ломаться") = audible_output_position;
        *self
            .submitted_output_end_position
            .lock()
            .expect("fake submitted position mutex не должен ломаться") =
            submitted_output_end_position;
    }
}

impl PlayerAudioClock for FixedAudioClock {
    /// Возвращает scripted playback позицию.
    fn now(&self) -> Duration {
        *self
            .now
            .lock()
            .expect("fake clock mutex не должен ломаться")
    }

    /// Возвращает scripted audible/submitted snapshot.
    fn output_timing(&self) -> AudioOutputClockTiming {
        let audible_output_position = self.now();
        let submitted_output_end_position = *self
            .submitted_output_end_position
            .lock()
            .expect("fake submitted position mutex не должен ломаться");
        AudioOutputClockTiming::new(audible_output_position, submitted_output_end_position)
    }

    /// Сбрасывает позицию и отмечает reset-вызов.
    fn reset(&self) {
        self.reset_count.fetch_add(1, Ordering::Relaxed);
        *self
            .now
            .lock()
            .expect("fake clock mutex не должен ломаться") = Duration::ZERO;
        *self
            .submitted_output_end_position
            .lock()
            .expect("fake submitted position mutex не должен ломаться") = Duration::ZERO;
    }

    /// Возвращает scripted underrun count.
    fn underrun_callbacks(&self) -> u64 {
        self.underrun_callbacks.load(Ordering::Relaxed)
    }
}

/// Fake output с управляемым buffer level для проверки EOF-drain boundary.
struct FixedAudioOutput {
    /// Нейтральный fake clock output-а.
    clock: Arc<FixedAudioClock>,

    /// Уровень buffer-а, который вернёт output boundary.
    buffer_level_ms: f64,

    /// Ошибка play, если сценарий проверяет propagation.
    play_error: Option<&'static str>,

    /// Ошибка pause, если сценарий проверяет propagation.
    pause_error: Option<&'static str>,

    /// Последний volume, который pipeline передал output boundary.
    last_volume: Arc<Mutex<Option<f32>>>,
}

impl FixedAudioOutput {
    /// Создаёт fake output с заданным уровнем buffer-а.
    fn new(buffer_level_ms: f64) -> Self {
        Self {
            clock: Arc::new(FixedAudioClock::new(Duration::ZERO, 0)),
            buffer_level_ms,
            play_error: None,
            pause_error: None,
            last_volume: Arc::new(Mutex::new(None)),
        }
    }

    /// Создаёт fake output с scripted play/pause errors.
    fn with_errors(play_error: Option<&'static str>, pause_error: Option<&'static str>) -> Self {
        Self {
            play_error,
            pause_error,
            ..Self::new(0.0)
        }
    }

    /// Возвращает clock handle до передачи output-а в pipeline.
    fn clock_handle(&self) -> Arc<FixedAudioClock> {
        Arc::clone(&self.clock)
    }

    /// Возвращает volume log handle до передачи output-а в pipeline.
    fn volume_handle(&self) -> Arc<Mutex<Option<f32>>> {
        Arc::clone(&self.last_volume)
    }
}

impl PlayerAudioOutput for FixedAudioOutput {
    /// Записывает все samples как успешно принятые.
    fn write_samples(&mut self, samples: &[f32], _intent: AudioOutputWriteIntent) -> u64 {
        samples.len() as u64
    }

    /// Fake stream всегда успешно стартует.
    fn play(&mut self) -> anyhow::Result<()> {
        match self.play_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(()),
        }
    }

    /// Fake pause возвращает timing того же output clock-а.
    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        match self.pause_error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(self.clock.output_timing()),
        }
    }

    /// Возвращает тот же generation, который запросил caller.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        Ok(generation)
    }

    /// Volume не влияет на EOF-drain state.
    fn set_volume(&mut self, volume: f32) {
        *self
            .last_volume
            .lock()
            .expect("fake volume mutex не должен ломаться") = Some(volume);
    }

    /// Возвращает scripted buffer level.
    fn buffer_level_ms(&self) -> f64 {
        self.buffer_level_ms
    }

    /// Возвращает fake clock для соблюдения output contract-а.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        let clock: Arc<dyn PlayerAudioClock> = self.clock.clone();
        clock
    }
}

#[test]
fn queued_video_frame_methods_preserve_fifo_order_and_len() {
    let mut pipeline = PlaybackPipeline::default();

    assert!(pipeline.video_present_queue_is_empty());
    assert_eq!(pipeline.video_present_queue_len(), 0);

    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));

    assert_eq!(pipeline.video_present_queue_len(), 2);
    assert_eq!(
        pipeline.front_queued_video_frame().map(|frame| frame.pts),
        Some(Duration::from_millis(16))
    );
    assert_eq!(
        pipeline
            .front_and_next_queued_video_frames()
            .map(|(front_frame, next_frame)| (front_frame.pts, next_frame.pts)),
        Some((Duration::from_millis(16), Duration::from_millis(33)))
    );

    assert_eq!(
        pipeline
            .pop_queued_video_frame_front()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(1))
    );
    assert_eq!(
        pipeline
            .pop_queued_video_frame_front()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(2))
    );
    assert!(pipeline.pop_queued_video_frame_front().is_none());
    assert!(pipeline.video_present_queue_is_empty());
}

#[test]
fn queued_video_frame_covers_target_for_generation_reports_only_matching_frames() {
    let mut pipeline = PlaybackPipeline::default();
    let target_position = Duration::from_millis(100);
    let active_generation = 7;
    let stale_generation = 6;

    assert!(
        !pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );

    let mut pretarget_frame = decoded_frame_for_tests(Duration::from_millis(90), 1);
    pretarget_frame.generation = active_generation;
    pipeline.enqueue_queued_video_frame(pretarget_frame);

    assert!(
        !pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );

    let mut stale_target_frame = decoded_frame_for_tests(Duration::from_millis(100), 2);
    stale_target_frame.generation = stale_generation;
    pipeline.enqueue_queued_video_frame(stale_target_frame);

    assert!(
        !pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );

    let mut active_target_frame = decoded_frame_for_tests(Duration::from_millis(100), 3);
    active_target_frame.generation = active_generation;
    pipeline.enqueue_queued_video_frame(active_target_frame);

    assert!(
        pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );
}

#[test]
fn present_video_frame_methods_keep_replacement_ownership_explicit() {
    let mut pipeline = PlaybackPipeline::default();

    assert!(!pipeline.has_present_video_frame());
    assert!(pipeline.present_video_frame().is_none());
    assert!(pipeline.take_present_video_frame().is_none());

    pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(10), 10));

    assert!(pipeline.has_present_video_frame());
    assert_eq!(
        pipeline.present_video_frame().map(|frame| frame.pts),
        Some(Duration::from_millis(10))
    );

    let replaced_frame = pipeline
        .replace_present_video_frame(decoded_frame_for_tests(Duration::from_millis(20), 20));

    assert_eq!(
        replaced_frame.map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(10))
    );
    assert_eq!(
        pipeline
            .take_present_video_frame()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(20))
    );
    assert!(!pipeline.has_present_video_frame());
}

#[test]
fn opened_media_boundary_methods_expose_installed_source_slots() {
    let mut pipeline = PlaybackPipeline::default();
    let track_infos = vec![
        source_slot_track(TrackId::new(1), TrackKind::Video, "V_VP9"),
        source_slot_track(TrackId::new(2), TrackKind::Audio, "A_OPUS"),
    ];

    assert!(!pipeline.has_demuxer());
    assert_eq!(pipeline.track_count(), 0);

    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(track_infos.clone())),
        Some(PathBuf::from("/tmp/source.webm")),
        Some("external source".to_owned()),
        track_infos,
    );

    assert!(pipeline.has_demuxer());
    assert_eq!(
        pipeline.source_file_path(),
        Some(Path::new("/tmp/source.webm"))
    );
    assert_eq!(pipeline.source_label(), Some("external source"));
    assert_eq!(pipeline.track_count(), 2);
    assert_eq!(pipeline.tracks()[0].id, TrackId::new(1));
    assert_eq!(pipeline.tracks()[1].kind, TrackKind::Audio);
}

#[test]
fn demux_track_list_update_invalidates_decoder_dependent_state() {
    let mut pipeline = PlaybackPipeline::default();
    let old_video_track = TrackId::new(1);
    let old_audio_track = TrackId::new(2);
    let new_audio_track = TrackId::new(3);
    let initial_tracks = vec![
        source_slot_track(old_video_track, TrackKind::Video, "V_VP9"),
        source_slot_track(old_audio_track, TrackKind::Audio, "A_OPUS"),
    ];

    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(initial_tracks.clone())),
        None,
        None,
        initial_tracks,
    );
    pipeline.select_video_track(
        old_video_track,
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    pipeline.select_audio_track(old_audio_track);
    pipeline.install_deferred_audio_decoder_config(
        audio_core::AudioDecoderConfig::from_track_metadata(
            old_audio_track.get(),
            "A_OPUS",
            Some(48_000),
            Some(2),
        ),
    );
    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
        vec![0.0],
        48_000,
        2,
    )));
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
        old_audio_track,
        Duration::ZERO,
        None,
        None,
        pipeline.seek_generation(),
        Bytes::from_static(b"old audio"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        old_video_track,
        Duration::ZERO,
        pipeline.seek_generation(),
        Bytes::from_static(b"old video"),
        true,
    ));
    pipeline.mark_video_decoder_bootstrapped();
    pipeline.note_video_packet_sent_to_decoder();

    pipeline.apply_demux_track_list_update(vec![source_slot_track(
        new_audio_track,
        TrackKind::Audio,
        "A_AAC",
    )]);

    assert_eq!(pipeline.tracks()[0].id, new_audio_track);
    assert!(pipeline.selected_audio_track_id().is_none());
    assert!(pipeline.selected_video_track_id().is_none());
    assert!(!pipeline.has_audio_decoder());
    assert!(!pipeline.has_deferred_audio_decoder_config());
    assert_eq!(pipeline.pending_audio_packet_len(), 0);
    assert_eq!(pipeline.pending_video_packet_len(), 0);
    assert!(!pipeline.video_decoder_needs_keyframe());
    assert_eq!(pipeline.video_decode_in_flight_packets(), 0);
}

#[test]
fn audio_decoder_boundaries_preserve_absent_success_and_error_states() {
    let mut pipeline = PlaybackPipeline::default();
    let packet = audio_core::EncodedAudioPacket::without_timing(TrackId::new(2).get(), b"packet");
    let bad_packet =
        audio_core::EncodedAudioPacket::without_timing(TrackId::new(2).get(), b"bad packet");

    assert!(!pipeline.has_audio_decoder());
    assert!(pipeline.decode_audio_packet(&packet).is_none());
    assert!(pipeline.reset_audio_decoder().is_none());

    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
        vec![0.25, -0.25],
        44_100,
        2,
    )));

    let decoded_audio = pipeline
        .decode_audio_packet(&packet)
        .expect("installed decoder должен вернуть decode result")
        .expect("successful fake decoder не должен падать");
    assert_eq!(decoded_audio.samples, vec![0.25, -0.25]);
    assert_eq!(decoded_audio.sample_rate, 44_100);
    assert_eq!(decoded_audio.channels, 2);

    pipeline.clear_audio_decoder();
    assert!(!pipeline.has_audio_decoder());

    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_decode_error(
        "decode failed",
    )));

    let decode_error = pipeline
        .decode_audio_packet(&bad_packet)
        .expect("installed decoder должен сохранить decode error")
        .expect_err("decode error должен дойти до session boundary");
    assert_eq!(decode_error.to_string(), "decode failed");

    pipeline.clear_audio_decoder();
    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_reset_error("reset failed")));

    let reset_error = pipeline
        .reset_audio_decoder()
        .expect("installed decoder должен вернуть reset result")
        .expect_err("reset error должен дойти до session boundary");
    assert_eq!(reset_error.to_string(), "reset failed");
}

#[test]
fn deferred_audio_decoder_config_boundary_preserves_absent_match_and_mismatch() {
    let mut pipeline = PlaybackPipeline::default();
    let config = audio_core::AudioDecoderConfig::from_track_metadata(
        TrackId::new(2).get(),
        "A_AAC",
        None,
        None,
    );

    assert!(!pipeline.has_deferred_audio_decoder_config());
    assert!(
        pipeline
            .take_deferred_audio_decoder_config(TrackId::new(2))
            .is_none()
    );

    pipeline.install_deferred_audio_decoder_config(config.clone());
    assert!(pipeline.has_deferred_audio_decoder_config());
    assert!(
        pipeline
            .take_deferred_audio_decoder_config(TrackId::new(3))
            .is_none()
    );
    assert!(pipeline.has_deferred_audio_decoder_config());

    let taken_config = pipeline
        .take_deferred_audio_decoder_config(TrackId::new(2))
        .expect("matching track should consume deferred decoder config");
    assert_eq!(taken_config, config);
    assert!(!pipeline.has_deferred_audio_decoder_config());
}

#[test]
fn audio_seek_runtime_state_classifies_slots_without_cpal_output() {
    assert_eq!(
        audio_seek_runtime_state_from_slots(false, false, false),
        AudioSeekRuntimeState::NoSelectedAudio
    );
    assert_eq!(
        audio_seek_runtime_state_from_slots(true, false, false),
        AudioSeekRuntimeState::WaitingForDecoder
    );
    assert_eq!(
        audio_seek_runtime_state_from_slots(true, true, false),
        AudioSeekRuntimeState::WaitingForOutput
    );
    assert_eq!(
        audio_seek_runtime_state_from_slots(true, true, true),
        AudioSeekRuntimeState::Ready
    );
}

#[test]
fn audio_seek_runtime_state_boundary_keeps_selection_ownership() {
    let mut pipeline = PlaybackPipeline::default();
    let track_id = TrackId::new(2);
    let decoder_config =
        audio_core::AudioDecoderConfig::from_track_metadata(track_id.get(), "A_OPUS", None, None);

    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::NoSelectedAudio
    );

    pipeline.select_audio_track(track_id);
    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForDecoder
    );

    pipeline.install_deferred_audio_decoder_config(decoder_config);
    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForDecoder
    );
    assert_eq!(pipeline.selected_audio_track_id(), Some(track_id));

    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
        vec![0.0, 0.0],
        48_000,
        2,
    )));
    assert_eq!(
        pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForOutput
    );
    assert_eq!(pipeline.selected_audio_track_id(), Some(track_id));
}

#[test]
fn absent_audio_output_boundaries_are_noop_without_losing_absent_state() {
    let mut pipeline = PlaybackPipeline::default();

    assert!(!pipeline.has_audio_output());
    assert_eq!(
        pipeline.write_audio_output_samples(&[0.0, 0.1], AudioOutputWriteIntent::DirectDecodedPcm,),
        None
    );
    assert!(pipeline.play_audio_output().is_none());
    assert!(pipeline.pause_audio_output_and_capture_clock().is_none());
    assert!(pipeline.clear_audio_output_for_seek(1).is_none());
    assert!(pipeline.audio_output_buffer_level_ms().is_none());
    assert!(pipeline.audio_output_clock().is_none());
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::NoSelectedAudio
    );

    pipeline.clear_audio_output();
    assert!(!pipeline.has_audio_output());
}

#[test]
fn installed_audio_output_boundary_forwards_calls_and_neutral_clock() {
    let mut pipeline = PlaybackPipeline::default();
    let output = FixedAudioOutput::new(42.0);
    let clock = output.clock_handle();
    let volume = output.volume_handle();

    pipeline.install_audio_output_for_tests(Box::new(output));

    assert!(pipeline.has_audio_output());
    assert!(pipeline.audio_output_clock().is_some());
    assert_eq!(
        pipeline
            .write_audio_output_samples(&[0.1, -0.1], AudioOutputWriteIntent::DirectDecodedPcm,),
        Some(2)
    );
    assert_eq!(pipeline.audio_output_buffer_level_ms(), Some(42.0));
    assert_eq!(
        pipeline
            .clear_audio_output_for_seek(7)
            .expect("installed output должен вернуть clear result")
            .expect("fake clear должен быть успешным"),
        7
    );
    assert!(pipeline.set_audio_output_volume(0.25));
    assert_eq!(
        *volume.lock().expect("fake volume mutex не должен ломаться"),
        Some(0.25)
    );

    pipeline.install_audio_clock(clock);
    assert!(pipeline.reset_audio_clock());
}

#[test]
fn audio_output_boundary_preserves_play_pause_errors() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::with_errors(
        Some("play failed"),
        Some("pause failed"),
    )));

    let play_error = pipeline
        .play_audio_output()
        .expect("installed output должен вернуть play result")
        .expect_err("play error должен пройти через boundary");
    assert_eq!(play_error.to_string(), "play failed");

    let pause_error = pipeline
        .pause_audio_output_and_capture_clock()
        .expect("installed output должен вернуть pause result")
        .expect_err("pause error должен пройти через boundary");
    assert_eq!(pause_error.to_string(), "pause failed");
}

#[test]
fn audio_eof_drain_state_preserves_queue_output_and_playback_distinctions() {
    let mut pipeline = PlaybackPipeline::default();
    let audio_track_id = TrackId::new(2);

    pipeline.select_audio_track(audio_track_id);
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::NoOutput
    );

    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
        audio_track_id,
        Duration::ZERO,
        None,
        Some(Duration::from_millis(20)),
        pipeline.seek_generation(),
        Bytes::from_static(b"encoded-audio"),
    ));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::PendingPackets { queued_packets: 1 }
    );

    let _pending_packet = pipeline.pop_pending_audio_packet_front();
    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(24.0)));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainingOutput {
            buffer_level_ms: 24.0,
            playback_requested: false,
        }
    );

    pipeline
        .play_audio_output()
        .expect("installed output должен вернуть play result")
        .expect("fake output play должен быть успешным");
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainingOutput {
            buffer_level_ms: 24.0,
            playback_requested: true,
        }
    );

    pipeline.install_audio_output_for_tests(Box::new(FixedAudioOutput::new(0.0)));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainedOutput {
            playback_requested: false,
        }
    );
}

#[test]
fn audio_eof_drain_waits_for_pcm_already_submitted_to_dac() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(TrackId::new(2));

    let output = FixedAudioOutput::new(0.0);
    let output_clock = output.clock_handle();
    output_clock.set_output_timing(Duration::ZERO, Duration::from_millis(100));
    pipeline.install_audio_output_for_tests(Box::new(output));

    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainingOutput {
            buffer_level_ms: 100.0,
            playback_requested: false,
        }
    );

    output_clock.set_output_timing(Duration::from_millis(100), Duration::from_millis(100));
    assert_eq!(
        pipeline.audio_eof_drain_state(),
        AudioEofDrainState::DrainedOutput {
            playback_requested: false,
        }
    );
}

#[test]
fn audio_buffer_clear_generation_boundary_records_ack_generation() {
    let mut pipeline = PlaybackPipeline::default();

    assert_eq!(pipeline.audio_buffer_clear_generation(), 0);

    pipeline.mark_audio_buffer_clear_ack(7);

    assert_eq!(pipeline.audio_buffer_clear_generation(), 7);
}

#[test]
fn no_audio_monotonic_fallback_counts_position_from_anchor() {
    let mut pipeline = PlaybackPipeline::default();
    let anchored_at = Instant::now();
    let initial_position = Duration::from_millis(100);

    pipeline.start_monotonic_media_clock(initial_position, anchored_at, PlaybackRate::NORMAL);

    assert_eq!(
        pipeline.monotonic_media_position(anchored_at + Duration::from_millis(40)),
        Some(Duration::from_millis(140))
    );
}

#[test]
fn no_audio_monotonic_fallback_scales_position_by_playback_rate() {
    let anchored_at = Instant::now();
    let initial_position = Duration::from_millis(100);
    let two_x_rate = PlaybackRate::new(2.0).expect("2x playback rate must validate");
    let half_x_rate = PlaybackRate::new(0.5).expect("0.5x playback rate must validate");

    let mut fast_pipeline = PlaybackPipeline::default();
    fast_pipeline.start_monotonic_media_clock(initial_position, anchored_at, two_x_rate);

    assert_eq!(
        fast_pipeline.monotonic_media_position(anchored_at + Duration::from_millis(40)),
        Some(Duration::from_millis(180))
    );

    let mut slow_pipeline = PlaybackPipeline::default();
    slow_pipeline.start_monotonic_media_clock(initial_position, anchored_at, half_x_rate);

    assert_eq!(
        slow_pipeline.monotonic_media_position(anchored_at + Duration::from_millis(40)),
        Some(Duration::from_millis(120))
    );
}

#[test]
fn no_audio_monotonic_deadline_mapping_preserves_anchor_rounding_phase() {
    let empty_pipeline = PlaybackPipeline::default();
    let now = Instant::now();
    assert_eq!(
        empty_pipeline.monotonic_media_position_after_wall_delay(now, Duration::from_nanos(1)),
        None
    );
    assert_eq!(
        empty_pipeline.monotonic_wall_delay_until_media_deadline(now, Duration::from_nanos(1)),
        None
    );

    let one_and_half_rate = PlaybackRate::new(1.5).expect("1.5x playback rate must validate");
    let mut pipeline = PlaybackPipeline::default();
    pipeline.start_monotonic_media_clock(Duration::ZERO, now, one_and_half_rate);
    let current_time = now + Duration::from_nanos(1);

    assert_eq!(
        pipeline.monotonic_media_position(current_time),
        Some(Duration::from_nanos(1))
    );
    assert_eq!(
        pipeline.monotonic_media_position_after_wall_delay(current_time, Duration::from_nanos(1)),
        Some(Duration::from_nanos(3))
    );
    assert_eq!(
        pipeline.monotonic_wall_delay_until_media_deadline(current_time, Duration::from_nanos(3)),
        Some(Duration::from_nanos(1))
    );
}

#[test]
fn no_audio_monotonic_fallback_boundary_rates_saturate_without_wrapping() {
    let anchored_at = Instant::now();
    let near_max_position = Duration::MAX.saturating_sub(Duration::from_nanos(1));

    let mut max_rate_pipeline = PlaybackPipeline::default();
    max_rate_pipeline.start_monotonic_media_clock(
        near_max_position,
        anchored_at,
        PlaybackRate::MAX,
    );

    assert_eq!(
        max_rate_pipeline.monotonic_media_position(anchored_at + Duration::from_secs(1)),
        Some(Duration::MAX)
    );

    let mut min_rate_pipeline = PlaybackPipeline::default();
    min_rate_pipeline.start_monotonic_media_clock(Duration::ZERO, anchored_at, PlaybackRate::MIN);

    assert_eq!(
        min_rate_pipeline.monotonic_media_position(anchored_at + Duration::from_millis(4)),
        Some(Duration::from_millis(1))
    );
}

#[test]
fn installing_audio_clock_clears_monotonic_fallback_anchor() {
    let mut pipeline = PlaybackPipeline::default();
    let anchored_at = Instant::now();
    let clock = Arc::new(FixedAudioClock::new(Duration::from_millis(12), 3));

    pipeline.start_monotonic_media_clock(Duration::from_secs(3), anchored_at, PlaybackRate::NORMAL);
    assert!(pipeline.monotonic_media_position(anchored_at).is_some());

    pipeline.install_audio_clock(Arc::clone(&clock) as Arc<dyn PlayerAudioClock>);

    assert!(pipeline.has_audio_clock());
    assert_eq!(pipeline.audio_clock_now(), Duration::from_millis(12));
    assert_eq!(pipeline.audio_clock_underrun_callbacks(), 3);
    assert!(pipeline.monotonic_media_position(anchored_at).is_none());
    assert!(pipeline.reset_audio_clock());
    assert_eq!(clock.reset_count(), 1);
    assert_eq!(pipeline.audio_clock_now(), Duration::ZERO);
}

#[test]
fn audio_clock_mapping_scales_output_progress_by_playback_rate() {
    let mut pipeline = PlaybackPipeline::default();
    let clock = Arc::new(FixedAudioClock::new(Duration::ZERO, 0));
    pipeline.install_audio_clock(Arc::clone(&clock) as Arc<dyn PlayerAudioClock>);
    pipeline.reanchor_audio_clock_media_mapping(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    clock.set_now(Duration::from_secs(1));

    assert_eq!(
        pipeline.media_position_from_audio_clock(),
        Duration::from_secs(12)
    );
}

/// Помогает сравнивать mapping позиции с допуском на f64 интерполяцию хвоста.
fn assert_media_position_close(actual: Duration, expected: Duration) {
    let delta = actual.abs_diff(expected);
    assert!(
        delta <= Duration::from_millis(1),
        "media position {actual:?} должна быть близка к {expected:?}"
    );
}

#[test]
fn rate_change_reanchor_accounts_written_output_tail_at_old_rate() {
    let mut pipeline = PlaybackPipeline::default();
    // Ring уже пуст, но 200 ms submitted PCM находятся между callback и DAC.
    let output = FixedAudioOutput::new(0.0);
    let clock = output.clock_handle();
    pipeline.install_audio_output_for_tests(Box::new(output));
    pipeline.install_audio_clock(clock.clone() as Arc<dyn PlayerAudioClock>);

    // Играем на 1.0x: anchor media 10s на output 60s.
    clock.set_output_timing(Duration::from_secs(60), Duration::from_millis(60_200));
    pipeline.reanchor_audio_clock_media_mapping(Duration::from_secs(10), PlaybackRate::NORMAL);

    // Смена на 2.0x при 200 ms записанного, но не проигранного output-а.
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    // В момент смены позиция не прыгает.
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_secs(10),
    );

    // Внутри хвоста media идёт со СТАРЫМ темпом: +100 ms output = +100 ms media.
    clock.set_now(Duration::from_millis(60_100));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(10_100),
    );

    // Конец хвоста точен: 200 ms output старого rate = +200 ms media.
    clock.set_now(Duration::from_millis(60_200));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(10_200),
    );

    // После хвоста работает новый rate: ещё +1s output = +2s media,
    // а не замороженная ошибка `tail × (new − old)`.
    clock.set_now(Duration::from_millis(61_200));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        Duration::from_millis(12_200),
    );
}

#[test]
fn rate_change_reanchor_without_buffered_output_matches_plain_reanchor() {
    let mut pipeline = PlaybackPipeline::default();
    let output = FixedAudioOutput::new(0.0);
    let clock = output.clock_handle();
    pipeline.install_audio_output_for_tests(Box::new(output));
    pipeline.install_audio_clock(clock.clone() as Arc<dyn PlayerAudioClock>);

    clock.set_now(Duration::from_secs(5));
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    clock.set_now(Duration::from_secs(6));
    assert_eq!(
        pipeline.media_position_from_audio_clock(),
        Duration::from_secs(12)
    );
}

#[test]
fn repeated_rate_change_inside_tail_keeps_mapping_monotonic_and_bounded() {
    let mut pipeline = PlaybackPipeline::default();
    let output = FixedAudioOutput::new(0.0);
    let clock = output.clock_handle();
    pipeline.install_audio_output_for_tests(Box::new(output));
    pipeline.install_audio_clock(clock.clone() as Arc<dyn PlayerAudioClock>);

    clock.set_output_timing(Duration::from_secs(60), Duration::from_millis(60_200));
    pipeline.reanchor_audio_clock_media_mapping(Duration::from_secs(10), PlaybackRate::NORMAL);
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        Duration::from_secs(10),
        PlaybackRate::new(2.0).expect("2x playback rate should be valid"),
    );

    // Вторая смена в середине ещё не проигранного хвоста.
    clock.set_output_timing(Duration::from_millis(60_100), Duration::from_millis(60_300));
    let mid_tail_position = pipeline.media_position_from_audio_clock();
    pipeline.reanchor_audio_clock_media_mapping_for_rate_change(
        mid_tail_position,
        PlaybackRate::new(4.0).expect("4x playback rate should be valid"),
    );

    // Позиция в момент смены сохраняется.
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        mid_tail_position,
    );

    // Хвост (ещё 200 ms output) заканчивается на значении старого mapping,
    // а не прыгает на новый rate сразу.
    clock.set_now(Duration::from_millis(60_300));
    let tail_end_position = pipeline.media_position_from_audio_clock();
    assert!(tail_end_position >= mid_tail_position);
    assert!(
        tail_end_position <= Duration::from_millis(10_500),
        "конец хвоста {tail_end_position:?} не должен применять 4x к старому output-у"
    );

    // После хвоста media идёт с новым 4x темпом.
    clock.set_now(Duration::from_millis(61_300));
    assert_media_position_close(
        pipeline.media_position_from_audio_clock(),
        tail_end_position + Duration::from_secs(4),
    );
}

#[test]
fn passthrough_audio_history_is_bounded_and_keeps_latest_samples() {
    let mut pipeline = PlaybackPipeline::default();
    // 100 Hz stereo: бюджет 600 ms = 60 frames = 120 samples.
    let sample_rate = 100;
    let channels = 2;

    let old_chunk: Vec<f32> = (0..100).map(|i| i as f32).collect();
    let new_chunk: Vec<f32> = (100..160).map(|i| i as f32).collect();
    pipeline.record_passthrough_audio_history(&old_chunk, sample_rate, channels);
    pipeline.record_passthrough_audio_history(&new_chunk, sample_rate, channels);

    let history = pipeline.take_passthrough_audio_history_for_priming(sample_rate, channels);
    assert_eq!(
        history.len(),
        120,
        "история должна быть обрезана до бюджета"
    );
    assert_eq!(
        history.last().copied(),
        Some(159.0),
        "история должна хранить последние samples"
    );
    assert_eq!(
        history.len() % channels as usize,
        0,
        "история frame-aligned"
    );

    // Повторный take пуст: история одноразовая для одного прайминга.
    assert!(
        pipeline
            .take_passthrough_audio_history_for_priming(sample_rate, channels)
            .is_empty()
    );
}

#[test]
fn passthrough_audio_history_with_mismatched_spec_is_not_used_for_priming() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.record_passthrough_audio_history(&[1.0, 2.0, 3.0, 4.0], 48_000, 2);

    assert!(
        pipeline
            .take_passthrough_audio_history_for_priming(44_100, 2)
            .is_empty(),
        "история чужого PCM format не должна праймить processor"
    );
}

#[test]
fn seek_clock_reset_restores_base_and_clears_fallback_sample_window() {
    let mut pipeline = PlaybackPipeline::default();
    let anchored_at = Instant::now();
    let target_position = Duration::from_secs(9);

    pipeline.set_media_clock_base(Duration::from_secs(2));
    pipeline.start_monotonic_media_clock(Duration::from_secs(4), anchored_at, PlaybackRate::NORMAL);
    pipeline.reset_audio_clock_sample(Duration::from_secs(3), anchored_at);

    pipeline.reset_clocks_for_seek(target_position);

    assert_eq!(pipeline.media_clock_base(), target_position);
    assert!(pipeline.monotonic_media_position(anchored_at).is_none());
    assert!(pipeline.audio_clock_stalled_for(Instant::now()) < Duration::from_secs(1));
}

#[test]
fn stalled_audio_duration_is_measured_from_last_changed_sample() {
    let mut pipeline = PlaybackPipeline::default();
    let first_observed_at = Instant::now();
    let unchanged_observed_at = first_observed_at + Duration::from_millis(20);
    let changed_observed_at = first_observed_at + Duration::from_millis(40);

    pipeline.reset_audio_clock_sample(Duration::ZERO, first_observed_at);
    pipeline.note_audio_clock_sample(Duration::ZERO, unchanged_observed_at);

    assert_eq!(
        pipeline.audio_clock_stalled_for(first_observed_at + Duration::from_millis(30)),
        Duration::from_millis(30)
    );

    pipeline.note_audio_clock_sample(Duration::from_millis(5), changed_observed_at);

    assert_eq!(
        pipeline.audio_clock_stalled_for(changed_observed_at + Duration::from_millis(15)),
        Duration::from_millis(15)
    );
}

#[test]
fn demux_boundaries_preserve_eof_and_seek_results() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(Vec::new())),
        None,
        None,
        Vec::new(),
    );

    let packet_result = pipeline
        .demux_next_packet()
        .expect("installed demuxer должен быть видим через boundary")
        .expect("fake demuxer не должен возвращать ошибку");
    assert!(packet_result.is_none());

    let seek_result = pipeline
        .seek_demuxer(DemuxSeekRequest::accurate(Duration::from_secs(3)))
        .expect("installed demuxer должен принять seek через boundary")
        .expect("fake demuxer должен принять accurate seek");
    assert_eq!(seek_result.actual_position, MediaTime::from_secs(3));
}

#[test]
fn selected_track_boundaries_manage_ids_requirement_and_clear_only_selection() {
    let mut pipeline = PlaybackPipeline::default();
    let video_track_id = TrackId::new(10);
    let audio_track_id = TrackId::new(20);
    let source_tracks = vec![
        source_slot_track(video_track_id, TrackKind::Video, "V_VP9"),
        source_slot_track(audio_track_id, TrackKind::Audio, "A_OPUS"),
    ];
    let initial_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
    let refined_requirement = initial_requirement.clone().with_resolution(1920, 1080);

    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(source_tracks.clone())),
        None,
        Some("selected-track-test".to_owned()),
        source_tracks,
    );
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
        audio_track_id,
        Duration::from_millis(1),
        None,
        None,
        pipeline.seek_generation(),
        Bytes::from_static(b"audio"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(2),
        pipeline.seek_generation(),
        Bytes::from_static(b"video"),
        true,
    ));

    pipeline.select_audio_track(audio_track_id);
    pipeline.select_video_track(video_track_id, initial_requirement.clone());

    assert_eq!(pipeline.selected_audio_track_id(), Some(audio_track_id));
    assert_eq!(pipeline.selected_video_track_id(), Some(video_track_id));
    assert!(pipeline.has_selected_audio_track());
    assert!(pipeline.has_selected_video_track());
    assert!(pipeline.video_packet_belongs_to_selected_track(video_track_id));
    assert!(!pipeline.video_packet_belongs_to_selected_track(TrackId::new(99)));
    assert_eq!(
        pipeline.active_video_requirement(),
        Some(&initial_requirement)
    );

    pipeline.set_active_video_requirement(refined_requirement.clone());

    assert_eq!(
        pipeline.active_video_requirement(),
        Some(&refined_requirement)
    );

    pipeline.clear_selected_tracks();

    assert!(pipeline.selected_audio_track_id().is_none());
    assert!(pipeline.selected_video_track_id().is_none());
    assert!(!pipeline.has_selected_audio_track());
    assert!(!pipeline.has_selected_video_track());
    assert!(pipeline.active_video_requirement().is_none());
    assert_eq!(pipeline.pending_audio_packet_len(), 1);
    assert_eq!(pipeline.pending_video_packet_len(), 1);
    assert!(pipeline.has_demuxer());
    assert_eq!(pipeline.track_count(), 2);
}

#[test]
fn video_frame_timing_first_observation_only_records_pts() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );
}

#[test]
fn video_frame_timing_valid_delta_updates_estimate_with_legacy_smoothing() {
    let mut pipeline = PlaybackPipeline::default();
    let observed_frame_duration = Duration::from_millis(20);

    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));
    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration);

    let old_micros = DEFAULT_VIDEO_FRAME_DURATION.as_micros() as u64;
    let observed_micros = observed_frame_duration.as_micros() as u64;
    let expected_micros = (old_micros.saturating_mul(7) + observed_micros) / 8;

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        Duration::from_micros(expected_micros.max(1))
    );
}

#[test]
fn video_frame_timing_ignores_out_of_range_deltas() {
    let mut pipeline = PlaybackPipeline::default();
    let first_pts = Duration::from_secs(10);
    let too_small_pts = first_pts + MIN_OBSERVED_VIDEO_FRAME_DURATION / 2;
    let too_large_pts = too_small_pts + MAX_OBSERVED_VIDEO_FRAME_DURATION * 2;

    pipeline.observe_decoded_video_frame_pts(first_pts);
    pipeline.observe_decoded_video_frame_pts(too_small_pts);
    pipeline.observe_decoded_video_frame_pts(too_large_pts);

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );
}

#[test]
fn video_frame_timing_reset_restores_default_and_clears_previous_pts() {
    let mut pipeline = PlaybackPipeline::default();
    let observed_frame_duration = Duration::from_millis(20);

    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));
    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration);
    assert_ne!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );

    pipeline.reset_video_frame_timing_estimator();
    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration * 2);

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );
}

#[test]
fn pending_packet_queue_boundaries_preserve_fifo_order_and_lengths() {
    let mut pipeline = PlaybackPipeline::default();
    let audio_track_id = TrackId::new(20);
    let video_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();

    assert!(pipeline.pending_audio_packet_is_empty());
    assert!(pipeline.pending_video_packet_is_empty());

    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
        audio_track_id,
        Duration::from_millis(10),
        None,
        None,
        generation,
        Bytes::from_static(b"audio-10"),
    ));
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
        audio_track_id,
        Duration::from_millis(20),
        None,
        None,
        generation,
        Bytes::from_static(b"audio-20"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(30),
        generation,
        Bytes::from_static(b"video-30"),
        true,
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(40),
        generation,
        Bytes::from_static(b"video-40"),
        false,
    ));

    assert_eq!(pipeline.pending_audio_packet_len(), 2);
    assert_eq!(pipeline.pending_video_packet_len(), 2);
    assert_eq!(
        pipeline
            .front_pending_video_packet()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(30))
    );
    assert_eq!(
        pipeline
            .pop_pending_audio_packet_front()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(10))
    );
    assert_eq!(
        pipeline
            .pop_pending_audio_packet_front()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(20))
    );
    assert_eq!(
        pipeline
            .pop_pending_video_packet_front()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(30))
    );
    assert_eq!(
        pipeline
            .pop_pending_video_packet_front()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(40))
    );
    assert!(pipeline.pending_audio_packet_is_empty());
    assert!(pipeline.pending_video_packet_is_empty());
}

#[test]
fn begin_seek_generation_saturates_without_wrapping() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.set_seek_generation_for_tests(u64::MAX - 1);

    assert_eq!(pipeline.begin_seek_generation(), u64::MAX);
    assert_eq!(pipeline.seek_generation(), u64::MAX);
    assert_eq!(pipeline.begin_seek_generation(), u64::MAX);
    assert_eq!(pipeline.seek_generation(), u64::MAX);
    assert!(pipeline.packet_generation_is_current(u64::MAX));
    assert!(!pipeline.packet_generation_is_current(u64::MAX - 1));
}

#[test]
fn clear_pending_packets_for_seek_does_not_touch_selection_or_decoder_state() {
    let mut pipeline = PlaybackPipeline::default();
    let video_track_id = TrackId::new(10);
    let audio_track_id = TrackId::new(20);
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
    let generation = pipeline.seek_generation();

    pipeline.select_audio_track(audio_track_id);
    pipeline.select_video_track(video_track_id, requirement.clone());
    pipeline.mark_video_decoder_bootstrapped();
    pipeline.note_video_packet_sent_to_decoder();
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new(
        audio_track_id,
        Duration::from_millis(10),
        None,
        None,
        generation,
        Bytes::from_static(b"audio"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(20),
        generation,
        Bytes::from_static(b"video"),
        true,
    ));

    pipeline.clear_pending_packets_for_seek();

    assert!(pipeline.pending_audio_packet_is_empty());
    assert!(pipeline.pending_video_packet_is_empty());
    assert_eq!(pipeline.selected_audio_track_id(), Some(audio_track_id));
    assert_eq!(pipeline.selected_video_track_id(), Some(video_track_id));
    assert_eq!(pipeline.active_video_requirement(), Some(&requirement));
    assert!(!pipeline.video_decoder_needs_keyframe());
    assert_eq!(pipeline.video_decode_in_flight_packets(), 1);
}

#[test]
fn clear_video_queues_returns_only_queued_resource_handles() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));
    pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(50), 3));
    pipeline.replace_seek_preroll_fallback_video_frame(decoded_frame_for_tests(
        Duration::from_millis(40),
        4,
    ));

    let released_resource_handles = pipeline.clear_video_queues();

    assert_eq!(
        released_resource_handles,
        vec![
            video_core::FrameResourceHandle(1),
            video_core::FrameResourceHandle(2)
        ]
    );
    assert!(pipeline.video_present_queue_is_empty());
    assert_eq!(
        pipeline
            .present_video_frame()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(3))
    );
    assert_eq!(
        pipeline
            .take_seek_preroll_fallback_video_frame()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(4))
    );
}

/// Fake demuxer для проверки source-slot boundaries без реального container backend-а.
struct SourceSlotFakeDemuxer {
    /// Metadata tracks, которые demuxer отдаёт по neutral contract.
    track_infos: Vec<TrackInfo>,
}

impl SourceSlotFakeDemuxer {
    /// Создаёт fake demuxer с фиксированным набором tracks.
    fn new(track_infos: Vec<TrackInfo>) -> Self {
        Self { track_infos }
    }
}

impl Demuxer for SourceSlotFakeDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.track_infos
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }

    fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
        Ok(None)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Создаёт минимальный track metadata для проверки source-slot getters.
fn source_slot_track(track_id: TrackId, kind: TrackKind, codec_id: &str) -> TrackInfo {
    TrackInfo {
        id: track_id,
        kind,
        codec_id: codec_id.to_owned(),
        codec_private: None,
        time_base: media_core::TimeBase::new(1, 1_000),
        duration: Some(Duration::from_secs(30)),
        sample_rate: (kind == TrackKind::Audio).then_some(48_000),
        channels: (kind == TrackKind::Audio).then_some(2),
        video: None,
    }
}
