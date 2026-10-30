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
    PlaybackState, PlayerCommand, PlayerEvent, PlayerSession, PlayerTickContext,
    PlayerVideoDropReason, PreparedMedia, SeekRequest,
};
use pushed_media_source::{
    OpenProgress, OpenedPushedStream, PushedStreamDemuxer, PushedStreamOpener,
};

use super::presentation_log::PresentationLog;
pub use super::presentation_log::PresentedFrame;
use super::simulated_audio::SimulatedAudioDevice;

/// Пауза между шагами плеера (настоящее время).
///
/// Время настоящее, а не виртуальное: часы видео без звука якорятся по
/// `Instant::now()`, и виртуальное время, бегущее быстрее реального, сдвигало бы
/// их относительно кадров. Звуковая карта-симулятор идёт по тем же часам, но не
/// быстрее [`AUDIO_ADVANCE_PER_TICK_CAP`] за шаг.
pub const TICK: Duration = Duration::from_millis(5);
/// Сколько «звуковая карта» проигрывает за один шаг, как бы долго шаг ни шёл.
///
/// Под нагрузкой поток теста может стоять десятки миллисекунд между шагами. Карта
/// по настоящему времени проиграла бы за это время несколько кадров видео сразу,
/// и плеер законно выбросил бы опоздавшие кадры — тест увидел бы «потерю кадров»,
/// которой у продукта нет. Поэтому часы звука идут не быстрее одного шага за шаг:
/// задержка обвязки замедляет часы, а не перескакивает их. Сценарии тестов
/// написаны в шагах («байты на шаге 40» = ~0,2 с звука) — с таким пределом это
/// верно при любой нагрузке. Без нагрузки шаг чуть дольше `TICK` (сама работа
/// шага), и часы звука идут чуть медленнее настоящих — плееру это безразлично:
/// звук и есть его часы. Настоящие задержки шагов видны в
/// [`PlaybackReport::max_tick_gap`].
pub const AUDIO_ADVANCE_PER_TICK_CAP: Duration = TICK;
/// Предел шагов: ~10 с на ролик меньше секунды.
const MAX_TICKS: usize = 2_000;
/// Сколько ждать открытия потока.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Что увидел и услышал «зритель».
#[derive(Debug)]
pub struct PlaybackReport {
    /// Показанные кадры по порядку показа: каждый показ плеера — одна запись,
    /// включая кадр, вытесненный следующим внутри того же шага
    /// ([`super::presentation_log::FrameSighting::SupersededWithinTick`]).
    pub frames: Vec<PresentedFrame>,
    /// Показы, которым обвязка не смогла назвать время (должно быть 0; иначе
    /// `frames` неполон по вине обвязки, а не плеера).
    pub unattributed_presentations: u64,
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
    /// Тишина посреди звука (пауза выхода или пустой буфер до последнего сэмпла).
    pub mid_stream_audio_silence: Duration,
    /// Записанный звук, выброшенный вместе с уничтоженным выходом.
    pub discarded_audio: Duration,
    /// Номер PCM-кадра (frame, не sample) в `audio_samples`, с которого начался
    /// звук после каждой очистки выхода перемоткой: всё записанное дальше звучит
    /// от цели перемотки.
    pub audio_frames_at_seek_clears: Vec<usize>,
    /// Кадры, выброшенные плеером (время, причина, шаг): опоздавшие, разгон
    /// перемотки и т. п. — чтобы отличить законный выброс от потери в обвязке.
    pub dropped_frames: Vec<(Duration, PlayerVideoDropReason, usize)>,
    /// Самая долгая пауза между шагами по настоящему времени: большая — задержка
    /// обвязки (поток теста стоял), а не дефект плеера.
    pub max_tick_gap: Duration,
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
    /// Журнал показов (перемотка забывает прочитанное старой позиции).
    presentations: &'session mut PresentationLog,
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
        self.presentations.forget_pending_for_seek();
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
    let mut max_tick_gap = Duration::ZERO;
    let mut presentations = PresentationLog::default();
    let mut dropped_frames = Vec::new();
    let mut presented_count = 0;
    let mut final_state = PlaybackState::Idle;
    let mut buffered_after_start = false;
    let mut seeks = Vec::new();
    let mut waiting_ticks = 0;
    let mut state_trace = Vec::new();
    for tick in 0..MAX_TICKS {
        let now = Instant::now();
        let tick_gap = now.duration_since(previous_tick);
        max_tick_gap = max_tick_gap.max(tick_gap);
        device.advance(tick_gap.min(AUDIO_ADVANCE_PER_TICK_CAP));
        previous_tick = now;
        let result = session.tick(PlayerTickContext::new(now));
        presented_count += result.video_frames_presented;
        let snapshot = session.compose_snapshot();
        presentations.observe_tick(tick, &result, &snapshot);
        dropped_frames.extend(
            result
                .dropped_video_frames
                .iter()
                .map(|dropped| (dropped.pts, dropped.reason, tick)),
        );
        let started_showing = presentations.has_frames();
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
        buffered_after_start |= started_showing && final_state == PlaybackState::Buffering;
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
            presentations: &mut presentations,
        };
        between_ticks(&mut control);
        if control.finished() {
            break;
        }
        thread::sleep(TICK);
    }
    let (frames, unattributed_presentations) = presentations.finish();
    PlaybackReport {
        frames,
        unattributed_presentations,
        presented_count,
        audio_samples: device.written_samples(),
        audio_format: device.format(),
        final_state,
        buffered_after_start,
        errors,
        seeks,
        state_trace,
        waiting_ticks,
        mid_stream_audio_silence: device.mid_stream_silence(),
        discarded_audio: device.discarded_audio(),
        audio_frames_at_seek_clears: device.written_frames_at_seek_clears(),
        dropped_frames,
        max_tick_gap,
    }
}
