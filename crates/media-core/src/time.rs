use std::time::Duration;

/// Временная база для перевода timestamp units в media-время.
///
/// Формула конвертации: `seconds = units * numer / denom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeBase {
    /// Числитель дроби временной базы.
    pub numer: u32,

    /// Знаменатель дроби временной базы.
    pub denom: u32,
}

impl TimeBase {
    /// Создаёт временную базу, если знаменатель не равен нулю.
    #[must_use]
    pub const fn new(numer: u32, denom: u32) -> Option<Self> {
        if denom == 0 {
            None
        } else {
            Some(Self { numer, denom })
        }
    }

    /// Конвертирует timestamp units в [`Duration`].
    #[must_use]
    pub fn timestamp_to_duration(self, units: u64) -> Duration {
        let total_nanoseconds = u128::from(units)
            .saturating_mul(u128::from(self.numer))
            .saturating_mul(1_000_000_000)
            / u128::from(self.denom);
        let clamped_nanoseconds = total_nanoseconds.min(u128::from(u64::MAX));
        Duration::from_nanos(clamped_nanoseconds as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::TimeBase;

    #[test]
    fn rejects_zero_denominator() {
        assert!(TimeBase::new(1, 0).is_none());
    }

    #[test]
    fn converts_timestamp_units_to_duration() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");

        let duration = time_base.timestamp_to_duration(1_500);

        assert_eq!(duration.as_millis(), 1_500);
    }
}
