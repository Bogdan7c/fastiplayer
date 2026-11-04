//! Проход по `trun` одного `traf` fMP4: время кадров и их точки произвольного доступа.
//!
//! Время декодирования течёт от `tfdt` через все `trun` дорожки. Кадры перебираются ради
//! длительностей; всё число страницы проверяется до перебора и при сложении (страница —
//! враг, см. `framing::timing`).

use super::SegmentTimingError;
use super::TimingStructure;
use super::byte_cursor::{ByteCursor, FullBoxHeader};
use super::random_access::{RandomAccessScan, SampleFlags};

/// `trun`: есть `data_offset`.
const TRUN_DATA_OFFSET: u32 = 0x00_0001;
/// `trun`: есть `first_sample_flags`.
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x00_0004;
/// `trun`: у каждого кадра своя длительность.
const TRUN_SAMPLE_DURATION: u32 = 0x00_0100;
/// `trun`: у каждого кадра свой размер.
const TRUN_SAMPLE_SIZE: u32 = 0x00_0200;
/// `trun`: у каждого кадра свои флаги.
const TRUN_SAMPLE_FLAGS: u32 = 0x00_0400;
/// `trun`: у каждого кадра своё смещение композиции.
const TRUN_SAMPLE_COMPOSITION_OFFSET: u32 = 0x00_0800;

/// Байтов в одном поле записи кадра `trun` (все поля 32-битные).
const TRUN_FIELD_BYTES: usize = 4;
/// Самое раннее начало и самый поздний конец показа кадров, в тиках дорожки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PresentationExtent {
    /// `(min начала, max конца)`; `None` — кадров ещё не было.
    bounds: Option<(i128, i128)>,
}

impl PresentationExtent {
    /// Учитывает один кадр (или группу кадров подряд) `[start, end)`.
    fn include(&mut self, start: i128, end: i128) {
        self.bounds = Some(match self.bounds {
            Some((earliest, latest_end)) => (earliest.min(start), latest_end.max(end)),
            None => (start, end),
        });
    }
}

/// Проход по `trun` одного `traf`: время декодирования течёт от `tfdt` через все `trun`.
#[derive(Debug)]
pub(super) struct RunWalker {
    /// Время декодирования следующего кадра, тики.
    next_decode_time: i128,
    /// Длительность кадра, когда `trun` её не пишет.
    default_sample_duration: u32,
    /// Накопленный отрезок показа.
    extent: PresentationExtent,
    /// Накопленные точки произвольного доступа.
    random_access: RandomAccessScan,
}

impl RunWalker {
    /// Проход от времени декодирования `tfdt` с длительностью кадра по умолчанию.
    pub(super) fn new(
        next_decode_time: i128,
        default_sample_duration: u32,
        random_access: RandomAccessScan,
    ) -> Self {
        Self {
            next_decode_time,
            default_sample_duration,
            extent: PresentationExtent::default(),
            random_access,
        }
    }

    /// `(min начала, max конца)` показа кадров, тики; `None` — кадров не было.
    pub(super) const fn presentation_bounds(&self) -> Option<(i128, i128)> {
        self.extent.bounds
    }

    /// Конец декодирования последнего кадра, тики.
    pub(super) const fn decode_end(&self) -> i128 {
        self.next_decode_time
    }

    /// Накопленные точки произвольного доступа.
    pub(super) const fn random_access(&self) -> RandomAccessScan {
        self.random_access
    }

    /// Учитывает кадры одного `trun`.
    pub(super) fn walk_run(&mut self, body: &[u8]) -> Result<(), SegmentTimingError> {
        let mut cursor = ByteCursor::new(body, TimingStructure::IsoTrackRun);
        let header = cursor.read_full_box_header()?;
        if header.version > 1 {
            return Err(SegmentTimingError::UnsupportedVersion {
                structure: TimingStructure::IsoTrackRun,
                version: header.version,
            });
        }
        let sample_count = cursor.read_u32()?;
        if header.has_flag(TRUN_DATA_OFFSET) {
            cursor.skip(4)?; // data_offset: i32
        }
        let first_sample_flags = if header.has_flag(TRUN_FIRST_SAMPLE_FLAGS) {
            Some(SampleFlags::new(cursor.read_u32()?))
        } else {
            None
        };
        let layout = SampleEntryLayout::from_header(header);
        if sample_count == 0 {
            return Ok(());
        }
        if layout.entry_bytes() == 0 {
            return self.include_uniform_samples(sample_count, first_sample_flags);
        }
        ensure_entries_fit(sample_count, layout.entry_bytes(), cursor.remaining_len())?;
        for sample in 0..sample_count {
            let inherited_flags = first_sample_flags.filter(|_| sample == 0);
            self.include_listed_sample(&mut cursor, layout, inherited_flags)?;
        }
        Ok(())
    }

    /// Кадры без записей: все одной длительности по умолчанию, без смещения
    /// композиции. Считается одной формулой — объявленное число кадров ничего
    /// не стоит странице, и перебирать его поштучно было бы уязвимостью.
    fn include_uniform_samples(
        &mut self,
        sample_count: u32,
        first_sample_flags: Option<SampleFlags>,
    ) -> Result<(), SegmentTimingError> {
        // u32 × u32 помещается в i128, но проверка — по общему правилу разборщика:
        // каждая операция над числами страницы проверяется.
        let total_duration = i128::from(sample_count)
            .checked_mul(i128::from(self.default_sample_duration))
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        let start = self.next_decode_time;
        let end = checked_ticks_add(start, total_duration)?;
        self.extent.include(start, end);
        self.random_access.include_uniform_samples(
            start,
            i128::from(self.default_sample_duration),
            sample_count,
            first_sample_flags,
        )?;
        self.next_decode_time = end;
        Ok(())
    }

    /// Один кадр из записей `trun`; `inherited_flags` — `first_sample_flags` для первого
    /// кадра (флаги самой записи главнее).
    fn include_listed_sample(
        &mut self,
        cursor: &mut ByteCursor<'_>,
        layout: SampleEntryLayout,
        inherited_flags: Option<SampleFlags>,
    ) -> Result<(), SegmentTimingError> {
        let duration = if layout.has_duration {
            cursor.read_u32()?
        } else {
            self.default_sample_duration
        };
        if layout.has_size {
            cursor.skip(TRUN_FIELD_BYTES)?;
        }
        let own_flags = if layout.has_flags {
            Some(SampleFlags::new(cursor.read_u32()?))
        } else {
            None
        };
        let composition_offset = match layout.composition_offset {
            CompositionOffsetField::Absent => 0,
            CompositionOffsetField::Unsigned => i128::from(cursor.read_u32()?),
            CompositionOffsetField::Signed => i128::from(cursor.read_i32()?),
        };
        let decode_time = self.next_decode_time;
        let presentation_start = checked_ticks_add(decode_time, composition_offset)?;
        let presentation_end = checked_ticks_add(presentation_start, i128::from(duration))?;
        self.extent.include(presentation_start, presentation_end);
        self.random_access
            .include_sample(presentation_start, own_flags.or(inherited_flags));
        self.next_decode_time = checked_ticks_add(decode_time, i128::from(duration))?;
        Ok(())
    }
}

/// Как записано смещение композиции кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompositionOffsetField {
    /// Поля нет: время показа = время декодирования.
    Absent,
    /// `trun` версии 0: беззнаковое.
    Unsigned,
    /// `trun` версии 1: со знаком.
    Signed,
}

/// Какие поля есть в записи кадра `trun`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleEntryLayout {
    /// Своя длительность.
    has_duration: bool,
    /// Размер (времени не касается, пропускается).
    has_size: bool,
    /// Флаги (из них — точки произвольного доступа).
    has_flags: bool,
    /// Смещение композиции.
    composition_offset: CompositionOffsetField,
}

impl SampleEntryLayout {
    /// Состав записи по флагам и версии `trun`.
    fn from_header(header: FullBoxHeader) -> Self {
        let composition_offset = match (
            header.has_flag(TRUN_SAMPLE_COMPOSITION_OFFSET),
            header.version,
        ) {
            (false, _) => CompositionOffsetField::Absent,
            (true, 0) => CompositionOffsetField::Unsigned,
            (true, _) => CompositionOffsetField::Signed,
        };
        Self {
            has_duration: header.has_flag(TRUN_SAMPLE_DURATION),
            has_size: header.has_flag(TRUN_SAMPLE_SIZE),
            has_flags: header.has_flag(TRUN_SAMPLE_FLAGS),
            composition_offset,
        }
    }

    /// Байтов в одной записи кадра.
    fn entry_bytes(self) -> usize {
        let present_fields = [
            self.has_duration,
            self.has_size,
            self.has_flags,
            self.composition_offset != CompositionOffsetField::Absent,
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        present_fields * TRUN_FIELD_BYTES
    }
}

/// Проверяет ДО перебора, что объявленные записи кадров помещаются в тело `trun`.
fn ensure_entries_fit(
    sample_count: u32,
    entry_bytes: usize,
    available_bytes: usize,
) -> Result<(), SegmentTimingError> {
    let fits = usize::try_from(sample_count)
        .ok()
        .and_then(|count| count.checked_mul(entry_bytes))
        .is_some_and(|required| required <= available_bytes);
    if fits {
        Ok(())
    } else {
        Err(SegmentTimingError::SampleCountExceedsBox {
            sample_count,
            available_bytes,
        })
    }
}

/// Сложение тиков с проверкой переполнения.
fn checked_ticks_add(left: i128, right: i128) -> Result<i128, SegmentTimingError> {
    left.checked_add(right)
        .ok_or(SegmentTimingError::TimeOutOfRange)
}
