//! Чистое планирование растеризации: на какое время рисовать следующим.
//!
//! Вход — «часы показа» (какой кадр на экране, как идёт время, режим) и сроки годности уже
//! готовых картинок. Выход — [`ScheduleDecision`]: одно lookup-время для растеризации либо
//! «нечего делать». Потоков, растеризатора и аллокаций здесь нет: поток-рисовальщик зовёт
//! [`plan_next_render`] в цикле, пока не получит `Idle`.
//!
//! # Правила
//! - Времена рисования лежат на сетке кадров: `опорное_время + k * шаг`.
//! - `Playing`: покрываем окно `[текущее, текущее + шаг * горизонт]`; рисуем первое время сетки,
//!   которое не покрыто ни одним готовым интервалом. Статичная сцена (один интервал на всё окно)
//!   даёт `Idle`; граница реплики внутри горизонта даёт рисование на первом кадре за границей.
//! - `Paused`: только текущий кадр.
//! - `Seeking`/`Scrubbing`: сначала цель, потом горизонт после неё. Устаревшие цели
//!   выбрасываются самим вызывающим (побеждают последние часы), планировщик видит только
//!   актуальную.
//! - Шаг приходит готовым: это наблюдаемая разница pts соседних РАЗНЫХ кадров в времени
//!   субтитров, поэтому на 2× и при пропуске кадров он уже вырос. Неизвестный или
//!   неположительный шаг — рисуем только опорный кадр.

use subtitle_core::SubtitleTime;

use super::validity::ValidityInterval;

/// Верхняя граница горизонта: защита от абсурдного значения в конфиге (цикл планирования
/// проходит по одному времени на кадр горизонта).
const MAX_PRERENDER_FRAMES: u32 = 64;

/// На сколько кадров вперёд рисовать заранее (конфиг `prerender_frames`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct PrerenderFrames(u32);

impl PrerenderFrames {
    /// Горизонт в кадрах; значение сверх разумного предела обрезается.
    pub(in crate::state::subtitles) fn new(frames: u32) -> Self {
        Self(frames.min(MAX_PRERENDER_FRAMES))
    }

    /// Число кадров горизонта.
    pub(in crate::state::subtitles) fn get(self) -> u32 {
        self.0
    }
}

/// Положительный шаг между соседними кадрами во времени субтитров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct FrameStep {
    micros: i64,
}

impl FrameStep {
    /// Шаг в микросекундах; ноль и отрицательное значения не шаг (`None`).
    pub(in crate::state::subtitles) fn from_micros(micros: i64) -> Option<Self> {
        (micros > 0).then_some(Self { micros })
    }

    /// Шаг в микросекундах (всегда больше нуля).
    pub(in crate::state::subtitles) fn micros(self) -> i64 {
        self.micros
    }
}

/// Что известно о шаге между кадрами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum FrameCadence {
    /// Шаг наблюдён (или оценён по частоте видео).
    Known(FrameStep),
    /// Шага нет (первый кадр, нулевая оценка): горизонт вперёд не строим.
    Unknown,
}

/// Скорость воспроизведения как множитель в тысячных долях (1000 = 1×).
///
/// На шаг скорость НЕ влияет — он уже наблюдён в pts. Планировщику скорость нужна только для
/// направления: при нуле и обратном ходе «вперёд» горизонта нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct PlaybackRate {
    thousandths: i32,
}

impl PlaybackRate {
    /// Нормальная скорость 1×.
    pub(in crate::state::subtitles) const NORMAL: Self = Self { thousandths: 1_000 };

    /// Скорость из тысячных долей (2000 = 2×).
    pub(in crate::state::subtitles) const fn from_thousandths(thousandths: i32) -> Self {
        Self { thousandths }
    }

    /// Идёт ли время вперёд.
    fn moves_forward(self) -> bool {
        self.thousandths > 0
    }
}

/// Режим показа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum PresentationMode {
    /// Обычное воспроизведение.
    Playing,
    /// Пауза: меняется только текущий кадр.
    Paused,
    /// Seek: до цели на экране старый кадр; цель — lookup-время.
    Seeking { target: SubtitleTime },
    /// Перетаскивание по таймлайну: цель меняется непрерывно, важна только последняя.
    Scrubbing { target: SubtitleTime },
}

/// «Часы показа»: снимок того, что UI показывает прямо сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct PresentationClock {
    /// Lookup-время текущего кадра (pts + сдвиг синхронизации).
    pub(in crate::state::subtitles) current: SubtitleTime,
    /// Шаг между кадрами во времени субтитров.
    pub(in crate::state::subtitles) cadence: FrameCadence,
    /// Скорость воспроизведения.
    pub(in crate::state::subtitles) rate: PlaybackRate,
    /// Режим показа.
    pub(in crate::state::subtitles) mode: PresentationMode,
}

/// Решение планировщика.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum ScheduleDecision {
    /// Растеризовать на это lookup-время.
    Render { lookup_time: SubtitleTime },
    /// Всё нужное уже готово: спать до новых часов или новых входов.
    Idle,
}

/// Следующее время для растеризации или `Idle`.
///
/// `prepared` — сроки годности готовых картинок С ТОЙ ЖЕ идентичностью входов (источник, холст,
/// ревизия): отбор по идентичности делает вызывающий, планировщик знает только про время.
pub(in crate::state::subtitles) fn plan_next_render(
    clock: &PresentationClock,
    horizon: PrerenderFrames,
    prepared: &[ValidityInterval],
) -> ScheduleDecision {
    let grid = clock.planning_grid(horizon);
    first_uncovered_time(
        grid.anchor,
        grid.step_micros,
        grid.frames_after_anchor,
        prepared,
    )
}

/// Сетка времён, которые планировщик хочет иметь готовыми.
#[derive(Debug, Clone, Copy)]
struct PlanningGrid {
    anchor: SubtitleTime,
    step_micros: i64,
    frames_after_anchor: u32,
}

impl PresentationClock {
    /// Опорное время планирования: цель в seek/scrub, иначе время показанного кадра.
    ///
    /// ЕДИНСТВЕННОЕ понятие «от какого времени считать»: и планировщик, и память потока о
    /// выданном, и очередь UI выбрасывают «ушедшее время» относительно него. Если бы они
    /// считали от показанного (старого) кадра, то при перемотке назад свежая картинка цели
    /// сразу считалась бы устаревшей и рисовалась бы заново бесконечно.
    pub(in crate::state::subtitles) fn planning_anchor(&self) -> SubtitleTime {
        match self.mode {
            PresentationMode::Seeking { target } | PresentationMode::Scrubbing { target } => target,
            PresentationMode::Playing | PresentationMode::Paused => self.current,
        }
    }

    /// Окно времени `[начало, конец]`, которое планировщик хочет покрыть. Картинки, чей срок
    /// не пересекает окно, планировщику не нужны.
    pub(in crate::state::subtitles) fn planning_window(
        &self,
        horizon: PrerenderFrames,
    ) -> (SubtitleTime, SubtitleTime) {
        let grid = self.planning_grid(horizon);
        let span = grid
            .step_micros
            .saturating_mul(i64::from(grid.frames_after_anchor));
        (
            grid.anchor,
            SubtitleTime::from_micros(grid.anchor.as_micros().saturating_add(span)),
        )
    }

    fn planning_grid(&self, horizon: PrerenderFrames) -> PlanningGrid {
        let anchor = self.planning_anchor();
        let policy = match self.mode {
            PresentationMode::Paused => HorizonPolicy::AnchorOnly,
            PresentationMode::Playing => horizon_policy_for(self.rate),
            PresentationMode::Seeking { .. } | PresentationMode::Scrubbing { .. } => {
                HorizonPolicy::AfterAnchor
            }
        };
        // Без известного шага сетки кадров нет: остаётся один опорный кадр.
        let (step_micros, frames_after_anchor) = match (policy, self.cadence) {
            (HorizonPolicy::AfterAnchor, FrameCadence::Known(step)) => {
                (step.micros(), horizon.get())
            }
            (HorizonPolicy::AnchorOnly, _)
            | (HorizonPolicy::AfterAnchor, FrameCadence::Unknown) => (0, 0),
        };
        PlanningGrid {
            anchor,
            step_micros,
            frames_after_anchor,
        }
    }
}

/// Нужен ли горизонт после опорного времени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HorizonPolicy {
    /// Только опорный кадр.
    AnchorOnly,
    /// Опорный кадр и `prerender_frames` кадров после него.
    AfterAnchor,
}

/// Горизонт воспроизведения: вперёд только при ходе времени вперёд.
fn horizon_policy_for(rate: PlaybackRate) -> HorizonPolicy {
    if rate.moves_forward() {
        HorizonPolicy::AfterAnchor
    } else {
        HorizonPolicy::AnchorOnly
    }
}

/// Первое непокрытое время сетки `anchor + k * step`, `k = 0..=frames_after_anchor`.
fn first_uncovered_time(
    anchor: SubtitleTime,
    step_micros: i64,
    frames_after_anchor: u32,
    prepared: &[ValidityInterval],
) -> ScheduleDecision {
    for frame_index in 0..=frames_after_anchor {
        let offset = step_micros.saturating_mul(i64::from(frame_index));
        let lookup_time = SubtitleTime::from_micros(anchor.as_micros().saturating_add(offset));
        if !prepared
            .iter()
            .any(|interval| interval.contains(lookup_time))
        {
            return ScheduleDecision::Render { lookup_time };
        }
    }
    ScheduleDecision::Idle
}

#[cfg(test)]
mod tests;
