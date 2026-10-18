//! Срок годности готовой картинки во времени субтитров.

use subtitle_core::SubtitleTime;
use subtitle_raster::AnimationChange;

use super::schedule::FrameStep;

/// Минимальная длина интервала годности: картинка всегда годна на своё время.
const MINIMAL_INTERVAL_MICROS: i64 = 1;

/// `time` со сдвигом на `micros` микросекунд (с насыщением, без переполнения).
fn shifted(time: SubtitleTime, micros: i64) -> SubtitleTime {
    SubtitleTime::from_micros(time.as_micros().saturating_add(micros))
}

/// Полуоткрытый отрезок времени субтитров `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct TimeSpan {
    start: SubtitleTime,
    end: SubtitleTime,
}

impl TimeSpan {
    /// Отрезок `[start, end)`; перепутанные границы упорядочиваются.
    pub(in crate::state::subtitles) fn new(start: SubtitleTime, end: SubtitleTime) -> Self {
        Self {
            start: start.min(end),
            end: start.max(end),
        }
    }

    /// Всё время: так инвалидируется дорожка, заменённая целиком.
    pub(in crate::state::subtitles) fn everything() -> Self {
        Self {
            start: SubtitleTime::from_micros(i64::MIN),
            end: SubtitleTime::from_micros(i64::MAX),
        }
    }

    /// Минимальный отрезок, покрывающий оба.
    pub(in crate::state::subtitles) fn union(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// Судьба срока годности картинки после укорочения события (см.
/// [`ValidityInterval::after_shortening`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum ShorteningEffect {
    /// Картинки укорочение не касается.
    Unaffected,
    /// Картинка нарисована до нового конца события: до него она верна, срок обрезан.
    Narrowed(ValidityInterval),
    /// Картинка нарисована в отрезке, где события уже нет: содержимое неверно.
    Invalid,
}

/// Полуоткрытый интервал `[valid_from, valid_until)`, на котором готовая картинка верна.
///
/// `rendered_at` — время, для которого картинку рисовали. `valid_until` — ближайшая смена
/// набора реплик после `rendered_at` (`SubtitleTimeline::next_change_after`); `None` — до конца
/// дорожки. Конец интервала НЕ включён: в момент смены набора нужна уже другая картинка.
///
/// `valid_from` обычно равен `rendered_at` (до него набор активных реплик мог быть другим).
/// У анимированной сцены он раньше: картинка `[g - шаг/2, g + шаг/2)` годится кадру, чьё время
/// БЛИЖАЙШЕ к `g` (см. [`Self::narrowed_by_animation`]). Инвариант: `valid_from <= rendered_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct ValidityInterval {
    valid_from: SubtitleTime,
    rendered_at: SubtitleTime,
    valid_until: Option<SubtitleTime>,
}

impl ValidityInterval {
    /// Интервал картинки, нарисованной на `rendered_at`; `valid_until` — следующая смена набора.
    pub(in crate::state::subtitles) const fn new(
        rendered_at: SubtitleTime,
        valid_until: Option<SubtitleTime>,
    ) -> Self {
        Self {
            valid_from: rendered_at,
            rendered_at,
            valid_until,
        }
    }

    /// Сужает срок годности по анимации нарисованной сцены.
    ///
    /// - [`AnimationChange::None`] — без изменений (статичная сцена живёт до смены реплик);
    /// - [`AnimationChange::At`] — картинка верна ровно до этого момента;
    /// - [`AnimationChange::EveryFrame`] — ЦЕНТРИРОВАННЫЙ отрезок вокруг `rendered_at`:
    ///   `[g - шаг/2, g + (шаг - шаг/2))`. Реальные pts не лежат точно на сетке (23.976 к/с с
    ///   pts в миллисекундах), и кадр должен получить картинку ближайшего к нему времени, а не
    ///   предыдущую: иначе `\move`/`\fad` отстают на кадр. Соседние картинки сетки смыкаются
    ///   без щелей. Нижняя граница не уходит раньше `latest_content_change(от, до)` — последней
    ///   смены набора реплик в окне `(g - шаг/2, g]`: до неё реплика ещё не началась или была
    ///   другой, и показывать её картинку нельзя. Верхняя ограничена `valid_until` (смена
    ///   набора после `g`).
    ///
    /// Шаг неизвестен (первый кадр) — интервал в одну микросекунду `[g, g + 1 мкс)`: он покрывает
    /// только `g`, любой другой кадр перерисуется.
    pub(in crate::state::subtitles) fn narrowed_by_animation(
        self,
        change: AnimationChange,
        frame_step: Option<FrameStep>,
        latest_content_change: impl FnOnce(SubtitleTime, SubtitleTime) -> Option<SubtitleTime>,
    ) -> Self {
        let (valid_from, animation_end) = match change {
            AnimationChange::None => return self,
            AnimationChange::At(change_at) => (self.valid_from, change_at),
            AnimationChange::EveryFrame => match frame_step {
                Some(step) => self.centered_around_rendered_time(step, latest_content_change),
                None => (
                    self.valid_from,
                    shifted(self.rendered_at, MINIMAL_INTERVAL_MICROS),
                ),
            },
        };
        // Не короче минимального интервала: иначе картинка не годилась бы даже на своё время.
        let earliest_end = shifted(self.rendered_at, MINIMAL_INTERVAL_MICROS);
        let narrowed_end = animation_end.max(earliest_end);
        let valid_until = match self.valid_until {
            Some(until) => Some(until.min(narrowed_end)),
            None => Some(narrowed_end),
        };
        Self {
            valid_from,
            valid_until,
            ..self
        }
    }

    /// Границы центрированного отрезка `EveryFrame` (см. [`Self::narrowed_by_animation`]).
    fn centered_around_rendered_time(
        self,
        step: FrameStep,
        latest_content_change: impl FnOnce(SubtitleTime, SubtitleTime) -> Option<SubtitleTime>,
    ) -> (SubtitleTime, SubtitleTime) {
        let before = step.micros() / 2;
        let after = step.micros() - before;
        let centered_start = shifted(self.rendered_at, -before);
        // Момент, с которого содержимое сцены стало таким, как в картинке.
        let content_start = latest_content_change(centered_start, self.rendered_at);
        let valid_from = content_start
            .map_or(centered_start, |change| change.max(centered_start))
            .min(self.rendered_at);
        (valid_from, shifted(self.rendered_at, after))
    }

    /// Время, на которое рисовали картинку.
    pub(in crate::state::subtitles) const fn rendered_at(&self) -> SubtitleTime {
        self.rendered_at
    }

    /// Верна ли картинка на `time` (конец интервала не включён).
    pub(in crate::state::subtitles) fn contains(&self, time: SubtitleTime) -> bool {
        self.valid_from <= time && self.valid_until.is_none_or(|until| time < until)
    }

    /// Пересекает ли срок годности окно `[start, end]` (границы включены). Не пересекающие
    /// окно картинки планировщику не нужны.
    pub(in crate::state::subtitles) fn intersects_window(
        &self,
        start: SubtitleTime,
        end: SubtitleTime,
    ) -> bool {
        self.valid_from <= end && self.valid_until.is_none_or(|until| start < until)
    }

    /// Задевает ли интервал отрезок времени `span` (например, время только что дописанных
    /// реплик встроенной дорожки): у такой картинки `valid_until` мог стать неверным.
    pub(in crate::state::subtitles) fn overlaps(&self, span: TimeSpan) -> bool {
        self.valid_from < span.end && self.valid_until.is_none_or(|until| span.start < until)
    }

    /// Что происходит с картинкой, когда событие укорочено и его показ потерял отрезок `lost`
    /// (`[new_end, previous_end)`).
    ///
    /// - не пересекает `lost` — не затронута;
    /// - нарисована ДО `lost` (`rendered_at < lost.start`): набор реплик до нового конца тот же,
    ///   поэтому картинка остаётся верной, но только до `lost.start` — срок обрезается (это и
    ///   даёт показу «без мигания»: ту же картинку не нужно рисовать заново);
    /// - нарисована внутри `lost` — содержит исчезнувшую реплику, выбрасывается.
    pub(in crate::state::subtitles) fn after_shortening(self, lost: TimeSpan) -> ShorteningEffect {
        if !self.overlaps(lost) {
            return ShorteningEffect::Unaffected;
        }
        if self.rendered_at >= lost.start {
            return ShorteningEffect::Invalid;
        }
        let valid_until = self
            .valid_until
            .map_or(lost.start, |until| until.min(lost.start));
        ShorteningEffect::Narrowed(Self {
            valid_until: Some(valid_until),
            ..self
        })
    }

    /// Закончилась ли годность к `time`: картинка не годна ни на `time`, ни позже.
    pub(in crate::state::subtitles) fn is_over_at(&self, time: SubtitleTime) -> bool {
        self.valid_until.is_some_and(|until| until <= time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: i64) -> SubtitleTime {
        SubtitleTime::from_millis(value)
    }

    #[test]
    fn interval_end_is_exclusive_and_start_is_inclusive() {
        let interval = ValidityInterval::new(ms(100), Some(ms(200)));
        assert!(!interval.contains(ms(99)));
        assert!(interval.contains(ms(100)));
        assert!(interval.contains(ms(199)));
        assert!(!interval.contains(ms(200)));
    }

    #[test]
    fn open_ended_interval_never_expires() {
        let interval = ValidityInterval::new(ms(100), None);
        assert!(interval.contains(ms(i64::MAX / 2_000)));
        assert!(!interval.is_over_at(ms(1_000_000)));
    }

    #[test]
    fn shortening_narrows_pictures_drawn_before_the_lost_span_and_invalidates_those_inside() {
        let lost = TimeSpan::new(ms(500), ms(10_000));
        let open = ValidityInterval::new(ms(100), None);
        let narrowed = match open.after_shortening(lost) {
            ShorteningEffect::Narrowed(interval) => interval,
            other => panic!("expected Narrowed, got {other:?}"),
        };
        assert!(narrowed.contains(ms(499)));
        assert!(!narrowed.contains(ms(500)));
        assert_eq!(narrowed.rendered_at(), ms(100));
        // Нарисована в потерянном отрезке (в том числе ровно на его начале) - неверна.
        assert_eq!(
            ValidityInterval::new(ms(500), None).after_shortening(lost),
            ShorteningEffect::Invalid
        );
        assert_eq!(
            ValidityInterval::new(ms(700), Some(ms(900))).after_shortening(lost),
            ShorteningEffect::Invalid
        );
        // Закончилась раньше потерянного отрезка - не затронута; уже короче нового конца - тоже.
        assert_eq!(
            ValidityInterval::new(ms(100), Some(ms(500))).after_shortening(lost),
            ShorteningEffect::Unaffected
        );
        assert_eq!(
            ValidityInterval::new(ms(20_000), None).after_shortening(lost),
            ShorteningEffect::Unaffected
        );
    }

    #[test]
    fn interval_overlaps_only_spans_that_touch_its_validity() {
        let interval = ValidityInterval::new(ms(100), Some(ms(200)));
        assert!(interval.overlaps(TimeSpan::new(ms(150), ms(160))));
        assert!(interval.overlaps(TimeSpan::new(ms(50), ms(101))));
        assert!(!interval.overlaps(TimeSpan::new(ms(200), ms(300))));
        assert!(!interval.overlaps(TimeSpan::new(ms(0), ms(100))));
        let open = ValidityInterval::new(ms(100), None);
        assert!(open.overlaps(TimeSpan::new(ms(5_000), ms(6_000))));
    }

    #[test]
    fn interval_intersects_a_window_with_inclusive_bounds() {
        let interval = ValidityInterval::new(ms(100), Some(ms(200)));
        assert!(interval.intersects_window(ms(150), ms(300)));
        assert!(
            interval.intersects_window(ms(0), ms(100)),
            "window end is inclusive"
        );
        assert!(
            !interval.intersects_window(ms(200), ms(300)),
            "interval end is exclusive"
        );
        assert!(!interval.intersects_window(ms(0), ms(99)));
    }

    fn no_content_change(_: SubtitleTime, _: SubtitleTime) -> Option<SubtitleTime> {
        None
    }

    #[test]
    fn animation_every_frame_is_centered_on_the_rendered_time() {
        let step = FrameStep::from_micros(42_000);
        let interval = ValidityInterval::new(ms(1_000), Some(ms(9_000))).narrowed_by_animation(
            AnimationChange::EveryFrame,
            step,
            no_content_change,
        );
        // [g - 21 мс, g + 21 мс): кадр берёт картинку ближайшего времени.
        assert!(!interval.contains(ms(978)));
        assert!(interval.contains(ms(979)));
        assert!(interval.contains(ms(1_020)));
        assert!(!interval.contains(ms(1_021)));
        assert_eq!(interval.rendered_at(), ms(1_000));
    }

    #[test]
    fn centered_intervals_of_neighbouring_grid_times_tile_without_gaps_for_odd_steps() {
        let step = FrameStep::from_micros(41_667);
        let narrow = |at: SubtitleTime| {
            ValidityInterval::new(at, None).narrowed_by_animation(
                AnimationChange::EveryFrame,
                step,
                no_content_change,
            )
        };
        let first = narrow(SubtitleTime::from_micros(0));
        let second = narrow(SubtitleTime::from_micros(41_667));
        // Соседи покрывают [-20833, 20834) и [20834, 62501): каждое время ровно одним из них.
        for micros in -20_833..62_501 {
            let time = SubtitleTime::from_micros(micros);
            assert!(
                first.contains(time) != second.contains(time),
                "time {micros} must belong to exactly one of the neighbours"
            );
        }
        assert!(!first.contains(SubtitleTime::from_micros(-20_834)));
        assert!(!second.contains(SubtitleTime::from_micros(62_501)));
    }

    #[test]
    fn animation_every_frame_never_starts_before_the_last_content_change() {
        let step = FrameStep::from_micros(40_000);
        // Реплика началась на 10 мс раньше кадра: картинка не годится кадрам до начала.
        let interval = ValidityInterval::new(ms(100), None).narrowed_by_animation(
            AnimationChange::EveryFrame,
            step,
            |from, to| {
                assert_eq!((from, to), (ms(80), ms(100)), "window is (g - step/2, g]");
                Some(ms(90))
            },
        );
        assert!(!interval.contains(ms(89)));
        assert!(interval.contains(ms(90)));
        // Смена раньше окна не ослабляет центрированную границу (её и так не бывает: окно).
        let early = ValidityInterval::new(ms(100), None).narrowed_by_animation(
            AnimationChange::EveryFrame,
            step,
            |_, _| Some(ms(10)),
        );
        assert!(early.contains(ms(80)) && !early.contains(ms(79)));
    }

    #[test]
    fn animation_every_frame_is_limited_by_the_next_set_change() {
        let step = FrameStep::from_micros(40_000);
        let earlier = ValidityInterval::new(ms(100), Some(ms(110))).narrowed_by_animation(
            AnimationChange::EveryFrame,
            step,
            no_content_change,
        );
        assert!(earlier.contains(ms(109)) && !earlier.contains(ms(110)));
    }

    #[test]
    fn animation_every_frame_without_known_step_covers_only_its_own_time() {
        let interval = ValidityInterval::new(ms(100), None).narrowed_by_animation(
            AnimationChange::EveryFrame,
            None,
            no_content_change,
        );
        assert!(interval.contains(ms(100)));
        assert!(!interval.contains(ms(99)));
        assert!(!interval.contains(SubtitleTime::from_micros(100_001)));
    }

    #[test]
    fn animation_change_at_moment_ends_interval_there_and_none_keeps_it() {
        let base = ValidityInterval::new(ms(0), Some(ms(500)));
        let at = base.narrowed_by_animation(AnimationChange::At(ms(200)), None, no_content_change);
        assert!(at.contains(ms(199)) && !at.contains(ms(200)));
        let later =
            base.narrowed_by_animation(AnimationChange::At(ms(900)), None, no_content_change);
        assert_eq!(later, base, "set change comes first");
        assert_eq!(
            base.narrowed_by_animation(AnimationChange::None, None, no_content_change),
            base
        );
        // Момент «в прошлом» не даёт пустого интервала.
        let past = ValidityInterval::new(ms(100), None).narrowed_by_animation(
            AnimationChange::At(ms(50)),
            None,
            no_content_change,
        );
        assert!(past.contains(ms(100)));
    }

    #[test]
    fn interval_is_over_exactly_at_its_end() {
        let interval = ValidityInterval::new(ms(0), Some(ms(50)));
        assert!(!interval.is_over_at(ms(49)));
        assert!(interval.is_over_at(ms(50)));
        assert!(interval.is_over_at(ms(60)));
    }
}
