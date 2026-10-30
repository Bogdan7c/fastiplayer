//! Время сегментов fMP4 (ISO/IEC 14496-12, фрагменты `moof`).
//!
//! Init (`moov`) даёт для каждой дорожки шкалу времени (`mdhd.timescale`, тиков
//! в секунду) и длительность кадра по умолчанию (`mvex/trex`). Медиасегмент —
//! `moof` + `mdat`; в `moof` на каждую дорожку свой `traf`:
//! - `tfhd` — номер дорожки и, возможно, своя длительность кадра по умолчанию;
//! - `tfdt` — время ДЕКОДИРОВАНИЯ первого кадра фрагмента;
//! - `trun` (один или несколько подряд) — кадры: длительность каждого (флаг
//!   0x100, иначе значение по умолчанию из `tfhd`, иначе из `trex`, иначе 0) и
//!   смещение композиции (флаг 0x800; в версии 0 беззнаковое, в версии 1 со знаком).
//!
//! Отрезок дорожки — время ПОКАЗА: `[min(dts + cto), max(dts + cto + duration))`,
//! как `buffered` в MSE. Кадры перебираются всё равно (ради длительностей),
//! поэтому смещения композиции учитываются почти бесплатно. Списки правок
//! (`elst`) не применяются — так же их игнорирует расчёт `buffered` для
//! фрагментов в браузерах; для полки с точностью «сегмент» это допустимо.
//! Отрезок сегмента — объединение отрезков всех дорожек.

use std::collections::BTreeMap;

use crate::framing::read_box_header;

use super::byte_cursor::{ByteCursor, FullBoxHeader};
use super::{MediaTime, MediaTimeSpan, SegmentTimingError, TimingStructure};

/// `tfhd`: есть 64-битный `base_data_offset`.
const TFHD_BASE_DATA_OFFSET: u32 = 0x00_0001;
/// `tfhd`: есть `sample_description_index`.
const TFHD_SAMPLE_DESCRIPTION_INDEX: u32 = 0x00_0002;
/// `tfhd`: есть `default_sample_duration`.
const TFHD_DEFAULT_SAMPLE_DURATION: u32 = 0x00_0008;

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
/// Байтов временных полей версии 0 (`creation_time` + `modification_time`).
const VERSION_0_TIMES_BYTES: usize = 8;
/// Байтов временных полей версии 1.
const VERSION_1_TIMES_BYTES: usize = 16;

/// Что init сообщает о времени одной дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IsoTrackTiming {
    /// Тиков в секунду; не ноль (проверено при разборе).
    timescale: u32,
    /// Длительность кадра по умолчанию из `trex`, если `trex` для дорожки есть.
    extends_default_sample_duration: Option<u32>,
}

/// Контекст времени init-сегмента fMP4: дорожки по `track_ID`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IsoBmffTimingContext {
    /// Дорожки init-сегмента.
    tracks: BTreeMap<u32, IsoTrackTiming>,
}

impl IsoBmffTimingContext {
    /// Читает `moov` init-сегмента: дорожки, их шкалы и `trex`.
    pub(super) fn parse(initialization: &[u8]) -> Result<Self, SegmentTimingError> {
        let movie = unique_child(
            initialization,
            *b"moov",
            TimingStructure::IsoInitialization,
            TimingStructure::IsoMovie,
        )?
        .ok_or(SegmentTimingError::Missing {
            structure: TimingStructure::IsoMovie,
        })?;

        let mut tracks = BTreeMap::new();
        let mut extends_defaults = Vec::new();
        for child in box_children(movie, TimingStructure::IsoMovie) {
            let child = child?;
            match &child.box_type {
                b"trak" => {
                    let (track_id, timescale) = parse_track(child.body)?;
                    let timing = IsoTrackTiming {
                        timescale,
                        extends_default_sample_duration: None,
                    };
                    if tracks.insert(track_id, timing).is_some() {
                        return Err(SegmentTimingError::DuplicateTrack {
                            track: u64::from(track_id),
                        });
                    }
                }
                b"mvex" => collect_track_extends(child.body, &mut extends_defaults)?,
                _ => {}
            }
        }
        // `trex` может стоять и до `trak`, поэтому значения раскладываются после обхода.
        for (track_id, default_duration) in extends_defaults {
            let track = tracks
                .get_mut(&track_id)
                .ok_or(SegmentTimingError::UnknownTrack {
                    track: u64::from(track_id),
                })?;
            track.extends_default_sample_duration = Some(default_duration);
        }
        if tracks.is_empty() {
            return Err(SegmentTimingError::Missing {
                structure: TimingStructure::IsoTrack,
            });
        }
        Ok(Self { tracks })
    }

    /// Отрезок времени показа медиасегмента `moof` + `mdat`.
    pub(super) fn segment_span(&self, segment: &[u8]) -> Result<MediaTimeSpan, SegmentTimingError> {
        let movie_fragment = first_child(segment, *b"moof", TimingStructure::IsoMediaSegment)?
            .ok_or(SegmentTimingError::Missing {
                structure: TimingStructure::IsoMovieFragment,
            })?;

        let mut segment_span: Option<MediaTimeSpan> = None;
        // Если ни в одной дорожке нет кадров — пустой отрезок в самом раннем `tfdt`.
        let mut earliest_empty_start: Option<MediaTime> = None;
        for child in box_children(movie_fragment, TimingStructure::IsoMovieFragment) {
            let child = child?;
            if &child.box_type != b"traf" {
                continue;
            }
            match self.track_fragment_span(child.body)? {
                TrackFragmentSpan::Samples(span) => {
                    segment_span = Some(segment_span.map_or(span, |known| known.union(span)));
                }
                TrackFragmentSpan::NoSamples(start) => {
                    earliest_empty_start =
                        Some(earliest_empty_start.map_or(start, |known| known.min(start)));
                }
            }
        }
        segment_span
            .or_else(|| earliest_empty_start.map(MediaTimeSpan::empty_at))
            .ok_or(SegmentTimingError::Missing {
                structure: TimingStructure::IsoTrackFragment,
            })
    }

    /// Отрезок одного `traf`.
    fn track_fragment_span(
        &self,
        track_fragment: &[u8],
    ) -> Result<TrackFragmentSpan, SegmentTimingError> {
        let header_body = unique_child(
            track_fragment,
            *b"tfhd",
            TimingStructure::IsoTrackFragment,
            TimingStructure::IsoTrackFragmentHeader,
        )?
        .ok_or(SegmentTimingError::Missing {
            structure: TimingStructure::IsoTrackFragmentHeader,
        })?;
        let header = TrackFragmentHeader::parse(header_body)?;
        let track = self
            .tracks
            .get(&header.track_id)
            .ok_or(SegmentTimingError::UnknownTrack {
                track: u64::from(header.track_id),
            })?;
        // Без `tfdt` время фрагмента зависело бы от прошлых сегментов, а разборщик
        // сегменты не помнит; потоки DASH/HLS/CMAF `tfdt` пишут всегда.
        let decode_time_body = unique_child(
            track_fragment,
            *b"tfdt",
            TimingStructure::IsoTrackFragment,
            TimingStructure::IsoDecodeTime,
        )?
        .ok_or(SegmentTimingError::Missing {
            structure: TimingStructure::IsoDecodeTime,
        })?;
        let base_decode_time = parse_base_decode_time(decode_time_body)?;

        let default_sample_duration = header
            .default_sample_duration
            .or(track.extends_default_sample_duration)
            .unwrap_or(0);
        let mut runs = RunWalker {
            next_decode_time: i128::from(base_decode_time),
            default_sample_duration,
            extent: PresentationExtent::default(),
        };
        for child in box_children(track_fragment, TimingStructure::IsoTrackFragment) {
            let child = child?;
            if &child.box_type == b"trun" {
                runs.walk_run(child.body)?;
            }
        }

        let timescale = u64::from(track.timescale);
        match runs.extent.bounds {
            Some((earliest, latest_end)) => {
                let start = MediaTime::from_ticks(earliest, timescale)?;
                let end = MediaTime::from_ticks(latest_end, timescale)?;
                MediaTimeSpan::new(start, end)
                    .map(TrackFragmentSpan::Samples)
                    .ok_or(SegmentTimingError::TimeOutOfRange)
            }
            None => MediaTime::from_ticks(i128::from(base_decode_time), timescale)
                .map(TrackFragmentSpan::NoSamples),
        }
    }
}

/// Итог одного `traf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackFragmentSpan {
    /// Кадры есть: отрезок их времени показа.
    Samples(MediaTimeSpan),
    /// Кадров нет: только время начала из `tfdt`.
    NoSamples(MediaTime),
}

/// Нужные времени поля `tfhd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackFragmentHeader {
    /// Номер дорожки.
    track_id: u32,
    /// Своя длительность кадра по умолчанию, если указана.
    default_sample_duration: Option<u32>,
}

impl TrackFragmentHeader {
    /// Читает `tfhd`; поля после длительности по умолчанию времени не касаются.
    fn parse(body: &[u8]) -> Result<Self, SegmentTimingError> {
        let mut cursor = ByteCursor::new(body, TimingStructure::IsoTrackFragmentHeader);
        let header = cursor.read_full_box_header()?;
        let track_id = cursor.read_u32()?;
        if header.has_flag(TFHD_BASE_DATA_OFFSET) {
            cursor.skip(8)?; // base_data_offset: u64
        }
        if header.has_flag(TFHD_SAMPLE_DESCRIPTION_INDEX) {
            cursor.skip(4)?; // sample_description_index: u32
        }
        let default_sample_duration = if header.has_flag(TFHD_DEFAULT_SAMPLE_DURATION) {
            Some(cursor.read_u32()?)
        } else {
            None
        };
        Ok(Self {
            track_id,
            default_sample_duration,
        })
    }
}

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
struct RunWalker {
    /// Время декодирования следующего кадра, тики.
    next_decode_time: i128,
    /// Длительность кадра, когда `trun` её не пишет.
    default_sample_duration: u32,
    /// Накопленный отрезок показа.
    extent: PresentationExtent,
}

impl RunWalker {
    /// Учитывает кадры одного `trun`.
    fn walk_run(&mut self, body: &[u8]) -> Result<(), SegmentTimingError> {
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
        if header.has_flag(TRUN_FIRST_SAMPLE_FLAGS) {
            cursor.skip(4)?; // first_sample_flags: u32
        }
        let layout = SampleEntryLayout::from_header(header);
        if sample_count == 0 {
            return Ok(());
        }
        if layout.entry_bytes() == 0 {
            return self.include_uniform_samples(sample_count);
        }
        ensure_entries_fit(sample_count, layout.entry_bytes(), cursor.remaining_len())?;
        for _ in 0..sample_count {
            self.include_listed_sample(&mut cursor, layout)?;
        }
        Ok(())
    }

    /// Кадры без записей: все одной длительности по умолчанию, без смещения
    /// композиции. Считается одной формулой — объявленное число кадров ничего
    /// не стоит странице, и перебирать его поштучно было бы уязвимостью.
    fn include_uniform_samples(&mut self, sample_count: u32) -> Result<(), SegmentTimingError> {
        // u32 × u32 помещается в i128, но проверка — по общему правилу разборщика:
        // каждая операция над числами страницы проверяется.
        let total_duration = i128::from(sample_count)
            .checked_mul(i128::from(self.default_sample_duration))
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        let start = self.next_decode_time;
        let end = checked_ticks_add(start, total_duration)?;
        self.extent.include(start, end);
        self.next_decode_time = end;
        Ok(())
    }

    /// Один кадр из записей `trun`.
    fn include_listed_sample(
        &mut self,
        cursor: &mut ByteCursor<'_>,
        layout: SampleEntryLayout,
    ) -> Result<(), SegmentTimingError> {
        let duration = if layout.has_duration {
            cursor.read_u32()?
        } else {
            self.default_sample_duration
        };
        if layout.has_size {
            cursor.skip(TRUN_FIELD_BYTES)?;
        }
        if layout.has_flags {
            cursor.skip(TRUN_FIELD_BYTES)?;
        }
        let composition_offset = match layout.composition_offset {
            CompositionOffsetField::Absent => 0,
            CompositionOffsetField::Unsigned => i128::from(cursor.read_u32()?),
            CompositionOffsetField::Signed => i128::from(cursor.read_i32()?),
        };
        let decode_time = self.next_decode_time;
        let presentation_start = checked_ticks_add(decode_time, composition_offset)?;
        let presentation_end = checked_ticks_add(presentation_start, i128::from(duration))?;
        self.extent.include(presentation_start, presentation_end);
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
    /// Флаги (времени не касаются, пропускаются).
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

/// `trak`: номер дорожки из `tkhd` и шкала из `mdia/mdhd`.
fn parse_track(track: &[u8]) -> Result<(u32, u32), SegmentTimingError> {
    let header_body = unique_child(
        track,
        *b"tkhd",
        TimingStructure::IsoTrack,
        TimingStructure::IsoTrackHeader,
    )?
    .ok_or(SegmentTimingError::Missing {
        structure: TimingStructure::IsoTrackHeader,
    })?;
    let track_id = read_after_versioned_times(header_body, TimingStructure::IsoTrackHeader)?;

    let media = unique_child(
        track,
        *b"mdia",
        TimingStructure::IsoTrack,
        TimingStructure::IsoTrack,
    )?
    .ok_or(SegmentTimingError::Missing {
        structure: TimingStructure::IsoTrack,
    })?;
    let media_header_body = unique_child(
        media,
        *b"mdhd",
        TimingStructure::IsoTrack,
        TimingStructure::IsoMediaHeader,
    )?
    .ok_or(SegmentTimingError::Missing {
        structure: TimingStructure::IsoMediaHeader,
    })?;
    let timescale = read_after_versioned_times(media_header_body, TimingStructure::IsoMediaHeader)?;
    if timescale == 0 {
        return Err(SegmentTimingError::ZeroTimescale { track: track_id });
    }
    Ok((track_id, timescale))
}

/// У `tkhd` и `mdhd` за временами создания/изменения (32 или 64 бита по версии)
/// идёт нужное 32-битное поле: `track_ID` и `timescale` соответственно.
fn read_after_versioned_times(
    body: &[u8],
    structure: TimingStructure,
) -> Result<u32, SegmentTimingError> {
    let mut cursor = ByteCursor::new(body, structure);
    let header = cursor.read_full_box_header()?;
    let times_bytes = match header.version {
        0 => VERSION_0_TIMES_BYTES,
        1 => VERSION_1_TIMES_BYTES,
        version => {
            return Err(SegmentTimingError::UnsupportedVersion { structure, version });
        }
    };
    cursor.skip(times_bytes)?;
    cursor.read_u32()
}

/// `mvex`: длительности по умолчанию из всех `trex`.
fn collect_track_extends(
    movie_extends: &[u8],
    extends_defaults: &mut Vec<(u32, u32)>,
) -> Result<(), SegmentTimingError> {
    for child in box_children(movie_extends, TimingStructure::IsoMovie) {
        let child = child?;
        if &child.box_type != b"trex" {
            continue;
        }
        let mut cursor = ByteCursor::new(child.body, TimingStructure::IsoTrackExtends);
        cursor.read_full_box_header()?;
        let track_id = cursor.read_u32()?;
        cursor.skip(4)?; // default_sample_description_index: u32
        let default_sample_duration = cursor.read_u32()?;
        if extends_defaults.iter().any(|(known, _)| *known == track_id) {
            return Err(SegmentTimingError::Duplicate {
                structure: TimingStructure::IsoTrackExtends,
            });
        }
        extends_defaults.push((track_id, default_sample_duration));
    }
    Ok(())
}

/// `tfdt`: базовое время декодирования (32 бита в версии 0, 64 — в версии 1).
fn parse_base_decode_time(body: &[u8]) -> Result<u64, SegmentTimingError> {
    let mut cursor = ByteCursor::new(body, TimingStructure::IsoDecodeTime);
    let header = cursor.read_full_box_header()?;
    match header.version {
        0 => cursor.read_u32().map(u64::from),
        1 => cursor.read_u64(),
        version => Err(SegmentTimingError::UnsupportedVersion {
            structure: TimingStructure::IsoDecodeTime,
            version,
        }),
    }
}

/// Один бокс внутри родителя: тип и тело без заголовка.
#[derive(Debug, Clone, Copy)]
struct IsoChild<'a> {
    /// Четырёхсимвольный тип.
    box_type: [u8; 4],
    /// Тело.
    body: &'a [u8],
}

/// Обход детей одного бокса. После первой ошибки обход заканчивается.
#[derive(Debug)]
struct IsoChildren<'a> {
    /// Ещё не прочитанные байты родителя.
    remaining: &'a [u8],
    /// Родитель — для ошибок.
    parent: TimingStructure,
}

impl<'a> Iterator for IsoChildren<'a> {
    type Item = Result<IsoChild<'a>, SegmentTimingError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }
        let parsed = self.split_next();
        if parsed.is_err() {
            // Дальше разбирать нечего: граница следующего бокса неизвестна.
            self.remaining = &[];
        }
        Some(parsed)
    }
}

impl<'a> IsoChildren<'a> {
    /// Отрезает следующий целый бокс.
    fn split_next(&mut self) -> Result<IsoChild<'a>, SegmentTimingError> {
        let truncated = SegmentTimingError::Truncated {
            structure: self.parent,
        };
        let header = read_box_header(self.remaining)
            .map_err(|_| SegmentTimingError::InvalidHeader {
                structure: self.parent,
            })?
            .ok_or(truncated.clone())?;
        let total_len = usize::try_from(header.total_len).map_err(|_| truncated.clone())?;
        let whole_box = self.remaining.get(..total_len).ok_or(truncated.clone())?;
        let body = whole_box.get(header.header_len..).ok_or(truncated)?;
        self.remaining = self.remaining.get(total_len..).unwrap_or_default();
        Ok(IsoChild {
            box_type: header.box_type,
            body,
        })
    }
}

/// Дети бокса с телом `parent_body`.
fn box_children(parent_body: &[u8], parent: TimingStructure) -> IsoChildren<'_> {
    IsoChildren {
        remaining: parent_body,
        parent,
    }
}

/// Тело единственного ребёнка нужного типа; два таких — ошибка `Duplicate`.
/// Все дети проверяются целиком: испорченный хвост родителя — тоже ошибка.
fn unique_child(
    parent_body: &[u8],
    box_type: [u8; 4],
    parent: TimingStructure,
    child: TimingStructure,
) -> Result<Option<&[u8]>, SegmentTimingError> {
    let mut found = None;
    for candidate in box_children(parent_body, parent) {
        let candidate = candidate?;
        if candidate.box_type != box_type {
            continue;
        }
        if found.is_some() {
            return Err(SegmentTimingError::Duplicate { structure: child });
        }
        found = Some(candidate.body);
    }
    Ok(found)
}

/// Тело первого ребёнка нужного типа; обход останавливается на нём (у
/// медиасегмента за `moof` идёт большой `mdat`, читать его не нужно).
fn first_child(
    parent_body: &[u8],
    box_type: [u8; 4],
    parent: TimingStructure,
) -> Result<Option<&[u8]>, SegmentTimingError> {
    for candidate in box_children(parent_body, parent) {
        let candidate = candidate?;
        if candidate.box_type == box_type {
            return Ok(Some(candidate.body));
        }
    }
    Ok(None)
}
