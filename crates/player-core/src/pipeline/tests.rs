use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use super::*;
use audio_core::{
    AudioOutputClockTiming, AudioOutputInputFrameCount, AudioOutputStreamFrameCount,
    AudioOutputWriteIntent,
};
use codec_core::VideoCodec;
use media_core::{MediaTime, TrackKind};

mod audio_boundaries;
mod audio_packet_window;
mod backlog_recovery;
mod clock_mapping;
mod demux_timing;
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

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
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

mod queues_media_slots;
mod seek_reset;

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

/// Создаёт neutral test spec из legacy rate/count параметров focused tests.
fn audio_output_spec_for_tests(sample_rate: u32, channels: u32) -> audio_core::AudioOutputSpec {
    audio_core::AudioOutputSpec::new(
        sample_rate,
        audio_core::AudioChannelLayout::from_channel_count(channels)
            .expect("test channel count must form a valid neutral layout"),
    )
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

    /// Возвращает neutral fake layout, согласованный с channel count-ом.
    fn channel_layout(&self) -> Option<audio_core::AudioChannelLayout> {
        audio_core::AudioChannelLayout::from_channel_count(self.channels).ok()
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
    /// Записывает все samples как одноканальные frames, принятые полностью.
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(samples.len()),
            AudioOutputStreamFrameCount::new(samples.len()),
        ))
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
