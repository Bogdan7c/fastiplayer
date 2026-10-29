//! Время источника: смещение меток времени полосы и длительность потока.

use std::time::Duration;

use media_core::{TimeBase, TrackId, TrackTimestamp};

/// Смещение меток времени полосы — аналог `SourceBuffer.timestampOffset` в MSE.
///
/// Время кадра для плеера = время из контейнера + смещение. Смещение бывает
/// отрицательным (страница «сдвигает» поток к нулю), поэтому хранится со знаком,
/// в целых микросекундах — как в протоколе моста.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimestampOffset {
    /// Смещение в микросекундах со знаком.
    micros: i64,
}

impl TimestampOffset {
    /// Нулевое смещение: время берётся из контейнера как есть.
    pub const ZERO: Self = Self { micros: 0 };

    /// Смещение из целых микросекунд со знаком.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self { micros }
    }

    /// Смещение в микросекундах.
    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.micros
    }

    /// Применяет смещение к метке времени контейнера.
    ///
    /// `None` — метка ушла в отрицательное время или переполнилась. MSE такие кадры
    /// выбрасывает (окно добавления начинается с нуля), источник делает так же.
    #[must_use]
    pub fn apply(self, container_time: Duration) -> Option<Duration> {
        let magnitude = Duration::from_micros(self.micros.unsigned_abs());
        if self.micros >= 0 {
            container_time.checked_add(magnitude)
        } else {
            container_time.checked_sub(magnitude)
        }
    }
}

/// Микросекунд в секунде — для перевода смещения в единицы дорожки.
const MICROS_PER_SECOND: i128 = 1_000_000;

impl TimestampOffset {
    /// То же смещение в единицах временной базы дорожки (`numer/denom` секунды),
    /// округлённое до ближайшей единицы. `None` — база вырождена или число не помещается.
    #[must_use]
    pub fn in_track_units(self, time_base: TimeBase) -> Option<i64> {
        let numer = i128::from(time_base.numer);
        if numer == 0 {
            return None;
        }
        // units = offset_µs × denom / (numer × 10⁶), с округлением до ближайшего.
        let scaled = i128::from(self.micros) * i128::from(time_base.denom);
        let divisor = numer * MICROS_PER_SECOND;
        let rounded = (scaled + scaled.signum() * divisor / 2) / divisor;
        i64::try_from(rounded).ok()
    }

    /// Применяет смещение к исходной метке дорожки и переписывает её номер на
    /// публичный. `None` — метка ушла в отрицательное время (как у `apply`).
    #[must_use]
    pub fn apply_to_track_timestamp(
        self,
        timestamp: TrackTimestamp,
        public_track: TrackId,
    ) -> Option<TrackTimestamp> {
        let shift = self.in_track_units(timestamp.time_base)?;
        let units = timestamp.units.checked_add(shift)?;
        if units.get() < 0 && timestamp.units.get() >= 0 {
            return None;
        }
        Some(TrackTimestamp::new(
            public_track,
            units.get(),
            timestamp.time_base,
        ))
    }
}

/// Что известно о длительности потока (MSE `duration`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PushedDuration {
    /// Длительность ещё не сообщили.
    #[default]
    Unknown,
    /// Конечная длительность ролика.
    Finite(Duration),
    /// Прямой эфир: конца нет.
    Live,
}

impl PushedDuration {
    /// Длительность в форме `Demuxer::duration()`: только конечная.
    #[must_use]
    pub const fn finite(self) -> Option<Duration> {
        match self {
            Self::Finite(duration) => Some(duration),
            Self::Unknown | Self::Live => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::{TimeBase, TrackId, TrackTimestamp};

    use super::{PushedDuration, TimestampOffset};

    #[test]
    fn track_timestamp_moves_by_offset_in_track_units_and_takes_public_id() {
        // fMP4 H.264 фикстур: база 1/12800; 0,4 с = 5120 единиц.
        let time_base = TimeBase {
            numer: 1,
            denom: 12_800,
        };
        let source = TrackTimestamp::new(TrackId::new(7), 5_120, time_base);
        let shifted = TimestampOffset::from_micros(-400_000)
            .apply_to_track_timestamp(source, TrackId::new(1))
            .expect("non-negative");
        assert_eq!(shifted, TrackTimestamp::new(TrackId::new(1), 0, time_base));
        assert_eq!(
            TimestampOffset::from_micros(-500_000)
                .apply_to_track_timestamp(source, TrackId::new(1)),
            None
        );
        // Округление до ближайшей единицы: 1 мкс при базе 1/48000 — 0 единиц, 11 мкс — 1.
        let audio_base = TimeBase {
            numer: 1,
            denom: 48_000,
        };
        assert_eq!(
            TimestampOffset::from_micros(1).in_track_units(audio_base),
            Some(0)
        );
        assert_eq!(
            TimestampOffset::from_micros(11).in_track_units(audio_base),
            Some(1)
        );
        assert_eq!(
            TimestampOffset::from_micros(-11).in_track_units(audio_base),
            Some(-1)
        );
        assert_eq!(
            TimestampOffset::ZERO.in_track_units(TimeBase { numer: 0, denom: 1 }),
            None
        );
    }

    #[test]
    fn positive_and_negative_offsets_shift_container_time() {
        let container_time = Duration::from_millis(500);
        assert_eq!(
            TimestampOffset::from_micros(1_500_000).apply(container_time),
            Some(Duration::from_millis(2_000))
        );
        assert_eq!(
            TimestampOffset::from_micros(-200_000).apply(container_time),
            Some(Duration::from_millis(300))
        );
        assert_eq!(
            TimestampOffset::ZERO.apply(container_time),
            Some(container_time)
        );
    }

    #[test]
    fn offset_below_zero_drops_the_timestamp() {
        assert_eq!(
            TimestampOffset::from_micros(-600_000).apply(Duration::from_millis(500)),
            None
        );
        assert_eq!(
            TimestampOffset::from_micros(i64::MIN).apply(Duration::ZERO),
            None
        );
    }

    #[test]
    fn only_finite_duration_is_reported_to_the_demuxer() {
        assert_eq!(
            PushedDuration::Finite(Duration::from_secs(3)).finite(),
            Some(Duration::from_secs(3))
        );
        assert_eq!(PushedDuration::Live.finite(), None);
        assert_eq!(PushedDuration::Unknown.finite(), None);
    }
}
