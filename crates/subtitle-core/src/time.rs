//! Время субтитров и сдвиг синхронизации.

/// Момент времени в микросекундах от начала медиа.
///
/// Парсеры выдают только неотрицательные значения, но тип допускает отрицательные:
/// они нужны арифметике сдвига синхронизации. Вся арифметика насыщающая — переполнение
/// i64 никогда не паникует и не заворачивается.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubtitleTime(i64);

impl SubtitleTime {
    /// Начало медиа.
    pub const ZERO: Self = Self(0);

    /// Создаёт время из микросекунд.
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Создаёт время из миллисекунд (насыщающее умножение на 1000).
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis.saturating_mul(1_000))
    }

    /// Возвращает время в микросекундах.
    pub const fn as_micros(self) -> i64 {
        self.0
    }
}

/// Сдвиг АБСОЛЮТНЫХ времён события (не пользовательская синхронизация): применяется один раз
/// при переносе времени дорожки на шкалу плеера (например, HLS-сегмент WebVTT).
///
/// Знак: положительный сдвиг переносит время ПОЗЖЕ (`время + сдвиг`), отрицательный — раньше.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubtitleTimeShift(i64);

impl SubtitleTimeShift {
    /// Отсутствие сдвига.
    pub const ZERO: Self = Self(0);

    /// Сдвиг в микросекундах (знаковый).
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Сдвиг в миллисекундах (насыщающее умножение на 1000).
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis.saturating_mul(1_000))
    }

    /// Сдвиг в микросекундах.
    pub const fn as_micros(self) -> i64 {
        self.0
    }
}

/// Сдвиг времени вышел за пределы i64. В отличие от арифметики синхронизации, здесь
/// насыщение недопустимо: молча склеенные в `i64::MAX` времена дали бы неверную реплику.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("shifting subtitle time {time_micros} us by {shift_micros} us overflows i64")]
pub struct SubtitleTimeShiftOverflow {
    time_micros: i64,
    shift_micros: i64,
}

impl SubtitleTimeShiftOverflow {
    /// Время, которое не удалось сдвинуть.
    pub fn time(self) -> SubtitleTime {
        SubtitleTime(self.time_micros)
    }

    /// Запрошенный сдвиг.
    pub fn shift(self) -> SubtitleTimeShift {
        SubtitleTimeShift(self.shift_micros)
    }
}

impl SubtitleTime {
    /// Время, сдвинутое на `shift`; переполнение i64 — ошибка, а не насыщение.
    pub fn shifted_by(self, shift: SubtitleTimeShift) -> Result<Self, SubtitleTimeShiftOverflow> {
        self.0
            .checked_add(shift.0)
            .map(Self)
            .ok_or(SubtitleTimeShiftOverflow {
                time_micros: self.0,
                shift_micros: shift.0,
            })
    }
}

/// Пользовательский сдвиг синхронизации субтитров.
///
/// Соглашение о знаке: положительный сдвиг означает, что субтитры появляются ПОЗЖЕ.
/// Поэтому для поиска активных событий по времени воспроизведения нужно смотреть
/// в таймлайн на момент `playback_time - offset` (см. [`Self::lookup_time_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubtitleSyncOffset(i64);

impl SubtitleSyncOffset {
    /// Отсутствие сдвига.
    pub const ZERO: Self = Self(0);

    /// Создаёт сдвиг из миллисекунд (насыщающее умножение на 1000).
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis.saturating_mul(1_000))
    }

    /// Возвращает сдвиг в микросекундах.
    pub const fn as_micros(self) -> i64 {
        self.0
    }

    /// Время, на которое нужно обратиться к таймлайну при данном времени воспроизведения.
    pub const fn lookup_time_for(self, playback_time: SubtitleTime) -> SubtitleTime {
        SubtitleTime(playback_time.0.saturating_sub(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_convert_to_micros() {
        assert_eq!(SubtitleTime::from_millis(1_500).as_micros(), 1_500_000);
        assert_eq!(SubtitleSyncOffset::from_millis(-250).as_micros(), -250_000);
    }

    #[test]
    fn millis_conversion_saturates_instead_of_wrapping() {
        assert_eq!(SubtitleTime::from_millis(i64::MAX).as_micros(), i64::MAX);
        assert_eq!(SubtitleTime::from_millis(i64::MIN).as_micros(), i64::MIN);
    }

    #[test]
    fn positive_offset_delays_subtitles_by_looking_earlier() {
        let offset = SubtitleSyncOffset::from_millis(500);
        let lookup = offset.lookup_time_for(SubtitleTime::from_millis(2_000));
        // Субтитр, написанный на 1.5 с, при сдвиге +0.5 с должен показываться на 2.0 с.
        assert_eq!(lookup, SubtitleTime::from_millis(1_500));
    }

    #[test]
    fn negative_offset_makes_subtitles_earlier() {
        let offset = SubtitleSyncOffset::from_millis(-500);
        let lookup = offset.lookup_time_for(SubtitleTime::from_millis(2_000));
        assert_eq!(lookup, SubtitleTime::from_millis(2_500));
    }

    #[test]
    fn zero_offset_is_identity_and_lookup_saturates() {
        let time = SubtitleTime::from_micros(42);
        assert_eq!(SubtitleSyncOffset::ZERO.lookup_time_for(time), time);
        let huge = SubtitleSyncOffset::from_millis(i64::MAX);
        assert_eq!(
            huge.lookup_time_for(SubtitleTime::from_micros(i64::MIN))
                .as_micros(),
            i64::MIN
        );
    }
}
