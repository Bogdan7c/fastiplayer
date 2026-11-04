//! Точки произвольного доступа сегмента: где внутри него можно начать декодирование.
//!
//! MSE стирает кадры не строго по диапазону `remove(start, end)`: конец диапазона
//! сдвигается вперёд до ближайшей точки произвольного доступа (кадра, с которого можно
//! начать декодирование) — кадры после стёртого до неё зависят от стёртого и уходят
//! вместе с ним (W3C Media Source Extensions, «Coded Frame Removal Algorithm»). Страница
//! об этом не сообщает: её `remove(0,07; 0,56)` у группы кадров на 2 с стирает всю группу,
//! и следующий `remove` начинается уже с 2,05 с. Чтобы полка помнила ровно то, что
//! помнит страница (решение владельца 124), ей нужно знать, есть ли в сегменте такие
//! точки и где последняя из них ([`SegmentRandomAccess`]).
//!
//! Здесь — чистые данные и разбор флагов кадра fMP4; правило стирания — у полки
//! (`shelf::removal`).

use super::{MediaTime, SegmentTimingError, TrackMedia};
use crate::time::TimestampOffset;

/// `sample_flags` fMP4: кадр не является точкой синхронизации.
const SAMPLE_IS_NON_SYNC: u32 = 0x0001_0000;
/// `sample_flags` fMP4: маска поля `sample_depends_on`.
const SAMPLE_DEPENDS_ON_MASK: u32 = 0x0300_0000;
/// `sample_flags` fMP4: значение `sample_depends_on` «зависит от других кадров».
const SAMPLE_DEPENDS_ON_OTHERS: u32 = 0x0100_0000;

/// Что известно о точках произвольного доступа сегмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentRandomAccess {
    /// Контейнер об этом не говорит (WebM, сегмент без кадров): полка не знает, что
    /// стёрла страница за концом диапазона, и держит сегмент (держать больше безопасно).
    Unrecorded,
    /// Кадры есть, но все зависят от кадров раньше сегмента: без них не декодируются
    /// (хвост группы кадров при однокадровых фрагментах).
    NoPoints,
    /// Самая поздняя по времени показа точка в сегменте (после `timestampOffset`).
    LatestPoint(MediaTime),
}

impl SegmentRandomAccess {
    /// То же после `timestampOffset` полосы. `None` — переполнение i64.
    #[must_use]
    pub(crate) fn shifted_by(self, offset: TimestampOffset) -> Option<Self> {
        match self {
            Self::LatestPoint(point) => point
                .as_micros()
                .checked_add(offset.as_micros())
                .map(|micros| Self::LatestPoint(MediaTime::from_micros(micros))),
            unchanged @ (Self::Unrecorded | Self::NoPoints) => Some(unchanged),
        }
    }

    /// Сводка двух дорожек одного сегмента (fMP4 «видео + звук»): кадр дорожки без
    /// сведений делает неизвестным весь сегмент, иначе важна самая поздняя точка.
    #[must_use]
    pub(super) fn merged_with(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unrecorded, _) | (_, Self::Unrecorded) => Self::Unrecorded,
            (Self::NoPoints, known) | (known, Self::NoPoints) => known,
            (Self::LatestPoint(first), Self::LatestPoint(second)) => {
                Self::LatestPoint(first.max(second))
            }
        }
    }
}

/// Флаги кадра fMP4 (`trun`, `tfhd` или `trex`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SampleFlags(u32);

impl SampleFlags {
    /// Флаги из поля контейнера.
    pub(super) const fn new(bits: u32) -> Self {
        Self(bits)
    }

    /// Можно ли начать декодирование с этого кадра. То же правило, что у демуксера
    /// (FFmpeg `mov`): кадр помечен синхронным и не объявлен зависимым.
    const fn is_random_access(self) -> bool {
        self.0 & SAMPLE_IS_NON_SYNC == 0
            && self.0 & SAMPLE_DEPENDS_ON_MASK != SAMPLE_DEPENDS_ON_OTHERS
    }
}

/// Откуда берётся ответ «это точка доступа» для кадров дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrackAccess {
    /// Каждый кадр — точка доступа: дорожка не видео (MSE считает так же; флаги кадров
    /// звука записаны как попало).
    EverySample,
    /// По флагам кадра; `default` — значение по умолчанию из `tfhd` или `trex`.
    ByFlags {
        /// Флаги кадра, у которого `trun` своих не пишет.
        default: Option<SampleFlags>,
    },
}

impl TrackAccess {
    /// Правило дорожки типа `media`.
    pub(super) const fn of_track(media: TrackMedia, default: Option<SampleFlags>) -> Self {
        match media {
            TrackMedia::NotVideo => Self::EverySample,
            TrackMedia::Video | TrackMedia::Unknown => Self::ByFlags { default },
        }
    }

    /// Точка ли кадр с флагами `flags` (`None` — своих нет, берутся по умолчанию).
    /// Внутри `None` — сведений нет вовсе.
    const fn sample_is_point(self, flags: Option<SampleFlags>) -> Option<bool> {
        match (self, flags) {
            (Self::EverySample, _) => Some(true),
            (Self::ByFlags { .. }, Some(own)) => Some(own.is_random_access()),
            (
                Self::ByFlags {
                    default: Some(fallback),
                },
                None,
            ) => Some(fallback.is_random_access()),
            (Self::ByFlags { default: None }, None) => None,
        }
    }
}

/// Накопитель точек доступа кадров одного `traf`, в тиках дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RandomAccessScan {
    /// Правило дорожки.
    access: TrackAccess,
    /// Самое позднее начало показа кадра-точки.
    latest_point: Option<i128>,
    /// Встретился кадр, про который ничего не известно.
    unrecorded: bool,
}

impl RandomAccessScan {
    /// Пустой накопитель.
    pub(super) const fn new(access: TrackAccess) -> Self {
        Self {
            access,
            latest_point: None,
            unrecorded: false,
        }
    }

    /// Учитывает один кадр: начало показа и его собственные флаги (`None` — нет).
    pub(super) fn include_sample(&mut self, start: i128, flags: Option<SampleFlags>) {
        match self.access.sample_is_point(flags) {
            Some(true) => {
                self.latest_point = Some(self.latest_point.map_or(start, |known| known.max(start)));
            }
            Some(false) => {}
            None => self.unrecorded = true,
        }
    }

    /// Учитывает `count` кадров подряд с шагом `step` без записей в `trun` (только первый
    /// может иметь свои флаги): одной формулой, как и их время (`count` выбирает страница,
    /// перебор поштучно был бы уязвимостью).
    pub(super) fn include_uniform_samples(
        &mut self,
        start: i128,
        step: i128,
        count: u32,
        first_flags: Option<SampleFlags>,
    ) -> Result<(), SegmentTimingError> {
        self.include_sample(start, first_flags);
        // Кадры после первого все одинаковы: достаточно посмотреть на последний.
        if let Some(after_first) = count.checked_sub(1).filter(|rest| *rest > 0) {
            let last_start = i128::from(after_first)
                .checked_mul(step)
                .and_then(|distance| start.checked_add(distance))
                .ok_or(SegmentTimingError::TimeOutOfRange)?;
            self.include_sample(last_start, None);
        }
        Ok(())
    }

    /// Итог `traf` на шкале `to_time` (перевод тиков дорожки во время).
    pub(super) fn finish(
        self,
        to_time: impl FnOnce(i128) -> Result<MediaTime, SegmentTimingError>,
    ) -> Result<SegmentRandomAccess, SegmentTimingError> {
        if self.unrecorded {
            return Ok(SegmentRandomAccess::Unrecorded);
        }
        self.latest_point
            .map_or(Ok(SegmentRandomAccess::NoPoints), |ticks| {
                to_time(ticks).map(SegmentRandomAccess::LatestPoint)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Флаги Twitch: `trex` ключевого кадра и `first_sample_flags` зависимого.
    const SYNC: SampleFlags = SampleFlags::new(0x0240_0000);
    const DEPENDENT: SampleFlags = SampleFlags::new(0x0141_0000);

    fn video_scan(default: Option<SampleFlags>) -> RandomAccessScan {
        RandomAccessScan::new(TrackAccess::of_track(TrackMedia::Video, default))
    }

    fn finished(scan: RandomAccessScan) -> SegmentRandomAccess {
        scan.finish(|ticks| {
            i64::try_from(ticks)
                .map(MediaTime::from_micros)
                .map_err(|_| SegmentTimingError::TimeOutOfRange)
        })
        .expect("in range")
    }

    #[test]
    fn dependent_samples_are_no_points_and_a_sync_sample_is_the_latest_point() {
        let mut scan = video_scan(Some(SYNC));
        scan.include_sample(100, Some(DEPENDENT));
        assert_eq!(finished(scan), SegmentRandomAccess::NoPoints);
        // Свой флаг главнее значения по умолчанию, а поздняя точка главнее ранней.
        scan.include_sample(300, None);
        scan.include_sample(200, Some(SYNC));
        assert_eq!(
            finished(scan),
            SegmentRandomAccess::LatestPoint(MediaTime::from_micros(300))
        );
    }

    #[test]
    fn a_sample_without_any_flags_makes_the_segment_unrecorded() {
        let mut scan = video_scan(None);
        scan.include_sample(100, Some(SYNC));
        scan.include_sample(200, None);
        assert_eq!(finished(scan), SegmentRandomAccess::Unrecorded);
    }

    #[test]
    fn every_audio_sample_is_a_point_whatever_its_flags_say() {
        let mut scan = RandomAccessScan::new(TrackAccess::of_track(TrackMedia::NotVideo, None));
        scan.include_sample(100, None);
        scan.include_sample(123, Some(DEPENDENT));
        assert_eq!(
            finished(scan),
            SegmentRandomAccess::LatestPoint(MediaTime::from_micros(123))
        );
    }

    #[test]
    fn uniform_samples_are_counted_by_formula_with_their_own_first_flags() {
        // Первый — ключевой, остальные зависимые: точка только первая.
        let mut key_then_dependent = video_scan(Some(DEPENDENT));
        key_then_dependent
            .include_uniform_samples(1_000, 40, u32::MAX, Some(SYNC))
            .expect("in range");
        assert_eq!(
            finished(key_then_dependent),
            SegmentRandomAccess::LatestPoint(MediaTime::from_micros(1_000))
        );
        // Все синхронные: точка — начало последнего кадра.
        let mut all_sync = video_scan(Some(SYNC));
        all_sync
            .include_uniform_samples(1_000, 40, 3, None)
            .expect("in range");
        assert_eq!(
            finished(all_sync),
            SegmentRandomAccess::LatestPoint(MediaTime::from_micros(1_080))
        );
    }

    #[test]
    fn segments_merge_by_the_latest_point_and_unknown_tracks_win() {
        let at = |micros| SegmentRandomAccess::LatestPoint(MediaTime::from_micros(micros));
        assert_eq!(at(5).merged_with(at(9)), at(9));
        assert_eq!(SegmentRandomAccess::NoPoints.merged_with(at(9)), at(9));
        assert_eq!(
            at(9).merged_with(SegmentRandomAccess::Unrecorded),
            SegmentRandomAccess::Unrecorded
        );
    }

    #[test]
    fn the_timestamp_offset_moves_the_latest_point_only() {
        let offset = TimestampOffset::from_micros(1_000);
        assert_eq!(
            SegmentRandomAccess::LatestPoint(MediaTime::from_micros(5)).shifted_by(offset),
            Some(SegmentRandomAccess::LatestPoint(MediaTime::from_micros(
                1_005
            )))
        );
        assert_eq!(
            SegmentRandomAccess::NoPoints.shifted_by(offset),
            Some(SegmentRandomAccess::NoPoints)
        );
    }
}
