//! Безоконный прогон: поток → открытие → `PlayerSession` с настоящими декодерами.
//!
//! Видео декодирует настоящий программный FFmpeg (`video-ffmpeg`), звук —
//! настоящий декодер (`audio`), выход звука — симулятор. «Кадр дошёл до показа» —
//! это `video_frames_presented` сессии и кадр в `current_video_frame`: ровно его
//! берёт рендер окна.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use player_core::{
    PlaybackState, PlayerCommand, PlayerEvent, PlayerSession, PlayerTickContext, PreparedMedia,
    SeekRequest,
};
use pushed_media_source::{
    OpenProgress, OpenedPushedStream, PushedStreamDemuxer, PushedStreamOpener,
};

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
    /// Шаг, на котором кадр впервые оказался на экране.
    pub tick: usize,
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
    /// Команды перемотки: (шаг, цель).
    pub seeks: Vec<(usize, Duration)>,
    /// Состояние на каждом шаге: (шаг, состояние, идёт ли перемотка).
    pub state_trace: Vec<(usize, PlaybackState, bool)>,
    /// Шагов, на которых плеер ждал (крутилка): перемотка видео идёт состоянием
    /// `Scrubbing`, только звука — `Seeking`, нехватка данных при игре — `Buffering`.
    pub waiting_ticks: usize,
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
    open_stream(opener).demuxer
}

/// Как [`open`], но целиком: демуксер и порт живой шкалы.
pub fn open_stream(opener: &mut PushedStreamOpener) -> OpenedPushedStream {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        match opener.poll_open().expect("stream opens") {
            OpenProgress::Ready(opened) => return opened,
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

/// Что тест видит между шагами плеера.
#[derive(Debug, Clone, Copy)]
pub struct TickObservation {
    /// Номер шага.
    pub tick: usize,
    /// Позиция плеера после шага — её приложение сообщает монитору источника.
    pub position: Duration,
}

/// Играет поток до конца в безоконной сессии и собирает отчёт.
///
/// `between_ticks` вызывается после каждого шага: так тест подкладывает байты
/// посреди воспроизведения, как это делает страница.
pub fn play(
    demuxer: impl media_core::Demuxer + 'static,
    mut between_ticks: impl FnMut(usize),
) -> PlaybackReport {
    play_observed(demuxer, |observation| between_ticks(observation.tick))
}

/// Как [`play`], но между шагами тест видит и позицию плеера (обратное давление:
/// приложение сообщает её монитору источника). На шаге, где плеер закончил,
/// обратный вызов не зовётся.
pub fn play_observed(
    demuxer: impl media_core::Demuxer + 'static,
    mut between_ticks: impl FnMut(TickObservation),
) -> PlaybackReport {
    let prepared = PreparedMedia::from_external_label("pushed media test", Box::new(demuxer));
    play_controlled(prepared, |control| {
        if !control.finished() {
            between_ticks(control.observation());
        }
    })
}

/// Пульт теста между шагами плеера: как кнопки приложения.
pub struct PlayerControl<'session> {
    /// Сессия.
    session: &'session mut PlayerSession,
    /// Номер шага.
    tick: usize,
    /// Позиция после шага.
    position: Duration,
    /// Состояние после шага.
    state: PlaybackState,
    /// Команды перемотки отчёта.
    seeks: &'session mut Vec<(usize, Duration)>,
}

impl PlayerControl<'_> {
    /// Номер шага и позиция.
    pub fn observation(&self) -> TickObservation {
        TickObservation {
            tick: self.tick,
            position: self.position,
        }
    }

    /// Состояние плеера после шага.
    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// Плеер закончил (конец или отказ).
    pub fn finished(&self) -> bool {
        matches!(self.state, PlaybackState::Ended | PlaybackState::Failed)
    }

    /// Команда «перемотать на `target`» (как ползунок приложения).
    pub fn seek(&mut self, target: Duration) {
        self.session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                media_core::MediaTime::from_duration(target),
            )))
            .expect("seek command accepted");
        self.seeks.push((self.tick, target));
        self.state = self.session.playback_state();
    }

    /// Команда «играть».
    pub fn play(&mut self) {
        self.session
            .dispatch_command(PlayerCommand::Play)
            .expect("play command accepted");
        self.state = self.session.playback_state();
    }
}

/// Играет подготовленное медиа в безоконной сессии; между шагами тест жмёт
/// кнопки через [`PlayerControl`]. Прогон кончается, когда плеер закончил и
/// тест на этом шаге ничего не скомандовал (или по пределу шагов).
pub fn play_controlled(
    prepared: PreparedMedia,
    mut between_ticks: impl FnMut(&mut PlayerControl<'_>),
) -> PlaybackReport {
    let device = SimulatedAudioDevice::default();
    let mut session = PlayerSession::with_audio_factories(
        Arc::new(audio::ProductionAudioDecoderFactory::default()),
        device.factory(),
    );
    session.set_system_capabilities(software_only_capabilities());
    session.load_prepared_media_with_autoplay(prepared, true);

    let mut errors = Vec::new();
    // Как приложение: плеер ещё при открытии просит декодер под дорожку видео, и
    // декодер встаёт до первых пакетов (поздняя замена декодера перематывает на
    // текущую позицию).
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
    let mut seeks = Vec::new();
    let mut waiting_ticks = 0;
    let mut state_trace = Vec::new();
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
                tick,
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
        state_trace.push((tick, final_state, snapshot.timeline.seeking));
        if snapshot.timeline.seeking
            || matches!(
                final_state,
                PlaybackState::Buffering | PlaybackState::Seeking | PlaybackState::Scrubbing
            )
        {
            waiting_ticks += 1;
        }
        let mut control = PlayerControl {
            session: &mut session,
            tick,
            position: snapshot.current_position,
            state: final_state,
            seeks: &mut seeks,
        };
        between_ticks(&mut control);
        if control.finished() {
            break;
        }
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
        seeks,
        state_trace,
        waiting_ticks,
    }
}
