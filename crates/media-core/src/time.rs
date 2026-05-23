use std::cmp::Ordering;
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

    /// Конвертирует старый unsigned timestamp contract в [`Duration`].
    ///
    /// Метод оставлен для существующих backend-адаптеров: новые signed
    /// timestamps должны идти через [`Self::track_units_to_duration`].
    #[must_use]
    pub fn timestamp_to_duration(self, units: u64) -> Duration {
        self.positive_units_to_duration(u128::from(units))
    }

    /// Конвертирует unsigned duration units track-а в [`Duration`].
    #[must_use]
    pub fn duration_units_to_duration(self, units: TrackDurationUnits) -> Duration {
        self.positive_units_to_duration(u128::from(units.get()))
    }

    /// Конвертирует signed track units в неотрицательную [`Duration`].
    ///
    /// Отрицательные container timestamps не выпускаются наружу на media
    /// timeline и насыщаются до нуля. Raw signed value при этом остаётся в
    /// [`TrackTimestamp`] для diagnostics и seek-result accounting.
    #[must_use]
    pub fn track_units_to_duration(self, units: TrackTimestampUnits) -> Duration {
        let signed_units = units.get();
        if signed_units <= 0 {
            return Duration::ZERO;
        }

        self.positive_units_to_duration(signed_units as u128)
    }

    /// Переводит уже проверенные неотрицательные units в [`Duration`].
    fn positive_units_to_duration(self, units: u128) -> Duration {
        let total_nanoseconds = units
            .saturating_mul(u128::from(self.numer))
            .saturating_mul(1_000_000_000)
            / u128::from(self.denom);
        let clamped_nanoseconds = total_nanoseconds.min(u128::from(u64::MAX));
        Duration::from_nanos(clamped_nanoseconds as u64)
    }

    /// Конвертирует старый unsigned timestamp contract в нормализованную [`MediaTime`].
    #[must_use]
    pub fn timestamp_to_media_time(self, units: u64) -> MediaTime {
        MediaTime::from_duration(self.timestamp_to_duration(units))
    }

    /// Конвертирует signed track units в нормализованную [`MediaTime`].
    #[must_use]
    pub fn track_units_to_media_time(self, units: TrackTimestampUnits) -> MediaTime {
        MediaTime::from_duration(self.track_units_to_duration(units))
    }

    /// Конвертирует normalized duration обратно в signed track units.
    ///
    /// `None` означает, что значение нельзя честно представить без
    /// переполнения или что временная база не задаёт ненулевой шаг времени.
    #[must_use]
    pub fn duration_to_track_units_checked(
        self,
        duration: MediaDuration,
    ) -> Option<TrackTimestampUnits> {
        let duration = duration.as_duration();
        if self.numer == 0 {
            return duration.is_zero().then_some(TrackTimestampUnits::ZERO);
        }

        let duration_nanoseconds = u128::from(duration.as_secs())
            .checked_mul(1_000_000_000)?
            .checked_add(u128::from(duration.subsec_nanos()))?;
        let units_numerator = duration_nanoseconds.checked_mul(u128::from(self.denom))?;
        let units_denominator = u128::from(self.numer).checked_mul(1_000_000_000)?;
        let unsigned_units = units_numerator / units_denominator;

        if unsigned_units > i64::MAX as u128 {
            None
        } else {
            Some(TrackTimestampUnits::new(unsigned_units as i64))
        }
    }

    /// Конвертирует normalized duration обратно в signed track units с насыщением.
    #[must_use]
    pub fn duration_to_track_units_saturating(
        self,
        duration: MediaDuration,
    ) -> TrackTimestampUnits {
        self.duration_to_track_units_checked(duration)
            .unwrap_or(TrackTimestampUnits::MAX)
    }

    /// Конвертирует normalized media time обратно в signed track units с насыщением.
    #[must_use]
    pub fn media_time_to_track_units_saturating(self, position: MediaTime) -> TrackTimestampUnits {
        self.duration_to_track_units_saturating(MediaDuration::from_duration(
            position.as_duration(),
        ))
    }
}

/// Сырые timestamp units одного track-а в container/backend временной базе.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackTimestampUnits(i64);

impl TrackTimestampUnits {
    /// Нулевой timestamp в track time base.
    pub const ZERO: Self = Self(0);

    /// Минимальный signed timestamp, который может прийти из container-а.
    pub const MIN: Self = Self(i64::MIN);

    /// Максимальный signed timestamp, который можно хранить без расширения типа.
    pub const MAX: Self = Self(i64::MAX);

    /// Создаёт raw signed timestamp units без нормализации.
    #[must_use]
    pub const fn new(units: i64) -> Self {
        Self(units)
    }

    /// Пробует создать raw signed units из старого unsigned backend contract-а.
    #[must_use]
    pub const fn from_unsigned_checked(units: u64) -> Option<Self> {
        if units > i64::MAX as u64 {
            None
        } else {
            Some(Self(units as i64))
        }
    }

    /// Создаёт raw signed units из старого unsigned backend contract-а с насыщением.
    #[must_use]
    pub const fn from_unsigned_saturating(units: u64) -> Self {
        if units > i64::MAX as u64 {
            Self::MAX
        } else {
            Self(units as i64)
        }
    }

    /// Возвращает raw signed units без изменения знака или нормализации.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Проверяет, что container timestamp находится раньше нулевой media timeline.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Складывает timestamp units с проверкой переполнения.
    #[must_use]
    pub fn checked_add(self, rhs: i64) -> Option<Self> {
        self.0.checked_add(rhs).map(Self)
    }

    /// Складывает timestamp units с насыщением вместо переполнения.
    #[must_use]
    pub fn saturating_add(self, rhs: i64) -> Self {
        Self(self.0.saturating_add(rhs))
    }

    /// Вычитает timestamp units с проверкой переполнения.
    #[must_use]
    pub fn checked_sub(self, rhs: i64) -> Option<Self> {
        self.0.checked_sub(rhs).map(Self)
    }

    /// Вычитает timestamp units с насыщением вместо переполнения.
    #[must_use]
    pub fn saturating_sub(self, rhs: i64) -> Self {
        Self(self.0.saturating_sub(rhs))
    }
}

impl From<i64> for TrackTimestampUnits {
    /// Создаёт raw signed timestamp units из container timestamp-а.
    fn from(units: i64) -> Self {
        Self::new(units)
    }
}

impl From<TrackTimestampUnits> for i64 {
    /// Возвращает raw signed units для backend adapters и diagnostics.
    fn from(units: TrackTimestampUnits) -> Self {
        units.get()
    }
}

/// Timestamp одного track-а в исходной container/backend временной базе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackTimestamp {
    /// Track, к которому относится timestamp.
    pub track_id: TrackId,

    /// Сырые signed timestamp units в `time_base`.
    pub units: TrackTimestampUnits,

    /// Временная база исходного track-а.
    pub time_base: TimeBase,
}

impl TrackTimestamp {
    /// Создаёт typed signed timestamp для конкретного track-а.
    #[must_use]
    pub const fn new(track_id: TrackId, units: i64, time_base: TimeBase) -> Self {
        Self {
            track_id,
            units: TrackTimestampUnits::new(units),
            time_base,
        }
    }

    /// Создаёт тот же timestamp для другого внешнего track id после remap-а demuxer-а.
    #[must_use]
    pub const fn with_track_id(self, track_id: TrackId) -> Self {
        Self { track_id, ..self }
    }

    /// Создаёт typed timestamp из старого unsigned backend contract-а.
    #[must_use]
    pub const fn from_unsigned_units(track_id: TrackId, units: u64, time_base: TimeBase) -> Self {
        Self {
            track_id,
            units: TrackTimestampUnits::from_unsigned_saturating(units),
            time_base,
        }
    }

    /// Переводит track timestamp в нормализованную media-позицию.
    #[must_use]
    pub fn to_media_time(self) -> MediaTime {
        self.time_base.track_units_to_media_time(self.units)
    }

    /// Сравнивает signed timestamp-ы на общей timeline без насыщения отрицательных units в ноль.
    #[must_use]
    pub fn cmp_timeline_position(self, other: Self) -> Ordering {
        let left_position = i128::from(self.units.get())
            * i128::from(self.time_base.numer)
            * i128::from(other.time_base.denom);
        let right_position = i128::from(other.units.get())
            * i128::from(other.time_base.numer)
            * i128::from(self.time_base.denom);

        left_position.cmp(&right_position)
    }
}

/// Сырые duration units одного track-а в container/backend временной базе.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackDurationUnits(u64);

impl TrackDurationUnits {
    /// Нулевая длительность в track time base.
    pub const ZERO: Self = Self(0);

    /// Максимальная длительность, которую можно хранить без расширения типа.
    pub const MAX: Self = Self(u64::MAX);

    /// Создаёт raw unsigned duration units без нормализации.
    #[must_use]
    pub const fn new(units: u64) -> Self {
        Self(units)
    }

    /// Возвращает raw unsigned units без изменения масштаба.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for TrackDurationUnits {
    /// Создаёт raw unsigned duration units из container duration-а.
    fn from(units: u64) -> Self {
        Self::new(units)
    }
}

impl From<TrackDurationUnits> for u64 {
    /// Возвращает raw unsigned duration units для backend adapters и diagnostics.
    fn from(units: TrackDurationUnits) -> Self {
        units.get()
    }
}

/// Duration одного track-а в исходной container/backend временной базе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackDuration {
    /// Track, к которому относится duration.
    pub track_id: TrackId,

    /// Сырые unsigned duration units в `time_base`.
    pub units: TrackDurationUnits,

    /// Временная база исходного track-а.
    pub time_base: TimeBase,
}

impl TrackDuration {
    /// Создаёт typed unsigned duration для конкретного track-а.
    #[must_use]
    pub const fn new(track_id: TrackId, units: u64, time_base: TimeBase) -> Self {
        Self {
            track_id,
            units: TrackDurationUnits::new(units),
            time_base,
        }
    }

    /// Создаёт ту же duration для другого внешнего track id после remap-а demuxer-а.
    #[must_use]
    pub const fn with_track_id(self, track_id: TrackId) -> Self {
        Self { track_id, ..self }
    }

    /// Переводит track duration в нормализованную media-длительность.
    #[must_use]
    pub fn to_media_duration(self) -> MediaDuration {
        MediaDuration::from_duration(self.time_base.duration_units_to_duration(self.units))
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

/// Compatibility-состояние исторического preview-контракта внутри interactive scrub.
///
/// Текущий player-core публикует только `Inactive`; остальные варианты оставлены для
/// внешних интеграций и будущей переписи preview-пайплайна.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimelinePreviewState {
    /// Preview-пайплайн сейчас не активен и не ожидает результата.
    Inactive,

    /// Для текущей scrub-цели ещё нет показанного preview frame-а.
    Pending,

    /// Показан frame текущего preview, но целевой frame ещё не подтверждён.
    Visible,

    /// Целевой preview frame был показан и может считаться готовым.
    Ready,

    /// Preview-пайплайн не успел дойти до целевого frame-а за отведённый бюджет.
    Expired,

    /// Preview-пайплайн был прерван ошибкой до готового preview frame-а.
    Failed,
}

impl Default for TimelinePreviewState {
    /// По умолчанию compatibility preview-состояние отсутствует.
    fn default() -> Self {
        Self::Inactive
    }
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

    /// Compatibility-статус preview, независимый от визуального stale-флага.
    pub preview_state: TimelinePreviewState,
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
            preview_state: TimelinePreviewState::Inactive,
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
            preview_state: TimelinePreviewState::Inactive,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::TrackId;

    use super::{
        MediaDuration, MediaTime, TimeBase, TimelineRange, TrackDuration, TrackDurationUnits,
        TrackTimestamp, TrackTimestampUnits,
    };

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
    fn legacy_unsigned_timestamp_keeps_large_u64_saturation() {
        let time_base = TimeBase::new(1, 1_000_000_000).expect("valid nanosecond time base");

        let duration = time_base.timestamp_to_duration(u64::MAX);

        assert_eq!(duration, Duration::from_nanos(u64::MAX));
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
        assert_eq!(timestamp.units.get(), 96_000);
        assert_eq!(media_time.as_duration(), Duration::from_secs(2));
    }

    #[test]
    fn track_duration_converts_through_timebase() {
        let time_base = TimeBase::new(1, 48_000).expect("valid audio time base");
        let duration = TrackDuration::new(TrackId::new(3), 960, time_base);

        let media_duration = duration.to_media_duration();

        assert_eq!(duration.track_id, TrackId::new(3));
        assert_eq!(duration.units.get(), 960);
        assert_eq!(media_duration.as_duration(), Duration::from_millis(20));
    }

    #[test]
    fn track_timestamp_remap_keeps_units_and_timebase() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let timestamp = TrackTimestamp::new(TrackId::new(1), -125, time_base);

        let remapped_timestamp = timestamp.with_track_id(TrackId::new(2));

        assert_eq!(remapped_timestamp.track_id, TrackId::new(2));
        assert_eq!(remapped_timestamp.units, timestamp.units);
        assert_eq!(remapped_timestamp.time_base, timestamp.time_base);
    }

    #[test]
    fn track_timestamp_timeline_cmp_keeps_negative_order() {
        let millisecond_time_base = TimeBase::new(1, 1_000).expect("valid ms time base");
        let sample_time_base = TimeBase::new(1, 48_000).expect("valid sample time base");
        let earlier_timestamp = TrackTimestamp::new(TrackId::new(1), -24_000, sample_time_base);
        let later_timestamp = TrackTimestamp::new(TrackId::new(2), -250, millisecond_time_base);

        assert_eq!(
            earlier_timestamp.cmp_timeline_position(later_timestamp),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn negative_track_timestamp_clamps_to_zero_but_keeps_raw_units() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let timestamp = TrackTimestamp::new(TrackId::new(5), -250, time_base);

        let media_time = timestamp.to_media_time();

        assert_eq!(timestamp.units.get(), -250);
        assert!(timestamp.units.is_negative());
        assert_eq!(media_time, MediaTime::ZERO);
    }

    #[test]
    fn large_signed_timestamp_saturates_without_panic() {
        let time_base = TimeBase::new(u32::MAX, 1).expect("valid time base");
        let timestamp = TrackTimestamp::new(TrackId::new(9), i64::MAX, time_base);

        let media_time = timestamp.to_media_time();

        assert_eq!(media_time.as_duration(), Duration::from_nanos(u64::MAX));
    }

    #[test]
    fn duration_converts_back_to_track_units() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");

        let units = time_base.media_time_to_track_units_saturating(MediaTime::from_millis(1_500));

        assert_eq!(units, TrackTimestampUnits::new(1_500));
    }

    #[test]
    fn timestamp_units_arithmetic_is_checked_and_saturating() {
        let units = TrackTimestampUnits::new(i64::MAX);

        assert_eq!(units.checked_add(1), None);
        assert_eq!(units.saturating_add(1), TrackTimestampUnits::MAX);
        assert_eq!(
            TrackTimestampUnits::from_unsigned_checked(i64::MAX as u64),
            Some(TrackTimestampUnits::MAX)
        );
        assert_eq!(TrackTimestampUnits::from_unsigned_checked(u64::MAX), None);
        assert_eq!(
            TrackTimestampUnits::new(i64::MIN).saturating_sub(1),
            TrackTimestampUnits::MIN
        );
    }

    #[test]
    fn duration_units_keep_unsigned_container_value() {
        let units = TrackDurationUnits::new(u64::MAX);

        assert_eq!(units.get(), u64::MAX);
        assert_eq!(TrackDurationUnits::from(u64::MAX), TrackDurationUnits::MAX);
    }
}
