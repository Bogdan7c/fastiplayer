//! Время кластеров WebM (Matroska, RFC 9559).
//!
//! Init даёт `Info/TimecodeScale` (наносекунд в одном тике меток; по умолчанию
//! 1 000 000 — тик = 1 мс) и для каждой дорожки `TrackNumber` → `DefaultDuration`
//! (наносекунды на кадр, необязательно). Init, который хранит сканер, имеет
//! `Segment` неизвестного размера — его тело идёт до конца init-сегмента.
//!
//! Кластер: `Timecode` — время начала кластера в тиках; кадры — `SimpleBlock`
//! или `BlockGroup` с `Block` и, возможно, `BlockDuration` (в тиках). В заголовке
//! блока после номера дорожки (число EBML переменной длины) идёт знаковая 16-битная
//! метка ОТНОСИТЕЛЬНО `Timecode`, затем байт флагов (биты 0x06 — тип «лейсинга»:
//! несколько кадров в одном блоке; тогда следующий байт = число кадров − 1).
//!
//! Метки блоков WebM — это уже время показа. Длительность блока: `BlockDuration`,
//! иначе `DefaultDuration` × число кадров в блоке, иначе не записана. Отрезок
//! кластера — `[min(начало блока), max(конец))`; если длительность последнего
//! блока дорожки не записана, его конец оценивается по шагу между двумя
//! последними блоками дорожки ([`SegmentEnd::Estimated`]), а у дорожки с одним
//! блоком конец открыт ([`SegmentEnd::Open`]); кластер без блоков — пустой
//! отрезок в его `Timecode`. `CodecDelay` (у Opus) не вычитается: это
//! сдвиг на единицы миллисекунд, для полки с точностью «сегмент» он незначим.
//!
//! Вложенность разбирается явными циклами фиксированной глубины
//! (Segment → Tracks → TrackEntry, Cluster → BlockGroup), рекурсии нет.

use std::collections::BTreeMap;

use crate::framing::{EbmlElementSize, read_element_header};

use super::byte_cursor::ByteCursor;
use super::{MediaTime, MediaTimeSpan, SegmentEnd, SegmentTimingError, TimedSpan, TimingStructure};

// ID элементов вместе с битом-маркером, как в спецификации. Сканер `matroska.rs`
// держит часть из них приватными; здесь — свой словарь только для времени.
/// EBML-заголовок файла.
const EBML_HEADER_ID: u32 = 0x1A45_DFA3;
/// `Segment`.
const SEGMENT_ID: u32 = 0x1853_8067;
/// `Info`.
const INFO_ID: u32 = 0x1549_A966;
/// `Info/TimecodeScale`.
const TIMECODE_SCALE_ID: u32 = 0x2A_D7B1;
/// `Tracks`.
const TRACKS_ID: u32 = 0x1654_AE6B;
/// `Tracks/TrackEntry`.
const TRACK_ENTRY_ID: u32 = 0xAE;
/// `TrackEntry/TrackNumber`.
const TRACK_NUMBER_ID: u32 = 0xD7;
/// `TrackEntry/DefaultDuration`.
const DEFAULT_DURATION_ID: u32 = 0x23_E383;
/// `Cluster`.
const CLUSTER_ID: u32 = 0x1F43_B675;
/// `Cluster/Timecode` (в RFC 9559 — `Timestamp`).
const CLUSTER_TIMECODE_ID: u32 = 0xE7;
/// `Cluster/SimpleBlock`.
const SIMPLE_BLOCK_ID: u32 = 0xA3;
/// `Cluster/BlockGroup`.
const BLOCK_GROUP_ID: u32 = 0xA0;
/// `BlockGroup/Block`.
const BLOCK_ID: u32 = 0xA1;
/// `BlockGroup/BlockDuration`.
const BLOCK_DURATION_ID: u32 = 0x9B;

/// `TimecodeScale` по умолчанию: тик = 1 мс.
const DEFAULT_TIMECODE_SCALE_NS: u64 = 1_000_000;
/// Наносекунд в секунде: шкала, в которой считается время кластера.
const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Самое длинное беззнаковое целое EBML.
const MAX_UNSIGNED_INTEGER_BYTES: usize = 8;
/// Самый длинный номер дорожки в заголовке блока (число EBML переменной длины).
const MAX_TRACK_NUMBER_BYTES: usize = 8;
/// Биты типа лейсинга в байте флагов блока.
const BLOCK_LACING_MASK: u8 = 0x06;

/// Контекст времени init-сегмента WebM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatroskaTimingContext {
    /// Наносекунд в одном тике меток; не ноль.
    timecode_scale_ns: u64,
    /// `TrackNumber` → `DefaultDuration` в наносекундах (если указана).
    default_durations_ns: BTreeMap<u64, Option<u64>>,
}

impl MatroskaTimingContext {
    /// Читает `Info` и `Tracks` init-сегмента.
    pub(super) fn parse(initialization: &[u8]) -> Result<Self, SegmentTimingError> {
        let segment = find_segment_body(initialization)?;
        let mut timecode_scale_ns: Option<u64> = None;
        let mut default_durations_ns: Option<BTreeMap<u64, Option<u64>>> = None;
        for child in element_children(segment, TimingStructure::MatroskaSegment) {
            let child = child?;
            if child.id == CLUSTER_ID {
                // Init кончается перед первым кластером.
                break;
            }
            child.require_known_size(TimingStructure::MatroskaSegment)?;
            match child.id {
                INFO_ID => {
                    if timecode_scale_ns.is_some() {
                        return Err(SegmentTimingError::Duplicate {
                            structure: TimingStructure::MatroskaInfo,
                        });
                    }
                    timecode_scale_ns = Some(parse_timecode_scale(child.body)?);
                }
                TRACKS_ID => {
                    if default_durations_ns.is_some() {
                        return Err(SegmentTimingError::Duplicate {
                            structure: TimingStructure::MatroskaTracks,
                        });
                    }
                    default_durations_ns = Some(parse_tracks(child.body)?);
                }
                _ => {}
            }
        }
        Ok(Self {
            timecode_scale_ns: timecode_scale_ns.unwrap_or(DEFAULT_TIMECODE_SCALE_NS),
            default_durations_ns: default_durations_ns.ok_or(SegmentTimingError::Missing {
                structure: TimingStructure::MatroskaTracks,
            })?,
        })
    }

    /// Отрезок времени одного `Cluster`.
    pub(super) fn cluster_span(&self, segment: &[u8]) -> Result<TimedSpan, SegmentTimingError> {
        let cluster = element_children(segment, TimingStructure::MatroskaCluster)
            .next()
            .transpose()?
            .filter(|element| element.id == CLUSTER_ID)
            .ok_or(SegmentTimingError::Missing {
                structure: TimingStructure::MatroskaCluster,
            })?;

        let mut cluster_timecode: Option<u64> = None;
        let mut extent = ClusterExtent::default();
        for child in element_children(cluster.body, TimingStructure::MatroskaCluster) {
            let child = child?;
            child.require_known_size(TimingStructure::MatroskaCluster)?;
            match child.id {
                CLUSTER_TIMECODE_ID => {
                    if cluster_timecode.is_some() {
                        return Err(SegmentTimingError::Duplicate {
                            structure: TimingStructure::MatroskaClusterTimecode,
                        });
                    }
                    cluster_timecode = Some(read_unsigned_integer(child.body)?);
                }
                SIMPLE_BLOCK_ID => {
                    let block = BlockHeader::parse(child.body)?;
                    let timing = self.block_relative_timing_ns(block, None)?;
                    extent.include(block.track_number, timing)?;
                }
                BLOCK_GROUP_ID => {
                    let (block, explicit_duration) = parse_block_group(child.body)?;
                    let timing = self.block_relative_timing_ns(block, explicit_duration)?;
                    extent.include(block.track_number, timing)?;
                }
                _ => {}
            }
        }
        let cluster_timecode = cluster_timecode.ok_or(SegmentTimingError::Missing {
            structure: TimingStructure::MatroskaClusterTimecode,
        })?;
        self.absolute_span(cluster_timecode, extent.finish()?)
    }

    /// Начало блока и его длительность (если записана) в наносекундах
    /// ОТНОСИТЕЛЬНО `Timecode` кластера.
    ///
    /// Вся арифметика — с проверкой: `TimecodeScale`, `BlockDuration` и
    /// `DefaultDuration` — 64-битные числа страницы, их произведение не помещается
    /// даже в i128 (u64::MAX² > i128::MAX).
    fn block_relative_timing_ns(
        &self,
        block: BlockHeader,
        explicit_duration_ticks: Option<u64>,
    ) -> Result<BlockTiming, SegmentTimingError> {
        let default_duration_ns = self.default_durations_ns.get(&block.track_number).ok_or(
            SegmentTimingError::UnknownTrack {
                track: block.track_number,
            },
        )?;
        let scale = i128::from(self.timecode_scale_ns);
        let start = i128::from(block.relative_timecode)
            .checked_mul(scale)
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        let duration = match (explicit_duration_ticks, default_duration_ns) {
            (Some(ticks), _) => Some(
                i128::from(ticks)
                    .checked_mul(scale)
                    .ok_or(SegmentTimingError::TimeOutOfRange)?,
            ),
            (None, Some(per_frame)) => Some(
                i128::from(*per_frame)
                    .checked_mul(i128::from(block.frame_count))
                    .ok_or(SegmentTimingError::TimeOutOfRange)?,
            ),
            // Длительность не записана: конец оценит `ClusterExtent::finish`.
            (None, None) => None,
        };
        Ok(BlockTiming { start, duration })
    }

    /// Переводит относительные границы в абсолютный отрезок по `Timecode` кластера.
    fn absolute_span(
        &self,
        cluster_timecode: u64,
        relative: RelativeSpan,
    ) -> Result<TimedSpan, SegmentTimingError> {
        let cluster_start_ns = i128::from(cluster_timecode)
            .checked_mul(i128::from(self.timecode_scale_ns))
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        let to_media_time = |relative_ns: i128| {
            cluster_start_ns
                .checked_add(relative_ns)
                .ok_or(SegmentTimingError::TimeOutOfRange)
                .and_then(|absolute_ns| MediaTime::from_ticks(absolute_ns, NANOS_PER_SECOND))
        };
        let start = to_media_time(relative.start)?;
        let end = to_media_time(relative.end)?;
        let span = MediaTimeSpan::new(start, end).ok_or(SegmentTimingError::TimeOutOfRange)?;
        let end_kind = match relative.end_kind {
            RelativeEnd::Exact => SegmentEnd::Exact,
            RelativeEnd::Estimated { certain_end } => SegmentEnd::Estimated {
                certain_end: to_media_time(certain_end)?,
            },
            RelativeEnd::Open { certain_end } => SegmentEnd::Open {
                certain_end: to_media_time(certain_end)?,
            },
        };
        Ok(TimedSpan::new(span, end_kind))
    }
}

/// Время одного блока, нс относительно кластера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockTiming {
    /// Начало блока.
    start: i128,
    /// Длительность (`BlockDuration` или `DefaultDuration` × кадров); `None` — не записана.
    duration: Option<i128>,
}

/// Два самых поздних блока одной дорожки: по ним оценивается конец, если
/// длительность последнего не записана. Только два числа на дорожку — память не
/// растёт с числом блоков, которое объявила страница.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackTail {
    /// Начало самого позднего блока дорожки.
    latest_start: i128,
    /// Его длительность, если записана.
    latest_duration: Option<i128>,
    /// Начало предыдущего (более раннего) блока дорожки.
    previous_start: Option<i128>,
}

impl TrackTail {
    /// Хвост из первого блока дорожки.
    const fn first(block: BlockTiming) -> Self {
        Self {
            latest_start: block.start,
            latest_duration: block.duration,
            previous_start: None,
        }
    }

    /// Учитывает ещё один блок дорожки (блоки идут в порядке декодирования,
    /// поэтому сравниваются начала, а не порядок прихода).
    fn include(&mut self, block: BlockTiming) {
        if block.start > self.latest_start {
            self.previous_start = Some(self.latest_start);
            self.latest_start = block.start;
            self.latest_duration = block.duration;
        } else if block.start == self.latest_start {
            self.latest_duration = self.latest_duration.or(block.duration);
        } else if self
            .previous_start
            .is_none_or(|previous| block.start > previous)
        {
            self.previous_start = Some(block.start);
        }
    }

    /// Конец дорожки, если длительность её последнего блока не записана:
    /// `Some(Some(оценка))` — по шагу между двумя последними блоками,
    /// `Some(None)` — мерить не по чему, `None` — длительность записана (оценка не нужна).
    fn estimated_end(self) -> Result<Option<Option<i128>>, SegmentTimingError> {
        if self.latest_duration.is_some() {
            return Ok(None);
        }
        let Some(previous_start) = self.previous_start else {
            return Ok(Some(None));
        };
        let step = self
            .latest_start
            .checked_sub(previous_start)
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        let estimate = self
            .latest_start
            .checked_add(step)
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        Ok(Some(Some(estimate)))
    }
}

/// Как получен конец кластера (нс относительно кластера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelativeEnd {
    /// Записан.
    Exact,
    /// Оценён; точно известно до `certain_end`.
    Estimated {
        /// Докуда время известно точно.
        certain_end: i128,
    },
    /// Открыт; точно известно до `certain_end`.
    Open {
        /// Докуда время известно точно.
        certain_end: i128,
    },
}

/// Отрезок кластера относительно его `Timecode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RelativeSpan {
    /// Самое раннее начало блока.
    start: i128,
    /// Лучший известный конец.
    end: i128,
    /// Как получен конец.
    end_kind: RelativeEnd,
}

/// Всё, что известно про время блоков кластера, нс относительно кластера.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ClusterExtent {
    /// `(min начала, max точно известного конца)`; `None` — блоков не было.
    /// Точно известный конец блока без длительности — его начало.
    bounds: Option<(i128, i128)>,
    /// Хвосты дорожек (дорожек не больше, чем объявил init).
    track_tails: BTreeMap<u64, TrackTail>,
}

impl ClusterExtent {
    /// Учитывает один блок.
    fn include(&mut self, track: u64, block: BlockTiming) -> Result<(), SegmentTimingError> {
        let certain_end = block
            .start
            .checked_add(block.duration.unwrap_or(0))
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        self.bounds = Some(match self.bounds {
            Some((earliest, latest_end)) => {
                (earliest.min(block.start), latest_end.max(certain_end))
            }
            None => (block.start, certain_end),
        });
        self.track_tails
            .entry(track)
            .and_modify(|tail| tail.include(block))
            .or_insert_with(|| TrackTail::first(block));
        Ok(())
    }

    /// Отрезок кластера. Кластер без блоков — пустой отрезок в его `Timecode`.
    ///
    /// Если у последнего блока какой-то дорожки длительность не записана, конец
    /// оценивается шагом между двумя её последними блоками (у видео это ровно
    /// длительность кадра); у дорожки с одним блоком оценивать не по чему — конец
    /// открыт. Конец кластера — наибольший из точных концов и оценок.
    fn finish(self) -> Result<RelativeSpan, SegmentTimingError> {
        let Some((start, certain_end)) = self.bounds else {
            return Ok(RelativeSpan {
                start: 0,
                end: 0,
                end_kind: RelativeEnd::Exact,
            });
        };
        let mut end = certain_end;
        let mut open = false;
        for tail in self.track_tails.values() {
            match tail.estimated_end()? {
                None => {}
                Some(Some(estimate)) => end = end.max(estimate),
                Some(None) => open = true,
            }
        }
        let end_kind = if open {
            RelativeEnd::Open { certain_end }
        } else if end > certain_end {
            RelativeEnd::Estimated { certain_end }
        } else {
            RelativeEnd::Exact
        };
        Ok(RelativeSpan {
            start,
            end,
            end_kind,
        })
    }
}

/// Заголовок блока: всё, что нужно времени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockHeader {
    /// Номер дорожки (`TrackNumber`).
    track_number: u64,
    /// Метка относительно `Timecode` кластера, в тиках.
    relative_timecode: i16,
    /// Кадров в блоке (больше одного — при лейсинге).
    frame_count: u32,
}

impl BlockHeader {
    /// Читает начало тела `SimpleBlock`/`Block`.
    fn parse(body: &[u8]) -> Result<Self, SegmentTimingError> {
        let (track_number, track_number_len) = read_variable_length_value(body)?;
        let mut cursor = ByteCursor::new(
            body.get(track_number_len..).unwrap_or_default(),
            TimingStructure::MatroskaBlock,
        );
        let relative_timecode = cursor.read_i16()?;
        let flags = cursor.read_u8()?;
        let frame_count = if flags & BLOCK_LACING_MASK == 0 {
            1
        } else {
            // При лейсинге первый байт после флагов — число кадров минус один.
            u32::from(cursor.read_u8()?) + 1
        };
        Ok(Self {
            track_number,
            relative_timecode,
            frame_count,
        })
    }
}

/// `BlockGroup`: заголовок `Block` и `BlockDuration`, если есть.
fn parse_block_group(body: &[u8]) -> Result<(BlockHeader, Option<u64>), SegmentTimingError> {
    let mut block: Option<BlockHeader> = None;
    let mut duration_ticks: Option<u64> = None;
    for child in element_children(body, TimingStructure::MatroskaBlockGroup) {
        let child = child?;
        child.require_known_size(TimingStructure::MatroskaBlockGroup)?;
        match child.id {
            BLOCK_ID if block.is_some() => {
                return Err(SegmentTimingError::Duplicate {
                    structure: TimingStructure::MatroskaBlock,
                });
            }
            BLOCK_ID => block = Some(BlockHeader::parse(child.body)?),
            BLOCK_DURATION_ID if duration_ticks.is_some() => {
                return Err(SegmentTimingError::Duplicate {
                    structure: TimingStructure::MatroskaBlockGroup,
                });
            }
            BLOCK_DURATION_ID => duration_ticks = Some(read_unsigned_integer(child.body)?),
            _ => {}
        }
    }
    let block = block.ok_or(SegmentTimingError::Missing {
        structure: TimingStructure::MatroskaBlock,
    })?;
    Ok((block, duration_ticks))
}

/// Тело `Segment` init-сегмента (после EBML-заголовка).
fn find_segment_body(initialization: &[u8]) -> Result<&[u8], SegmentTimingError> {
    for element in element_children(initialization, TimingStructure::MatroskaInitialization) {
        let element = element?;
        match element.id {
            SEGMENT_ID => return Ok(element.body),
            EBML_HEADER_ID => {
                element.require_known_size(TimingStructure::MatroskaInitialization)?;
            }
            _ => {
                return Err(SegmentTimingError::Missing {
                    structure: TimingStructure::MatroskaSegment,
                });
            }
        }
    }
    Err(SegmentTimingError::Missing {
        structure: TimingStructure::MatroskaSegment,
    })
}

/// `Info`: `TimecodeScale` или значение по умолчанию.
fn parse_timecode_scale(info: &[u8]) -> Result<u64, SegmentTimingError> {
    let mut scale = DEFAULT_TIMECODE_SCALE_NS;
    for child in element_children(info, TimingStructure::MatroskaInfo) {
        let child = child?;
        child.require_known_size(TimingStructure::MatroskaInfo)?;
        if child.id == TIMECODE_SCALE_ID {
            scale = read_unsigned_integer(child.body)?;
        }
    }
    if scale == 0 {
        return Err(SegmentTimingError::ZeroTimecodeScale);
    }
    Ok(scale)
}

/// `Tracks`: номер каждой дорожки и её `DefaultDuration`.
fn parse_tracks(tracks: &[u8]) -> Result<BTreeMap<u64, Option<u64>>, SegmentTimingError> {
    let mut default_durations_ns = BTreeMap::new();
    for entry in element_children(tracks, TimingStructure::MatroskaTracks) {
        let entry = entry?;
        entry.require_known_size(TimingStructure::MatroskaTracks)?;
        if entry.id != TRACK_ENTRY_ID {
            continue;
        }
        let mut track_number: Option<u64> = None;
        let mut default_duration_ns: Option<u64> = None;
        for field in element_children(entry.body, TimingStructure::MatroskaTracks) {
            let field = field?;
            field.require_known_size(TimingStructure::MatroskaTracks)?;
            match field.id {
                TRACK_NUMBER_ID => track_number = Some(read_unsigned_integer(field.body)?),
                DEFAULT_DURATION_ID => {
                    default_duration_ns = Some(read_unsigned_integer(field.body)?);
                }
                _ => {}
            }
        }
        let track_number = track_number.ok_or(SegmentTimingError::Missing {
            structure: TimingStructure::MatroskaTracks,
        })?;
        if default_durations_ns
            .insert(track_number, default_duration_ns)
            .is_some()
        {
            return Err(SegmentTimingError::DuplicateTrack {
                track: track_number,
            });
        }
    }
    Ok(default_durations_ns)
}

/// Беззнаковое целое EBML: 0–8 байтов big-endian (пустое тело = 0).
fn read_unsigned_integer(body: &[u8]) -> Result<u64, SegmentTimingError> {
    if body.len() > MAX_UNSIGNED_INTEGER_BYTES {
        return Err(SegmentTimingError::InvalidHeader {
            structure: TimingStructure::MatroskaUnsignedInteger,
        });
    }
    Ok(body
        .iter()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte)))
}

/// Значение числа EBML переменной длины (без бита-маркера) и его длина в байтах.
fn read_variable_length_value(bytes: &[u8]) -> Result<(u64, usize), SegmentTimingError> {
    let truncated = SegmentTimingError::Truncated {
        structure: TimingStructure::MatroskaBlock,
    };
    let first = *bytes.first().ok_or(truncated.clone())?;
    // Длина = число ведущих нулей первого байта + 1; нулевой байт длину не задаёт.
    let length = first.leading_zeros() as usize + 1;
    if first == 0 || length > MAX_TRACK_NUMBER_BYTES {
        return Err(SegmentTimingError::InvalidHeader {
            structure: TimingStructure::MatroskaBlock,
        });
    }
    let encoded = bytes.get(..length).ok_or(truncated)?;
    // Бит-маркер стоит на позиции `8 - length` первого байта; данные — биты ниже него.
    let marker_free_first = u64::from(first) & (0xFF_u64 >> length);
    let value = encoded
        .iter()
        .skip(1)
        .fold(marker_free_first, |value, byte| {
            (value << 8) | u64::from(*byte)
        });
    Ok((value, length))
}

/// Объявлен ли размер элемента или тело идёт до конца родителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclaredBodySize {
    /// Размер указан.
    Known,
    /// Размер «неизвестен»: тело — весь остаток родителя.
    ToParentEnd,
}

/// Один элемент внутри родителя.
#[derive(Debug, Clone, Copy)]
struct EbmlChild<'a> {
    /// ID вместе с битом-маркером.
    id: u32,
    /// Тело элемента.
    body: &'a [u8],
    /// Как получена граница тела.
    size: DeclaredBodySize,
}

impl EbmlChild<'_> {
    /// Неизвестный размер допустим только у `Segment` и `Cluster`.
    fn require_known_size(&self, parent: TimingStructure) -> Result<(), SegmentTimingError> {
        match self.size {
            DeclaredBodySize::Known => Ok(()),
            DeclaredBodySize::ToParentEnd => {
                Err(SegmentTimingError::InvalidHeader { structure: parent })
            }
        }
    }
}

/// Обход детей одного элемента. После первой ошибки обход заканчивается.
#[derive(Debug)]
struct EbmlChildren<'a> {
    /// Ещё не прочитанные байты родителя.
    remaining: &'a [u8],
    /// Родитель — для ошибок.
    parent: TimingStructure,
}

impl<'a> Iterator for EbmlChildren<'a> {
    type Item = Result<EbmlChild<'a>, SegmentTimingError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }
        let parsed = self.split_next();
        if parsed.is_err() {
            // Граница следующего элемента неизвестна — обход окончен.
            self.remaining = &[];
        }
        Some(parsed)
    }
}

impl<'a> EbmlChildren<'a> {
    /// Отрезает следующий элемент.
    fn split_next(&mut self) -> Result<EbmlChild<'a>, SegmentTimingError> {
        let truncated = SegmentTimingError::Truncated {
            structure: self.parent,
        };
        let header = read_element_header(self.remaining)
            .map_err(|_| SegmentTimingError::InvalidHeader {
                structure: self.parent,
            })?
            .ok_or(truncated.clone())?;
        let after_header = self
            .remaining
            .get(header.header_len..)
            .ok_or(truncated.clone())?;
        let (body, size, rest) = match header.size {
            EbmlElementSize::Known(body_len) => {
                let body_len = usize::try_from(body_len).map_err(|_| truncated.clone())?;
                let body = after_header.get(..body_len).ok_or(truncated)?;
                let rest = after_header.get(body_len..).unwrap_or_default();
                (body, DeclaredBodySize::Known, rest)
            }
            EbmlElementSize::Unknown => (after_header, DeclaredBodySize::ToParentEnd, &[][..]),
        };
        self.remaining = rest;
        Ok(EbmlChild {
            id: header.id,
            body,
            size,
        })
    }
}

/// Дети элемента с телом `parent_body`.
fn element_children(parent_body: &[u8], parent: TimingStructure) -> EbmlChildren<'_> {
    EbmlChildren {
        remaining: parent_body,
        parent,
    }
}
