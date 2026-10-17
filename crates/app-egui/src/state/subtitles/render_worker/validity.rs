//! Срок годности готовой картинки во времени субтитров.

use subtitle_core::SubtitleTime;

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

/// Полуоткрытый интервал `[rendered_at, valid_until)`, на котором готовая картинка верна.
///
/// `rendered_at` — время, для которого картинку рисовали (нижняя граница консервативна: до него
/// набор активных реплик мог быть другим). `valid_until` — ближайшая смена набора реплик после
/// `rendered_at` (`SubtitleTimeline::next_change_after`); `None` — до конца дорожки. Конец
/// интервала НЕ включён: в момент смены набора нужна уже другая картинка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct ValidityInterval {
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
            rendered_at,
            valid_until,
        }
    }

    /// Время, на которое рисовали картинку.
    pub(in crate::state::subtitles) const fn rendered_at(&self) -> SubtitleTime {
        self.rendered_at
    }

    /// Верна ли картинка на `time` (конец интервала не включён).
    pub(in crate::state::subtitles) fn contains(&self, time: SubtitleTime) -> bool {
        self.rendered_at <= time && self.valid_until.is_none_or(|until| time < until)
    }

    /// Пересекает ли срок годности окно `[start, end]` (границы включены). Не пересекающие
    /// окно картинки планировщику не нужны.
    pub(in crate::state::subtitles) fn intersects_window(
        &self,
        start: SubtitleTime,
        end: SubtitleTime,
    ) -> bool {
        self.rendered_at <= end && self.valid_until.is_none_or(|until| start < until)
    }

    /// Задевает ли интервал отрезок времени `span` (например, время только что дописанных
    /// реплик встроенной дорожки): у такой картинки `valid_until` мог стать неверным.
    pub(in crate::state::subtitles) fn overlaps(&self, span: TimeSpan) -> bool {
        self.rendered_at < span.end && self.valid_until.is_none_or(|until| span.start < until)
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

    #[test]
    fn interval_is_over_exactly_at_its_end() {
        let interval = ValidityInterval::new(ms(0), Some(ms(50)));
        assert!(!interval.is_over_at(ms(49)));
        assert!(interval.is_over_at(ms(50)));
        assert!(interval.is_over_at(ms(60)));
    }
}
