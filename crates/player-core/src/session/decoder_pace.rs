//! Нейтральный детектор «видеодекодер не успевает за воспроизведением».
//!
//! Модуль ничего не знает про VA-API/FFmpeg: он смотрит только на наблюдаемые
//! последствия отставания — кадры, опоздавшие к показу (late drops), и «голодание»
//! очереди показа, когда у декодера есть работа, а кадров всё равно нет.
//! Политика (что делать с отставанием) живёт в shell-слое и получает только событие
//! [`PlayerEvent::VideoDecoderFallingBehind`](crate::PlayerEvent).
//!
//! Владелец состояния — [`DecoderPaceMonitor`]: чистая логика на инжектированном
//! времени (`Instant` берётся из tick-а, часы внутри не читаются), поэтому всё
//! проверяется детерминированными unit-тестами.
//!
//! # Инварианты
//! * Считаем только в «зачётное» время: состояние `Playing`, нет seek-транзакции,
//!   прошла пауза тишины после последнего сброса.
//! * Любая смена `media_instance_id`, `render_generation`, скорости или выход из
//!   зачётного состояния сбрасывает накопление и перезапускает окно тишины.
//! * Голодание по вине сети/демукса (у декодера нет входной работы) и голодание из-за
//!   выходного backpressure (потребитель не освобождает surfaces/слоты) не считаются.
//! * Остановка аудио-часов (звук стоит, медиа-время не идёт) — не зачётный tick: ожидаемых
//!   кадров нет, а пауза планировщика с причиной «голодание» вызвана устройством, не декодером.
//! * Tick, в котором разрыв между наблюдениями обрезан [`MAX_OBSERVATION_GAP`] (подвисание
//!   UI, suspend, скачок часов), не засчитывает late-drop-ы: пачка устаревших кадров после
//!   такого провала не свидетельствует об устойчивом темпе декодера.
//! * Голодание шага засчитывается, только если входная работа у декодера была и в начале шага
//!   (прошлое наблюдение), и в конце (текущее): иначе ожидание сети приписалось бы декодеру.
//! * После срабатывания накопление начинается заново: повтор возможен не раньше
//!   следующего полного окна.

use std::time::{Duration, Instant};

use crate::{MediaInstanceId, PlaybackRate, PlaybackState};

/// Верхняя граница одного шага накопления.
///
/// Защита от редких длинных пауз между tick-ами (подвисание UI, спящий процесс):
/// такой разрыв не должен целиком попадать в «ожидаемые кадры» и размывать/раздувать оценку.
const MAX_OBSERVATION_GAP: Duration = Duration::from_millis(250);

/// Пороги детектора отставания.
///
/// Значения по умолчанию — стартовые: окончательные цифры калибруются измерениями на
/// реальном «слабом» железе (T410) по debug-логам закрытия окон.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecoderPaceThresholds {
    /// Тишина после старта/seek/паузы/смены скорости/смены декодера до начала накопления.
    ///
    /// Умолчание 2 с: за это время декодер успевает пройти холодный старт (открытие
    /// контекста, заполнение DPB, первые I-кадры), который не говорит об устойчивой скорости.
    pub quiet_after_reset: Duration,

    /// Длина окна оценки в «зачётном» wall-времени.
    ///
    /// Умолчание 4 с: достаточно длинное, чтобы короткий всплеск (тяжёлая сцена, GC
    /// системы) не срабатывал, и достаточно короткое, чтобы переключиться до заметной деградации.
    pub evaluation_window: Duration,

    /// Доля пропущенных кадров от ожидаемых за окно, начиная с которой считаем отставание.
    ///
    /// Умолчание 0.10: одиночные потери (1–2 кадра на 100) нормальны, 10% уже видимые рывки.
    pub missed_frame_ratio: f64,
}

impl Default for DecoderPaceThresholds {
    fn default() -> Self {
        Self {
            quiet_after_reset: Duration::from_secs(2),
            evaluation_window: Duration::from_secs(4),
            missed_frame_ratio: 0.10,
        }
    }
}

/// Нейтральное доказательство отставания декодера за один закрытый окном интервал.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecoderPaceEvidence {
    /// Длина окна в зачётном wall-времени.
    pub evaluation_window: Duration,

    /// Сколько кадров нужно было показать за окно: Σ(wall_dt × скорость) / кадровый интервал.
    pub expected_frames: f64,

    /// Точное число кадров, отброшенных как опоздавшие.
    pub late_frames: u64,

    /// Голодание по вине декодера, переведённое в кадры.
    pub starved_frames: f64,

    /// Итоговая доля `(late + starved) / expected`.
    pub missed_frame_ratio: f64,

    /// Скорость воспроизведения, при которой окно накопилось.
    pub playback_rate: PlaybackRate,

    /// Media instance, к которому относится доказательство.
    pub media_instance_id: Option<MediaInstanceId>,

    /// Поколение render resources (меняется при seek/scrub/смене декодера).
    pub render_generation: u64,
}

/// Есть ли у декодера входная работа (пакеты в очередях или в обработке).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoderInputWork {
    /// Хотя бы один пакет ждёт декодера или уже обрабатывается им.
    Pending,

    /// Входной работы нет: отсутствие кадров — вина сети/демукса, не декодера.
    Absent,
}

/// Стоит ли декодер на выходном backpressure (держит потребитель, а не декодер).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputPressure {
    /// Нет свободных surfaces/слотов/места в очереди показа.
    Blocked,

    /// Выход свободен.
    Clear,
}

/// Была ли в этом tick-е пустая очередь показа при воспроизведении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentQueueStarvation {
    /// Очередь показа оказалась пустой.
    Starved,

    /// Кадры были.
    Fed,
}

/// Идут ли аудио-часы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioClockActivity {
    /// Часы идут (или звука ещё нет): медиа-время течёт нормально.
    Running,

    /// Часы стоят: медиа-время не идёт, tick не является свидетельством о декодере.
    Stalled,
}

/// Идёт ли активная seek-транзакция.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekActivity {
    /// Seek-транзакция не закрыта.
    Active,

    /// Seek-транзакции нет.
    Idle,
}

/// Одно наблюдение tick-а: единственный вход монитора.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DecoderPaceObservation {
    /// Время tick-а (инжектируется снаружи).
    pub(crate) now: Instant,
    /// Состояние воспроизведения.
    pub(crate) playback_state: PlaybackState,
    /// Идёт ли seek-транзакция.
    pub(crate) seek_activity: SeekActivity,
    /// Текущий media instance.
    pub(crate) media_instance_id: Option<MediaInstanceId>,
    /// Поколение render resources.
    pub(crate) render_generation: u64,
    /// Скорость воспроизведения.
    pub(crate) playback_rate: PlaybackRate,
    /// Оценка кадрового интервала видео (media-время).
    pub(crate) frame_interval: Duration,
    /// Сколько кадров отброшено как опоздавшие именно в этом tick-е.
    pub(crate) late_drops_this_tick: u64,
    /// Пустая ли очередь показа в этом tick-е.
    pub(crate) starvation: PresentQueueStarvation,
    /// Есть ли у декодера входная работа.
    pub(crate) decoder_input: DecoderInputWork,
    /// Стоит ли декодер на выходном backpressure.
    pub(crate) output_pressure: OutputPressure,
    /// Идут ли аудио-часы.
    pub(crate) audio_clock: AudioClockActivity,
}

/// Идентичность «одного прогона»: любое изменение сбрасывает накопление.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PaceRunIdentity {
    media_instance_id: Option<MediaInstanceId>,
    render_generation: u64,
    playback_rate: PlaybackRate,
}

/// Накопители текущего окна оценки.
#[derive(Debug, Clone, Copy, Default)]
struct PaceWindow {
    /// Накопленное зачётное wall-время.
    eligible_wall: Duration,
    /// Ожидаемые кадры.
    expected_frames: f64,
    /// Опоздавшие кадры (точный счётчик).
    late_frames: u64,
    /// Кадры, потерянные голоданием по вине декодера.
    starved_frames: f64,
}

/// Один шаг между двумя соседними наблюдениями.
#[derive(Debug, Clone, Copy)]
struct ObservationStep {
    /// Зачётная длительность шага (уже обрезана [`MAX_OBSERVATION_GAP`]).
    wall_delta: Duration,
    /// Был ли разрыв обрезан капом (тогда late-drop-ы шага не засчитываются).
    gap_was_clamped: bool,
    /// Входная работа декодера в начале шага (предыдущее наблюдение).
    decoder_input_at_step_start: DecoderInputWork,
}

/// Владелец состояния детектора отставания декодера.
#[derive(Debug)]
pub(crate) struct DecoderPaceMonitor {
    /// Идентичность последнего наблюдения; `None` до первого.
    run_identity: Option<PaceRunIdentity>,
    /// Время предыдущего наблюдения для расчёта wall_dt.
    last_observed_at: Option<Instant>,
    /// До какого момента действует окно тишины.
    quiet_until: Option<Instant>,
    /// Входная работа декодера в предыдущем наблюдении (начало текущего шага).
    previous_decoder_input: DecoderInputWork,
    /// Накопления текущего окна.
    window: PaceWindow,
    /// Прогон (медиа, поколение, скорость), о котором уже сообщено отставание.
    ///
    /// Событие — одно на прогон: потребителю (политике Dynamic) повтор не нужен, а в
    /// режимах без переключения повтор каждые 4 с только засорял бы лог. Новый прогон
    /// (seek, смена декодера, скорости или файла) снова взводит детектор.
    reported_run: Option<PaceRunIdentity>,
}

impl Default for DecoderPaceMonitor {
    fn default() -> Self {
        Self {
            run_identity: None,
            last_observed_at: None,
            quiet_until: None,
            previous_decoder_input: DecoderInputWork::Absent,
            window: PaceWindow::default(),
            reported_run: None,
        }
    }
}

impl DecoderPaceMonitor {
    /// Принимает наблюдение tick-а; возвращает доказательство, если окно закрылось отставанием.
    pub(crate) fn observe(
        &mut self,
        observation: DecoderPaceObservation,
        thresholds: &DecoderPaceThresholds,
    ) -> Option<DecoderPaceEvidence> {
        let identity = PaceRunIdentity {
            media_instance_id: observation.media_instance_id,
            render_generation: observation.render_generation,
            playback_rate: observation.playback_rate,
        };
        let identity_changed = self.run_identity != Some(identity);
        let eligible_state = observation.playback_state == PlaybackState::Playing
            && observation.seek_activity == SeekActivity::Idle
            && observation.audio_clock == AudioClockActivity::Running;

        if identity_changed || !eligible_state {
            self.reset_with_quiet_period(
                observation.now,
                identity,
                observation.decoder_input,
                thresholds,
            );
            return None;
        }

        let previous_observed_at = self.last_observed_at.replace(observation.now);
        let previous_decoder_input =
            std::mem::replace(&mut self.previous_decoder_input, observation.decoder_input);
        let quiet_until = self.quiet_until.unwrap_or(observation.now);
        if observation.now < quiet_until {
            return None;
        }

        // Время тишины в зачёт не идёт: отсчитываем от конца тишины.
        let accumulation_start = previous_observed_at
            .unwrap_or(observation.now)
            .max(quiet_until);
        let raw_wall_delta = observation
            .now
            .saturating_duration_since(accumulation_start);
        let gap_was_clamped = raw_wall_delta > MAX_OBSERVATION_GAP;
        let step = ObservationStep {
            wall_delta: raw_wall_delta.min(MAX_OBSERVATION_GAP),
            gap_was_clamped,
            decoder_input_at_step_start: previous_decoder_input,
        };
        self.accumulate(&observation, step);

        if self.window.eligible_wall < thresholds.evaluation_window {
            return None;
        }
        self.close_window(&observation, thresholds)
    }

    /// Сбрасывает накопление и стартует окно тишины от `now`.
    fn reset_with_quiet_period(
        &mut self,
        now: Instant,
        identity: PaceRunIdentity,
        decoder_input: DecoderInputWork,
        thresholds: &DecoderPaceThresholds,
    ) {
        self.run_identity = Some(identity);
        self.last_observed_at = Some(now);
        self.quiet_until = Some(now + thresholds.quiet_after_reset);
        self.previous_decoder_input = decoder_input;
        self.window = PaceWindow::default();
    }

    /// Добавляет в окно один шаг наблюдения.
    fn accumulate(&mut self, observation: &DecoderPaceObservation, step: ObservationStep) {
        let wall_delta = step.wall_delta;
        let frame_interval_secs = observation.frame_interval.as_secs_f64();
        if frame_interval_secs <= 0.0 {
            // Без валидного кадрового интервала кадры не переводятся; шаг пропускаем целиком.
            return;
        }
        let media_delta_secs =
            wall_delta.as_secs_f64() * f64::from(observation.playback_rate.as_f32());
        let frames_in_step = media_delta_secs / frame_interval_secs;

        self.window.eligible_wall += wall_delta;
        self.window.expected_frames += frames_in_step;
        // Late-drop-ы tick-а с обрезанным разрывом не считаем: знаменатель обрезан, а
        // числитель (пачка устаревших кадров после провала) — нет, это дало бы ложный сигнал.
        if !step.gap_was_clamped {
            self.window.late_frames = self
                .window
                .late_frames
                .saturating_add(observation.late_drops_this_tick);
        }

        // Голодание — вина декодера только если у него была работа и в начале шага, и в конце
        // (иначе шаг — ожидание сети, прервавшееся приходом пакета) и выход не заблокирован.
        let starved_by_decoder = observation.starvation == PresentQueueStarvation::Starved
            && observation.decoder_input == DecoderInputWork::Pending
            && step.decoder_input_at_step_start == DecoderInputWork::Pending
            && observation.output_pressure == OutputPressure::Clear;
        if starved_by_decoder {
            self.window.starved_frames += frames_in_step;
        }
    }

    /// Закрывает окно: логирует для калибровки и решает, сработал ли детектор.
    fn close_window(
        &mut self,
        observation: &DecoderPaceObservation,
        thresholds: &DecoderPaceThresholds,
    ) -> Option<DecoderPaceEvidence> {
        let closed_window = std::mem::take(&mut self.window);
        let missed_frames = closed_window.late_frames as f64 + closed_window.starved_frames;
        let missed_frame_ratio = if closed_window.expected_frames > 0.0 {
            missed_frames / closed_window.expected_frames
        } else {
            0.0
        };
        let fell_behind = missed_frame_ratio >= thresholds.missed_frame_ratio;
        let already_reported =
            self.run_identity.is_some() && self.reported_run == self.run_identity;

        if fell_behind && already_reported {
            tracing::debug!(
                expected_frames = closed_window.expected_frames,
                late_frames = closed_window.late_frames,
                starved_frames = closed_window.starved_frames,
                missed_frame_ratio,
                playback_rate = observation.playback_rate.as_f32(),
                "Video decoder is still falling behind playback (already reported for this run)"
            );
            return None;
        }
        if fell_behind {
            self.reported_run = self.run_identity;
            tracing::info!(
                expected_frames = closed_window.expected_frames,
                late_frames = closed_window.late_frames,
                starved_frames = closed_window.starved_frames,
                missed_frame_ratio,
                playback_rate = observation.playback_rate.as_f32(),
                "Video decoder is falling behind playback"
            );
        } else {
            tracing::debug!(
                expected_frames = closed_window.expected_frames,
                late_frames = closed_window.late_frames,
                starved_frames = closed_window.starved_frames,
                missed_frame_ratio,
                playback_rate = observation.playback_rate.as_f32(),
                "Video decoder pace window closed without falling behind"
            );
        }

        fell_behind.then_some(DecoderPaceEvidence {
            evaluation_window: closed_window.eligible_wall,
            expected_frames: closed_window.expected_frames,
            late_frames: closed_window.late_frames,
            starved_frames: closed_window.starved_frames,
            missed_frame_ratio,
            playback_rate: observation.playback_rate,
            media_instance_id: observation.media_instance_id,
            render_generation: observation.render_generation,
        })
    }
}

mod tick_bridge;

#[cfg(test)]
mod tests;
