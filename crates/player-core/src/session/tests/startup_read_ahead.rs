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
struct StartupHarness {
    session: PlayerSession,
    video_decoder: SharedFakeVideoDecoderThread,
    output_factory: Arc<FillingAudioOutputFactory>,
    first_frame_published: bool,
    acked_packets: usize,
    frames_presented: u64,
}

impl StartupHarness {
    fn new() -> Self {
        let output_factory = Arc::new(FillingAudioOutputFactory {
            last_handle: Mutex::new(None),
        });
        let mut session = PlayerSession::with_audio_factories(
            Arc::new(audio::decoder::ProductionAudioDecoderFactory::default()),
            Arc::clone(&output_factory) as Arc<dyn AudioOutputFactory>,
        );
        install_media(
            &mut session,
            Box::new(open_fixture("two-audio-tracks.mkv")),
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
            acked_packets: 0,
            frames_presented: 0,
        }
    }

    fn output(&self) -> Option<ScriptedAudioOutputHandle> {
        self.output_factory
            .last_handle
            .lock()
            .expect("output handle lock")
            .clone()
    }

    /// Один tick с дефолтным config-ом; decoder «созревает» после N принятых packets.
    fn tick(&mut self) {
        // Decoder принимает (ack-ает) каждый packet сразу, как реальный: пакеты оседают
        // в его DPB, а кадр появляется позже, поэтому in-flight лимит не душит отправку.
        let sent_packets = self.video_decoder.sent_packets().len();
        self.video_decoder
            .add_completed_packet_count(sent_packets - self.acked_packets);
        self.acked_packets = sent_packets;
        if !self.first_frame_published
            && self.video_decoder.sent_packets().len() >= FIRST_FRAME_AFTER_PACKETS
        {
            self.video_decoder
                .push_decoded_frame(decoded_frame_for_tests(Duration::ZERO, 1));
            self.first_frame_published = true;
        }
        let result = self.session.tick(PlayerTickContext::with_config(
            Instant::now(),
            PlayerTickConfig::default(),
        ));
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

/// Пока кадра нет вообще, чтение вперёд идёт заметно дальше обычных 200 мс, но упирается
/// в стартовый потолок (память ограничена, demux снова блокируется).
#[test]
fn read_ahead_is_bounded_by_startup_cap_when_first_frame_never_comes() {
    let mut harness = StartupHarness::new();
    // Decoder «зависает»: первый кадр не придёт никогда, нужен только потолок.
    harness.first_frame_published = true;

    for _ in 0..TICK_BUDGET {
        harness.tick();
    }

    let cap_ms = PlayerTickConfig::default().audio_startup_read_ahead_cap_ms;
    let level_ms = harness
        .output()
        .expect("audio output created")
        .buffer_level_ms();
    assert!(
        level_ms >= cap_ms - 500.0,
        "read-ahead must go far beyond the normal high-water while starved: {level_ms}"
    );
    assert!(
        level_ms <= cap_ms + 200.0,
        "read-ahead must stay bounded by the startup cap ({cap_ms}): {level_ms}"
    );
    assert_eq!(harness.session.playback_state(), PlaybackState::Buffering);
}
