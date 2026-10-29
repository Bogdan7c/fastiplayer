//! Безоконный прогон: поток → открытие → `PlayerSession` с настоящими декодерами.
//!
//! Видео декодирует настоящий программный FFmpeg (`video-ffmpeg`), звук —
//! настоящий декодер (`audio`), выход звука — симулятор. «Кадр дошёл до показа» —
//! это `video_frames_presented` сессии и кадр в `current_video_frame`: ровно его
//! берёт рендер окна.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use player_core::{PlaybackState, PlayerEvent, PlayerSession, PlayerTickContext, PreparedMedia};
use pushed_media_source::{OpenProgress, PushedStreamDemuxer, PushedStreamOpener};

use super::simulated_audio::SimulatedAudioDevice;

/// Пауза между шагами плеера (настоящее время).
///
/// Время настоящее, а не виртуальное: часы видео без звука якорятся по
/// `Instant::now()`, и виртуальное время, бегущее быстрее реального, сдвигало бы
/// их относительно кадров. Звуковая карта-симулятор идёт по тем же часам.
const TICK: Duration = Duration::from_millis(5);
/// Предел шагов: ~10 с на ролик меньше секунды.
const MAX_TICKS: usize = 2_000;
/// Сколько ждать открытия потока.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Показанный кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedFrame {
    /// Время кадра.
    pub pts: Duration,
    /// Размер кадра.
    pub width: u32,
    /// Размер кадра.
    pub height: u32,
    /// Позиция плеера (часы звука) в момент показа.
    pub position: Duration,
}

/// Что увидел и услышал «зритель».
#[derive(Debug)]
pub struct PlaybackReport {
    /// Показанные кадры по порядку (каждый — один раз).
    pub frames: Vec<PresentedFrame>,
    /// Сумма `video_frames_presented` всех шагов.
    pub presented_count: u64,
    /// PCM, записанный в выход.
    pub audio_samples: Vec<f32>,
    /// Частота и каналы выхода.
    pub audio_format: Option<(u32, usize)>,
    /// Состояние в конце.
    pub final_state: PlaybackState,
    /// Ждал ли плеер данных (Buffering) после начала показа.
    pub buffered_after_start: bool,
    /// Ошибки плеера.
    pub errors: Vec<String>,
}

impl PlaybackReport {
    /// Длительность записанного звука.
    pub fn audio_duration(&self) -> Duration {
        let Some((sample_rate, channels)) = self.audio_format else {
            return Duration::ZERO;
        };
        let frames = self.audio_samples.len() / channels;
        Duration::from_secs_f64(frames as f64 / f64::from(sample_rate))
    }

    /// Громкость (RMS) записанного звука: у синуса 0,5–1 — заметно больше нуля.
    pub fn audio_rms(&self) -> f32 {
        if self.audio_samples.is_empty() {
            return 0.0;
        }
        let energy: f32 = self
            .audio_samples
            .iter()
            .map(|sample| sample * sample)
            .sum();
        (energy / self.audio_samples.len() as f32).sqrt()
    }

    /// Наибольшее расхождение «время кадра − позиция плеера» при показе.
    pub fn max_av_offset(&self) -> Duration {
        self.frames
            .iter()
            .map(|frame| frame.pts.abs_diff(frame.position))
            .max()
            .unwrap_or_default()
    }
}

/// Ждёт открытия потока (подготовка — вне потока плеера, ждать здесь можно).
pub fn open(opener: &mut PushedStreamOpener) -> PushedStreamDemuxer {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        match opener.poll_open().expect("stream opens") {
            OpenProgress::Ready(demuxer) => return demuxer,
            OpenProgress::Waiting(hint) => {
                assert!(Instant::now() < deadline, "stream did not open in time");
                thread::sleep(hint.retry_after());
            }
        }
    }
}

/// Возможности, как у приложения с программным декодом: FFmpeg + рендер,
/// принимающий программные кадры 4:2:0 8 бит.
fn software_only_capabilities() -> capability_core::SystemCapabilities {
    let mut scanner = capability_core::CapabilityScanner::new();
    scanner.register_provider(Box::new(
        video_ffmpeg::FfmpegSoftwareCapabilityProvider::new(),
    ));
    let mut render = render_core::RenderCapabilities::wgpu_nv12(Some(4096));
    render.supported_frame_contracts =
        vec![video_frame_contract::VideoFrameContract::host_yuv420_planar8()];
    scanner.register_render_capabilities(render);
    scanner.scan()
}

/// Потоков программного декодера: столько же на любой машине. По умолчанию
/// FFmpeg берёт почти все ядра и держит в конвейере столько же кадров — у роликов
/// фикстур (10–20 кадров) первый кадр появлялся бы только в конце потока, и тест
/// проверял бы редкий режим «старт во время доигрывания» вместо обычного.
const DECODER_THREADS: std::num::NonZeroUsize = std::num::NonZeroUsize::new(2).expect("non-zero");

/// Настоящий программный декодер видео (собранный FFmpeg проекта).
fn start_software_video_decoder() -> player_core::StartedVideoBackend {
    let thread_config = video_core::VideoDecoderThreadConfig {
        software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::Fixed(
            DECODER_THREADS,
        ),
        ..video_core::VideoDecoderThreadConfig::default()
    };
    video_ffmpeg::decoder_thread::start_decoder_thread(
        video_ffmpeg::FfmpegDecoderThreadConfig::from_thread_config(thread_config),
    )
    .expect("software FFmpeg decoder")
}

/// Играет поток до конца в безоконной сессии и собирает отчёт.
///
/// `between_ticks` вызывается после каждого шага: так тест подкладывает байты
/// посреди воспроизведения, как это делает страница.
pub fn play(
    demuxer: impl media_core::Demuxer + 'static,
    mut between_ticks: impl FnMut(usize),
) -> PlaybackReport {
    let device = SimulatedAudioDevice::default();
    let mut session = PlayerSession::with_audio_factories(
        Arc::new(audio::ProductionAudioDecoderFactory::default()),
        device.factory(),
    );
    session.set_system_capabilities(software_only_capabilities());
    session.load_prepared_media_with_autoplay(
        PreparedMedia::from_external_label("pushed media test", Box::new(demuxer)),
        true,
    );

    let mut errors = Vec::new();
    // Как приложение: плеер ещё при открытии просит декодер под дорожку видео, и
    // декодер встаёт до первых пакетов. Поздняя замена декодера перематывает на
    // текущую позицию, а перемотки у потока пока нет (03b).
    for event in session.take_events() {
        if let PlayerEvent::VideoBackendSelectionRequested(_) = event {
            session.set_video_backend(start_software_video_decoder());
        }
    }
    let mut previous_tick = Instant::now();
    let mut frames: Vec<PresentedFrame> = Vec::new();
    let mut presented_count = 0;
    let mut final_state = PlaybackState::Idle;
    let mut buffered_after_start = false;
    for tick in 0..MAX_TICKS {
        let now = Instant::now();
        device.advance(now.duration_since(previous_tick));
        previous_tick = now;
        let result = session.tick(PlayerTickContext::new(now));
        presented_count += result.video_frames_presented;
        let snapshot = session.compose_snapshot();
        if let Some(frame) = &snapshot.current_video_frame
            && frames.last().is_none_or(|last| last.pts != frame.pts)
        {
            frames.push(PresentedFrame {
                pts: frame.pts,
                width: frame.width,
                height: frame.height,
                position: snapshot.current_position,
            });
        }
        for event in session.take_events() {
            match event {
                PlayerEvent::VideoBackendSelectionRequested(request)
                    if !request.decodable_by_active_backend =>
                {
                    errors.push(format!("the software decoder cannot decode {request:?}"));
                }
                PlayerEvent::RecoverableError(error) | PlayerEvent::FatalError(error) => {
                    errors.push(format!("{error:?}"));
                }
                _ => {}
            }
        }
        final_state = snapshot.playback_state;
        buffered_after_start |= !frames.is_empty() && final_state == PlaybackState::Buffering;
        if matches!(final_state, PlaybackState::Ended | PlaybackState::Failed) {
            break;
        }
        between_ticks(tick);
        thread::sleep(TICK);
    }
    PlaybackReport {
        frames,
        presented_count,
        audio_samples: device.written_samples(),
        audio_format: device.format(),
        final_state,
        buffered_after_start,
        errors,
    }
}
