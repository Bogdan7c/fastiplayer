//! Допуски покрытия Period для static (VOD) SegmentTimeline.
//!
//! ISO/IEC 23009-1 и DASH-IF IOP не требуют, чтобы `SegmentTimeline` совпадал
//! с Period до тика: Period сам «обрезает» медиа. Реальные packager-ы (ffmpeg
//! `-f dash`) пишут audio, стартующее чуть позже видео (в init это пустая
//! правка `elst`, её учитывает isomp4-патч): первый `S@t` лежит чуть правее
//! начала Period, а последний сегмент выступает за его конец, потому что
//! сегмент нельзя разрезать на границе Period.
//!
//! Допуск намеренно ограничен «одним сегментом» с каждой стороны, где сегмент
//! измеряется самым длинным сегментом timeline:
//! - в начале пропуск не больше такого сегмента (иначе это не priming, а
//!   настоящая дыра в покрытии);
//! - в конце выступ за Period не больше такого сегмента (priming сдвигает хвост
//!   audio максимум на кадр-другой, а не на секунды). Хвостовые сегменты,
//!   лежащие целиком за Period (у ffmpeg последний сегмент бывает совсем
//!   коротким), планировщик отбрасывает, а выступающий сегмент обрезается.
//!
//! Всё остальное (начало левее Period, timeline кончился раньше Period, дальний
//! пропуск/выступ) остаётся типизированной ошибкой, а не молчаливой подгонкой.

use super::DashPlanError;

/// Сырые границы timeline в тиках addressing timescale (без PTO-сдвига).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RawTimelineExtent {
    /// Начало первого сегмента.
    pub first_start: u64,
    /// Конец последнего сегмента.
    pub last_end: u64,
    /// Самая длинная duration среди сегментов: верхняя граница leading gap.
    pub longest_segment_duration: u64,
}

/// Проверяет покрытие finite static Period с учётом допуска в один сегмент.
pub(super) fn validate_static_period_coverage(
    extent: RawTimelineExtent,
    period_start_raw: u64,
    period_end_raw: u64,
) -> Result<(), DashPlanError> {
    // Сегмент, начинающийся левее Period, не может быть обрезан в neutral demux.
    if extent.first_start < period_start_raw {
        return Err(DashPlanError::SegmentCrossesPeriodBoundary);
    }
    let latest_allowed_first_start = period_start_raw
        .checked_add(extent.longest_segment_duration)
        .ok_or(DashPlanError::TimelineOverflow)?;
    // Слишком широкий leading gap — это не priming, а дыра в покрытии.
    if extent.first_start > latest_allowed_first_start {
        return Err(DashPlanError::IncompleteStaticPeriod);
    }
    // Timeline закончился раньше Period: хвоста Period нечем играть.
    if extent.last_end < period_end_raw {
        return Err(DashPlanError::IncompleteStaticPeriod);
    }
    // Timeline целиком правее конца Period: играть из него нечего.
    if extent.first_start >= period_end_raw {
        return Err(DashPlanError::IncompleteStaticPeriod);
    }
    let latest_allowed_last_end = period_end_raw
        .checked_add(extent.longest_segment_duration)
        .ok_or(DashPlanError::TimelineOverflow)?;
    // Выступ дальше одного сегмента — это не обрезка Period, а чужой timeline.
    if extent.last_end > latest_allowed_last_end {
        return Err(DashPlanError::SegmentCrossesPeriodBoundary);
    }
    Ok(())
}
