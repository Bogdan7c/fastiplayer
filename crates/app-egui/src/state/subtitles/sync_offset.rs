//! Шаги сдвига синхронизации субтитров (клавиши Z/X): чистая арифметика с границами.
//!
//! Знак совпадает с `SubtitleSyncOffset`: положительный сдвиг означает «субтитры позже».
//! Сдвиг живёт ровно столько же, сколько media (память «для каждого видео» — сессия 07).

use subtitle_core::SubtitleSyncOffset;

/// Размер одного шага сдвига, миллисекунды (решение владельца: 100 мс).
pub(crate) const SYNC_STEP_MILLIS: i64 = 100;

/// Предел сдвига в обе стороны, миллисекунды (10 минут): защита от бессмысленного ухода
/// и от переполнения при зажатой клавише.
pub(crate) const MAX_SYNC_OFFSET_MILLIS: i64 = 600_000;

/// Направление одного шага сдвига.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncNudge {
    /// Субтитры раньше на шаг (клавиша Z, сдвиг -0,1 с).
    Earlier,
    /// Субтитры позже на шаг (клавиша X, сдвиг +0,1 с).
    Later,
}

/// Сдвиг после одного шага, зажатый в `±MAX_SYNC_OFFSET_MILLIS`.
pub(super) fn nudged_offset(current: SubtitleSyncOffset, nudge: SyncNudge) -> SubtitleSyncOffset {
    let current_millis = current.as_micros() / 1_000;
    let step = match nudge {
        SyncNudge::Earlier => -SYNC_STEP_MILLIS,
        SyncNudge::Later => SYNC_STEP_MILLIS,
    };
    let next_millis = current_millis
        .saturating_add(step)
        .clamp(-MAX_SYNC_OFFSET_MILLIS, MAX_SYNC_OFFSET_MILLIS);
    SubtitleSyncOffset::from_millis(next_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_one_hundred_milliseconds_in_the_named_direction() {
        let later = nudged_offset(SubtitleSyncOffset::ZERO, SyncNudge::Later);
        assert_eq!(later.as_micros(), 100_000);
        let back = nudged_offset(later, SyncNudge::Earlier);
        assert_eq!(back, SubtitleSyncOffset::ZERO);
        let earlier = nudged_offset(SubtitleSyncOffset::ZERO, SyncNudge::Earlier);
        assert_eq!(earlier.as_micros(), -100_000);
    }

    #[test]
    fn offset_is_clamped_to_ten_minutes_in_both_directions() {
        let at_limit = SubtitleSyncOffset::from_millis(MAX_SYNC_OFFSET_MILLIS);
        assert_eq!(nudged_offset(at_limit, SyncNudge::Later), at_limit);
        let at_negative_limit = SubtitleSyncOffset::from_millis(-MAX_SYNC_OFFSET_MILLIS);
        assert_eq!(
            nudged_offset(at_negative_limit, SyncNudge::Earlier),
            at_negative_limit
        );
        // Из предела шаг назад работает как обычно.
        assert_eq!(
            nudged_offset(at_limit, SyncNudge::Earlier).as_micros(),
            (MAX_SYNC_OFFSET_MILLIS - SYNC_STEP_MILLIS) * 1_000
        );
    }
}
