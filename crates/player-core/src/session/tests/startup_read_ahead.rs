//! Регрессия «вечный Buffering на старте»: audio read-ahead high-water не должен
//! душить demux, пока video decoder ещё копит глубину для первого кадра.
//!
//! Сценарий — настоящий interleaved MKV (`two-audio-tracks.mkv`: VP8 + FLAC + Opus),
//! настоящий демуксер и настоящие аудио-декодеры, ВИДЕО ВКЛЮЧЕНО. Fake video decoder
//! (как H.264 с B-frames/DPB) отдаёт первый кадр только после `FIRST_FRAME_AFTER_PACKETS`
//! принятых packets. Fake audio output накапливает записанный звук в buffer level и не
//! расходует его, пока stream не запущен (как реальное устройство до `play`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio_core::{
    AudioOutputClockTiming, AudioOutputDeviceRequest, AudioOutputDeviceRoute,
    AudioOutputStreamHealth, AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport,
    CreatedAudioOutput,
};

use super::audio_track_switch::harness::{install_media, open_fixture};
use super::test_support::{
    ScriptedAudioOutput, ScriptedAudioOutputHandle, SharedFakeVideoDecoderThread,
    decoded_frame_for_tests,
};
use super::*;
use crate::{PlaybackIntent, StartedVideoBackend};

/// Сколько video packets decoder обязан принять до первого кадра (reorder/DPB глубина).
///
/// Фикстура — 10 fps, а decode-ahead относительно ещё не запущенного audio clock-а пускает к
/// decoder-у не больше ~6 packets (0.5 с), поэтому глубина 5 — максимум, достижимый вообще.
/// При этом обычные 200 мс аудио набираются уже за 2-3 video packets: без исправления
/// demux встаёт раньше, чем decoder получит 5-й packet.
const FIRST_FRAME_AFTER_PACKETS: usize = 5;

/// Бюджет tick-ов сценария: с запасом на все стадии, но конечный (без бесконечного ожидания).
const TICK_BUDGET: usize = 400;

/// Output, у которого уровень буфера растёт с записанным звуком (ScriptedAudioOutput держит
/// уровень константой, а гейт high-water смотрит именно на уровень).
struct FillingAudioOutput {
    inner: Box<dyn PlayerAudioOutput>,
    handle: ScriptedAudioOutputHandle,
    sample_rate: u32,
    channels: usize,
}

impl PlayerAudioOutput for FillingAudioOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let report = self.inner.write_samples(samples, intent)?;
        let written_ms =
            (samples.len() / self.channels) as f64 * 1000.0 / f64::from(self.sample_rate);
        self.handle
            .set_buffer_level_ms(self.handle.buffer_level_ms() + written_ms);
        Ok(report)
    }

    fn play(&mut self) -> anyhow::Result<()> {
        self.inner.play()
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        self.inner.pause_and_freeze_clock()
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.inner.clear_buffer_for_seek(generation)
    }

    fn set_volume(&mut self, volume: f32) {
        self.inner.set_volume(volume);
    }

    fn buffer_level_ms(&self) -> f64 {
        self.inner.buffer_level_ms()
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.inner.clock()
    }

    fn stream_health(&self) -> AudioOutputStreamHealth {
        self.inner.stream_health()
    }
}

/// Factory, отдающая `FillingAudioOutput` и запоминающая handle для проверок.
struct FillingAudioOutputFactory {
    last_handle: Mutex<Option<ScriptedAudioOutputHandle>>,
}

impl AudioOutputFactory for FillingAudioOutputFactory {
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        _device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        let channels = spec.channels() as usize;
        let (inner, handle) = ScriptedAudioOutput::new(0.0, None)
            .with_input_channels(channels)
            .into_parts();
        *self.last_handle.lock().expect("output handle lock") = Some(handle.clone());
        let output = FillingAudioOutput {
            inner,
            handle,
            sample_rate: spec.sample_rate,
            channels,
        };
        Ok(CreatedAudioOutput::new(
            Box::new(output),
            AudioOutputDeviceRoute::SystemDefault,
        ))
    }
}

/// Стенд: сессия с настоящим MKV, видео включено, дефолтный tick config.
pub(super) struct StartupHarness {
    pub(super) session: PlayerSession,
    pub(super) video_decoder: SharedFakeVideoDecoderThread,
    output_factory: Arc<FillingAudioOutputFactory>,
    pub(super) first_frame_published: bool,
    /// Сколько packets decoder обязан принять до первого кадра.
    first_frame_after_packets: usize,
    acked_packets: usize,
    pub(super) frames_presented: u64,
}

impl StartupHarness {
    fn new() -> Self {
        Self::with_fixture("two-audio-tracks.mkv", FIRST_FRAME_AFTER_PACKETS)
    }

    /// Стенд на заданной фикстуре; decoder выдаёт первый кадр после `first_frame_after_packets`.
    pub(super) fn with_fixture(fixture_name: &str, first_frame_after_packets: usize) -> Self {
        let output_factory = Arc::new(FillingAudioOutputFactory {
            last_handle: Mutex::new(None),
        });
        let mut session = PlayerSession::with_audio_factories(
            Arc::new(audio::decoder::ProductionAudioDecoderFactory::default()),
            Arc::clone(&output_factory) as Arc<dyn AudioOutputFactory>,
        );
        install_media(
            &mut session,
            Box::new(open_fixture(fixture_name)),
            PlaybackIntent::StartPlaying,
        );
        let video_decoder = SharedFakeVideoDecoderThread::new();
        session.set_video_backend(StartedVideoBackend::from_decoder_thread(
            "startup-read-ahead-test-backend",
            video_decoder.clone(),
        ));
        Self {
            session,
            video_decoder,
            output_factory,
            first_frame_published: false,
            first_frame_after_packets,
            acked_packets: 0,
            frames_presented: 0,
        }
    }

    pub(super) fn output(&self) -> Option<ScriptedAudioOutputHandle> {
        self.output_factory
            .last_handle
            .lock()
            .expect("output handle lock")
            .clone()
    }

    /// Один tick с дефолтным config-ом; decoder «созревает» после N принятых packets.
    pub(super) fn tick(&mut self) {
        self.tick_with_config(PlayerTickConfig::default());
    }

    /// Один tick с явным config-ом (для пределов запаса сжатого звука).
    pub(super) fn tick_with_config(&mut self, tick_config: PlayerTickConfig) {
        // Decoder принимает (ack-ает) каждый packet сразу, как реальный: пакеты оседают
        // в его DPB, а кадр появляется позже, поэтому in-flight лимит не душит отправку.
        let sent_packets = self.video_decoder.sent_packets().len();
        self.video_decoder
            .add_completed_packet_count(sent_packets - self.acked_packets);
        self.acked_packets = sent_packets;
        if !self.first_frame_published
            && self.video_decoder.sent_packets().len() >= self.first_frame_after_packets
        {
            self.video_decoder
                .push_decoded_frame(decoded_frame_for_tests(Duration::ZERO, 1));
            self.first_frame_published = true;
        }
        let result = self
            .session
            .tick(PlayerTickContext::with_config(Instant::now(), tick_config));
        self.frames_presented += result.video_frames_presented;
        // Звуковая карта «играет» только после запуска stream-а: часы идут от play.
        if let Some(output) = self.output()
            && output.play_count() > 0
        {
            let audible = self.session.audio_clock_now() + Duration::from_millis(5);
            output.clock.set_output_timing(audible, audible);
        }
    }
}

/// Старт с дефолтным config-ом доходит до Playing, звук идёт на output, кадр показан.
#[test]
fn startup_with_slow_first_video_frame_reaches_playing_with_audio_and_video() {
    let mut harness = StartupHarness::new();
    assert_eq!(harness.session.playback_state(), PlaybackState::Buffering);

    for _ in 0..TICK_BUDGET {
        harness.tick();
        if harness.session.playback_state() == PlaybackState::Playing
            && harness.frames_presented > 0
        {
            break;
        }
    }

    assert_eq!(
        harness.session.playback_state(),
        PlaybackState::Playing,
        "sent video packets: {}",
        harness.video_decoder.sent_packets().len()
    );
    assert!(
        harness.video_decoder.sent_packets().len() >= FIRST_FRAME_AFTER_PACKETS,
        "decoder must have been fed enough packets to produce the first frame"
    );
    assert!(
        harness.frames_presented > 0,
        "first video frame must be presented"
    );
    let output = harness.output().expect("audio output created");
    assert!(output.play_count() > 0, "audio stream must be started");
    assert!(
        !output.written_samples().is_empty(),
        "decoded audio must reach the output"
    );
}

/// Кадр не приходит никогда (decoder принимает packets, но молчит): декодированный звук
/// по-прежнему упирается в стартовый потолок, сжатый — в запас, и память ограничена.
///
/// Раньше demux вставал на потолке, и тест закреплял вечный `Buffering` как норму. Теперь
/// чтение идёт дальше в пределах запаса (10 с), а фикстура короче (3 с): файл кончается
/// раньше, чем запас исчерпан, и старт ждёт кадр уже внутри EOF drain.
#[test]
fn read_ahead_memory_stays_bounded_when_first_frame_never_comes() {
    let mut harness = StartupHarness::new();
    // Decoder «зависает»: первый кадр не придёт никогда.
    harness.first_frame_published = true;

    for _ in 0..TICK_BUDGET {
        harness.tick();
    }

    let config = PlayerTickConfig::default();
    let level_ms = harness
        .output()
        .expect("audio output created")
        .buffer_level_ms();
    assert!(
        level_ms >= config.audio_startup_read_ahead_cap_ms - 500.0,
        "read-ahead must go far beyond the normal high-water while starved: {level_ms}"
    );
    assert!(
        level_ms <= config.audio_startup_read_ahead_cap_ms + 200.0,
        "decoded audio must stay bounded by the startup cap: {level_ms}"
    );
    let audio_backlog = harness.session.pipeline.pending_audio_backlog();
    assert!(
        audio_backlog.media_span < config.startup_video_wait_max_audio_backlog,
        "compressed backlog must stay within its limit: {audio_backlog:?}"
    );
    assert!(
        audio_backlog.encoded_bytes < config.startup_video_wait_max_audio_backlog_bytes,
        "compressed backlog must stay within its byte limit: {audio_backlog:?}"
    );
    assert!(
        matches!(
            harness.session.playback_state(),
            PlaybackState::Buffering | PlaybackState::Draining
        ),
        "without a frame and with backlog room the start keeps waiting: {:?}",
        harness.session.playback_state()
    );
    assert_eq!(harness.frames_presented, 0);
}

/// Кадра нет, а запас сжатого звука исчерпан: ждать картинку дальше нельзя — звук
/// стартует первым (решение владельца, сессия 03e моста). Память при этом ограничена.
#[test]
fn first_frame_that_never_comes_starts_audio_first_after_backlog_limit() {
    let backlog_span_limit = Duration::from_millis(300);
    let config = PlayerTickConfig {
        startup_video_wait_max_audio_backlog: backlog_span_limit,
        ..PlayerTickConfig::default()
    };
    let mut harness = StartupHarness::new();
    // Decoder «зависает»: первый кадр не придёт никогда.
    harness.first_frame_published = true;

    for _ in 0..TICK_BUDGET {
        harness.tick_with_config(config);
        if harness.session.playback_state() == PlaybackState::Playing {
            break;
        }
    }

    assert_eq!(
        harness.session.playback_state(),
        PlaybackState::Playing,
        "exhausted backlog must start playback without a frame, not hang in Buffering"
    );
    let output = harness.output().expect("audio output created");
    assert!(output.play_count() > 0, "audio stream must be started");
    assert!(
        !output.written_samples().is_empty(),
        "decoded audio must reach the output"
    );
    assert_eq!(harness.frames_presented, 0, "there is no frame to show");
    assert!(
        output.buffer_level_ms() <= config.audio_startup_read_ahead_cap_ms + 200.0,
        "decoded audio must stay bounded by the startup cap: {}",
        output.buffer_level_ms()
    );
    // Предел меряется между началами packets: сверх него — не больше одного packet-а.
    let audio_backlog = harness.session.pipeline.pending_audio_backlog();
    assert!(
        audio_backlog.media_span < backlog_span_limit + Duration::from_millis(100),
        "compressed backlog must stop at its limit: {audio_backlog:?}"
    );
    assert!(
        audio_backlog.media_span >= backlog_span_limit,
        "audio-first start must happen only after the backlog limit: {audio_backlog:?}"
    );
}

/// Ветка «звук выбран, output-а ещё нет, а сжатый packet ждёт» блокирует чтение только
/// внутри одного tick-а: тот же tick декодирует packet и лениво создаёт output, поэтому
/// к концу tick-а такого состояния не бывает — вечной блокировки нет (ревью 03e).
#[test]
fn absent_output_never_keeps_compressed_audio_across_a_tick() {
    let mut harness = StartupHarness::new();
    // Кадр не придёт: весь прогон — голодный preroll.
    harness.first_frame_published = true;

    for tick in 0..TICK_BUDGET {
        harness.tick();
        let output_absent = harness.session.audio_buffer_level_ms().is_none();
        let pending_audio = harness.session.pipeline.pending_audio_packet_len();
        assert!(
            !(output_absent && pending_audio > 0),
            "tick {tick}: {pending_audio} compressed packets wait without an output"
        );
    }
    assert!(
        harness.output().is_some(),
        "the first decoded packet must create the output"
    );
}
