use std::time::Duration;

use crate::TrackId;

/// Нормализованная позиция на media timeline относительно начала текущего media.
///
/// Тип намеренно хранит `Duration`, а не container timestamp: container-specific
/// единицы переводятся в этот тип через [`TrackTimestamp`] и [`TimeBase`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaTime(Duration);

impl MediaTime {
    /// Нулевая позиция timeline.
    pub const ZERO: Self = Self(Duration::ZERO);

    /// Максимальная позиция, которую можно безопасно представить через `Duration`.
    pub const MAX: Self = Self(Duration::MAX);

    /// Создаёт media-позицию из уже нормализованной длительности от начала media.
    #[must_use]
    pub const fn from_duration(position: Duration) -> Self {
        Self(position)
    }

    /// Создаёт media-позицию из миллисекунд.
    #[must_use]
    pub const fn from_millis(milliseconds: u64) -> Self {
        Self(Duration::from_millis(milliseconds))
    }

    /// Создаёт media-позицию из секунд.
    #[must_use]
    pub const fn from_secs(seconds: u64) -> Self {
        Self(Duration::from_secs(seconds))
    }

    /// Создаёт media-позицию из наносекунд.
    #[must_use]
    pub const fn from_nanos(nanoseconds: u64) -> Self {
        Self(Duration::from_nanos(nanoseconds))
    }

    /// Возвращает внутреннее представление как standard library `Duration`.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// Возвращает позицию в секундах для UI/diagnostics formatting.
    #[must_use]
    pub fn as_secs_f64(self) -> f64 {
        self.0.as_secs_f64()
    }

    /// Добавляет длительность с насыщением вместо переполнения.
    #[must_use]
    pub fn saturating_add(self, duration: MediaDuration) -> Self {
        Self(
            self.0
                .checked_add(duration.as_duration())
                .unwrap_or(Duration::MAX),
        )
    }

    /// Вычитает длительность с насыщением в ноль.
    #[must_use]
    pub fn saturating_sub(self, duration: MediaDuration) -> Self {
        Self(self.0.saturating_sub(duration.as_duration()))
    }

    /// Ограничивает позицию заданным seekable-диапазоном.
    #[must_use]
    pub fn clamp_to(self, range: TimelineRange) -> Self {
        range.clamp(self)
    }
}

impl From<Duration> for MediaTime {
    /// Делает миграцию старых `Duration` контрактов явной в местах вызова.
    fn from(position: Duration) -> Self {
        Self::from_duration(position)
    }
}

impl From<MediaTime> for Duration {
    /// Возвращает compatibility-представление для старого UI/runtime кода.
    fn from(position: MediaTime) -> Self {
        position.as_duration()
    }
}

/// Нормализованная длительность media или timeline-окна.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaDuration(Duration);

impl MediaDuration {
    /// Нулевая длительность.
    pub const ZERO: Self = Self(Duration::ZERO);

    /// Максимальная длительность, которую можно безопасно представить через `Duration`.
    pub const MAX: Self = Self(Duration::MAX);

    /// Создаёт media-длительность из standard library `Duration`.
    #[must_use]
    pub const fn from_duration(duration: Duration) -> Self {
        Self(duration)
    }

    /// Создаёт media-длительность из миллисекунд.
    #[must_use]
    pub const fn from_millis(milliseconds: u64) -> Self {
        Self(Duration::from_millis(milliseconds))
    }

    /// Создаёт media-длительность из секунд.
    #[must_use]
    pub const fn from_secs(seconds: u64) -> Self {
        Self(Duration::from_secs(seconds))
    }

    /// Создаёт media-длительность из наносекунд.
    #[must_use]
    pub const fn from_nanos(nanoseconds: u64) -> Self {
        Self(Duration::from_nanos(nanoseconds))
    }

    /// Возвращает внутреннее представление как standard library `Duration`.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// Возвращает длительность в секундах для UI/diagnostics formatting.
    #[must_use]
    pub fn as_secs_f64(self) -> f64 {
        self.0.as_secs_f64()
    }
}

impl From<Duration> for MediaDuration {
    /// Делает миграцию старых `Duration` контрактов явной в местах вызова.
    fn from(duration: Duration) -> Self {
        Self::from_duration(duration)
    }
}

impl From<MediaDuration> for Duration {
    /// Возвращает compatibility-представление для старого UI/runtime кода.
    fn from(duration: MediaDuration) -> Self {
        duration.as_duration()
    }
}

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

    /// Конвертирует timestamp units в нормализованную [`MediaTime`].
    #[must_use]
    pub fn timestamp_to_media_time(self, units: u64) -> MediaTime {
        MediaTime::from_duration(self.timestamp_to_duration(units))
    }
}

/// Timestamp одного track-а в исходной container/backend временной базе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackTimestamp {
    /// Track, к которому относится timestamp.
    pub track_id: TrackId,

    /// Сырые timestamp units в `time_base`.
    pub units: u64,

    /// Временная база исходного track-а.
    pub time_base: TimeBase,
}

impl TrackTimestamp {
    /// Создаёт typed timestamp для конкретного track-а.
    #[must_use]
    pub const fn new(track_id: TrackId, units: u64, time_base: TimeBase) -> Self {
        Self {
            track_id,
            units,
            time_base,
        }
    }

    /// Переводит track timestamp в нормализованную media-позицию.
    #[must_use]
    pub fn to_media_time(self) -> MediaTime {
        self.time_base.timestamp_to_media_time(self.units)
    }
}

/// Закрытый seekable-диапазон media timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineRange {
    /// Начало seekable-окна.
    pub start: MediaTime,

    /// Конец seekable-окна.
    pub end: MediaTime,
}

impl TimelineRange {
    /// Создаёт диапазон только если конец не раньше начала.
    #[must_use]
    pub fn new(start: MediaTime, end: MediaTime) -> Option<Self> {
        if end < start {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// Создаёт диапазон, насыщая некорректный конец до начала.
    #[must_use]
    pub fn from_bounds_saturating(start: MediaTime, end: MediaTime) -> Self {
        if end < start {
            Self { start, end: start }
        } else {
            Self { start, end }
        }
    }

    /// Возвращает длительность диапазона.
    #[must_use]
    pub fn duration(self) -> MediaDuration {
        MediaDuration::from_duration(
            self.end
                .as_duration()
                .saturating_sub(self.start.as_duration()),
        )
    }

    /// Проверяет, находится ли позиция внутри диапазона.
    #[must_use]
    pub fn contains(self, position: MediaTime) -> bool {
        self.start <= position && position <= self.end
    }

    /// Ограничивает позицию границами диапазона.
    #[must_use]
    pub fn clamp(self, position: MediaTime) -> MediaTime {
        if position < self.start {
            self.start
        } else if position > self.end {
            self.end
        } else {
            position
        }
    }
}

/// Нейтральная причина, почему timeline сейчас нельзя seek-ать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelineNotSeekableReason {
    /// Media ещё не открыт.
    NoMedia,

    /// Источник открыт, но длительность или seekable window ещё неизвестны.
    UnknownTimeline,

    /// Источник явно не поддерживает seek.
    SourceNotSeekable,

    /// Индекс контейнера или network range metadata ещё не готов.
    IndexUnavailable,
}

/// Полный snapshot timeline-состояния без ссылок на player/backend internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineSnapshot {
    /// Текущая позиция playback на нормализованной media timeline.
    pub current_position: MediaTime,

    /// Цель активного seek/scrub, если такая операция сейчас идёт.
    pub target_position: Option<MediaTime>,

    /// Полная длительность media, если она известна.
    pub duration: Option<MediaDuration>,

    /// Seekable-диапазон, если источник уже сообщил его.
    pub seekable_range: Option<TimelineRange>,

    /// Можно ли сейчас отправлять seek/scrub команды.
    pub seekable: bool,

    /// Причина недоступности seek, если `seekable == false`.
    pub not_seekable_reason: Option<TimelineNotSeekableReason>,

    /// Идёт ли commit seek-запроса.
    pub seeking: bool,

    /// Идёт ли interactive scrub.
    pub scrubbing: bool,

    /// Показываемый кадр относится к старой позиции во время seek/scrub.
    pub stale_frame: bool,
}

impl TimelineSnapshot {
    /// Создаёт seekable VOD timeline из известной длительности.
    #[must_use]
    pub fn seekable_vod(duration: MediaDuration) -> Self {
        let end = MediaTime::from_duration(duration.as_duration());
        Self {
            current_position: MediaTime::ZERO,
            target_position: None,
            duration: Some(duration),
            seekable_range: Some(TimelineRange::from_bounds_saturating(MediaTime::ZERO, end)),
            seekable: true,
            not_seekable_reason: None,
            seeking: false,
            scrubbing: false,
            stale_frame: false,
        }
    }
}

impl Default for TimelineSnapshot {
    /// Создаёт timeline для пустой player session.
    fn default() -> Self {
        Self {
            current_position: MediaTime::ZERO,
            target_position: None,
            duration: None,
            seekable_range: None,
            seekable: false,
            not_seekable_reason: Some(TimelineNotSeekableReason::NoMedia),
            seeking: false,
            scrubbing: false,
            stale_frame: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::TrackId;

    use super::{MediaDuration, MediaTime, TimeBase, TimelineRange, TrackTimestamp};

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

    #[test]
    fn media_time_converts_from_and_to_duration() {
        let position = MediaTime::from_duration(Duration::from_millis(2_500));

        assert_eq!(position.as_duration(), Duration::from_millis(2_500));
        assert_eq!(Duration::from(position), Duration::from_millis(2_500));
    }

    #[test]
    fn media_time_ordering_uses_normalized_duration() {
        let earlier_position = MediaTime::from_millis(100);
        let later_position = MediaTime::from_millis(200);

        assert!(earlier_position < later_position);
    }

    #[test]
    fn media_time_saturating_clamp_uses_timeline_range() {
        let range = TimelineRange::from_bounds_saturating(
            MediaTime::from_millis(1_000),
            MediaTime::from_millis(2_000),
        );

        assert_eq!(
            MediaTime::from_millis(500).clamp_to(range),
            MediaTime::from_millis(1_000)
        );
        assert_eq!(
            MediaTime::from_millis(2_500).clamp_to(range),
            MediaTime::from_millis(2_000)
        );
        assert_eq!(
            MediaTime::from_millis(1_500).clamp_to(range),
            MediaTime::from_millis(1_500)
        );
    }

    #[test]
    fn media_time_saturating_add_and_sub_do_not_overflow_or_underflow() {
        let near_max_position = MediaTime::MAX.saturating_add(MediaDuration::from_millis(1));
        let below_zero_position = MediaTime::ZERO.saturating_sub(MediaDuration::from_millis(1));

        assert_eq!(near_max_position, MediaTime::MAX);
        assert_eq!(below_zero_position, MediaTime::ZERO);
    }

    #[test]
    fn track_timestamp_converts_through_timebase() {
        let time_base = TimeBase::new(1, 48_000).expect("valid audio time base");
        let timestamp = TrackTimestamp::new(TrackId::new(3), 96_000, time_base);

        let media_time = timestamp.to_media_time();

        assert_eq!(timestamp.track_id, TrackId::new(3));
        assert_eq!(media_time.as_duration(), Duration::from_secs(2));
    }
}
