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
//! Отрезок сегмента — объединение отрезков всех дорожек; отрезок каждой дорожки
//! отдельно уходит в [`TrackSpans`] (по месту дорожки в полосе, `track_spans`).

use std::collections::BTreeMap;

use crate::framing::read_box_header;

use super::byte_cursor::ByteCursor;
use super::iso_bmff_run::RunWalker;
use super::random_access::{RandomAccessScan, SampleFlags, TrackAccess};
use super::{
    DecodeOrder, LaneContent, MediaTime, MediaTimeSpan, SegmentRandomAccess, SegmentTimingError,
    TimingStructure, TrackMedia, TrackSlot, TrackSlotAssigner, TrackSpans,
};

/// `tfhd`: есть 64-битный `base_data_offset`.
const TFHD_BASE_DATA_OFFSET: u32 = 0x00_0001;
/// `tfhd`: есть `sample_description_index`.
const TFHD_SAMPLE_DESCRIPTION_INDEX: u32 = 0x00_0002;
/// `tfhd`: есть `default_sample_duration`.
const TFHD_DEFAULT_SAMPLE_DURATION: u32 = 0x00_0008;
/// `tfhd`: есть `default_sample_size`.
const TFHD_DEFAULT_SAMPLE_SIZE: u32 = 0x00_0010;
/// `tfhd`: есть `default_sample_flags`.
const TFHD_DEFAULT_SAMPLE_FLAGS: u32 = 0x00_0020;

/// Байтов `default_sample_size` + `default_sample_flags` в конце `trex`.
const TREX_SIZE_AND_FLAGS_BYTES: usize = 8;

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
    /// Флаги кадра по умолчанию из `trex`, если они записаны.
    extends_default_sample_flags: Option<SampleFlags>,
    /// Тип дорожки: у не-видео каждый кадр — точка произвольного доступа.
    media: TrackMedia,
    /// Место дорожки в полосе (тип и порядковый номер среди дорожек того же типа).
    slot: TrackSlot,
}

/// Контекст времени init-сегмента fMP4: дорожки по `track_ID`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IsoBmffTimingContext {
    /// Дорожки init-сегмента.
    tracks: BTreeMap<u32, IsoTrackTiming>,
    /// Есть ли среди дорожек видео (по `hdlr`).
    content: LaneContent,
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
        let mut track_media = Vec::new();
        let mut slots = TrackSlotAssigner::default();
        let mut extends_defaults = Vec::new();
        for child in box_children(movie, TimingStructure::IsoMovie) {
            let child = child?;
            match &child.box_type {
                b"trak" => {
                    let (track_id, timescale) = parse_track(child.body)?;
                    let media = track_media_of(child.body);
                    track_media.push(media);
                    let timing = IsoTrackTiming {
                        timescale,
                        extends_default_sample_duration: None,
                        extends_default_sample_flags: None,
                        media,
                        slot: slots.next(media),
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
        for extends in extends_defaults {
            let track =
                tracks
                    .get_mut(&extends.track_id)
                    .ok_or(SegmentTimingError::UnknownTrack {
                        track: u64::from(extends.track_id),
                    })?;
            track.extends_default_sample_duration = Some(extends.default_sample_duration);
            track.extends_default_sample_flags = extends.default_sample_flags;
        }
        if tracks.is_empty() {
            return Err(SegmentTimingError::Missing {
                structure: TimingStructure::IsoTrack,
            });
        }
        Ok(Self {
            tracks,
            content: LaneContent::of_tracks(track_media),
        })
    }

    /// Есть ли в init видеодорожка.
    pub(super) const fn content(&self) -> LaneContent {
        self.content
    }

    /// Отрезок времени показа медиасегмента `moof` + `mdat`, отрезки его дорожек,
    /// время декодирования (`tfdt` + длительности кадров) и точки произвольного доступа.
    pub(super) fn segment_spans(
        &self,
        segment: &[u8],
    ) -> Result<IsoSegmentSpans, SegmentTimingError> {
        let movie_fragment = first_child(segment, *b"moof", TimingStructure::IsoMediaSegment)?
            .ok_or(SegmentTimingError::Missing {
                structure: TimingStructure::IsoMovieFragment,
            })?;

        let mut segment_span: Option<MediaTimeSpan> = None;
        let mut track_spans = TrackSpans::default();
        // Время декодирования — отдельно от показа: у B-кадров они расходятся.
        let mut decode_span: Option<MediaTimeSpan> = None;
        let mut decode_tracks = TrackSpans::default();
        // Точки доступа: `None` — кадров ещё не было.
        let mut random_access: Option<SegmentRandomAccess> = None;
        // Если ни в одной дорожке нет кадров — пустой отрезок в самом раннем `tfdt`.
        let mut earliest_empty_start: Option<MediaTime> = None;
        for child in box_children(movie_fragment, TimingStructure::IsoMovieFragment) {
            let child = child?;
            if &child.box_type != b"traf" {
                continue;
            }
            let (slot, fragment_span) = self.track_fragment_span(child.body)?;
            match fragment_span {
                TrackFragmentSpan::Samples {
                    presentation,
                    decode,
                    points,
                } => {
                    random_access =
                        Some(random_access.map_or(points, |known| known.merged_with(points)));
                    segment_span =
                        Some(segment_span.map_or(presentation, |known| known.union(presentation)));
                    track_spans.include(slot, presentation);
                    decode_span = Some(decode_span.map_or(decode, |known| known.union(decode)));
                    decode_tracks.include(slot, decode);
                }
                TrackFragmentSpan::NoSamples(start) => {
                    earliest_empty_start =
                        Some(earliest_empty_start.map_or(start, |known| known.min(start)));
                }
            }
        }
        let span = segment_span
            .or_else(|| earliest_empty_start.map(MediaTimeSpan::empty_at))
            .ok_or(SegmentTimingError::Missing {
                structure: TimingStructure::IsoTrackFragment,
            })?;
        // Без кадров декодировать нечего: такой фрагмент подаётся по времени показа.
        let decode_order = decode_span.map_or(DecodeOrder::FollowsPresentation, |span| {
            DecodeOrder::Recorded {
                span,
                tracks: decode_tracks,
            }
        });
        Ok(IsoSegmentSpans {
            span,
            tracks: track_spans,
            decode_order,
            random_access: random_access.unwrap_or(SegmentRandomAccess::Unrecorded),
        })
    }

    /// Отрезок одного `traf` и место его дорожки в полосе.
    fn track_fragment_span(
        &self,
        track_fragment: &[u8],
    ) -> Result<(TrackSlot, TrackFragmentSpan), SegmentTimingError> {
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
        let mut runs = RunWalker::new(
            i128::from(base_decode_time),
            default_sample_duration,
            RandomAccessScan::new(TrackAccess::of_track(
                track.media,
                header
                    .default_sample_flags
                    .or(track.extends_default_sample_flags),
            )),
        );
        for child in box_children(track_fragment, TimingStructure::IsoTrackFragment) {
            let child = child?;
            if &child.box_type == b"trun" {
                runs.walk_run(child.body)?;
            }
        }

        let timescale = u64::from(track.timescale);
        let fragment_span = match runs.presentation_bounds() {
            Some((earliest, latest_end)) => {
                let start = MediaTime::from_ticks(earliest, timescale)?;
                let end = MediaTime::from_ticks(latest_end, timescale)?;
                let presentation =
                    MediaTimeSpan::new(start, end).ok_or(SegmentTimingError::TimeOutOfRange)?;
                // Кадры декодируются подряд от `tfdt` до конца последнего из них.
                let decode = MediaTimeSpan::new(
                    MediaTime::from_ticks(i128::from(base_decode_time), timescale)?,
                    MediaTime::from_ticks(runs.decode_end(), timescale)?,
                )
                .ok_or(SegmentTimingError::TimeOutOfRange)?;
                TrackFragmentSpan::Samples {
                    presentation,
                    decode,
                    points: runs
                        .random_access()
                        .finish(|ticks| MediaTime::from_ticks(ticks, timescale))?,
                }
            }
            None => MediaTime::from_ticks(i128::from(base_decode_time), timescale)
                .map(TrackFragmentSpan::NoSamples)?,
        };
        Ok((track.slot, fragment_span))
    }
}

/// Что сказал о себе один медиасегмент fMP4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IsoSegmentSpans {
    /// Время показа всех дорожек.
    pub(super) span: MediaTimeSpan,
    /// Время показа каждой дорожки.
    pub(super) tracks: TrackSpans,
    /// Время декодирования.
    pub(super) decode_order: DecodeOrder,
    /// Точки произвольного доступа.
    pub(super) random_access: SegmentRandomAccess,
}

/// Итог одного `traf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackFragmentSpan {
    /// Кадры есть: отрезки их времени показа и декодирования.
    Samples {
        /// От самого раннего начала показа до самого позднего конца.
        presentation: MediaTimeSpan,
        /// От `tfdt` до конца декодирования последнего кадра.
        decode: MediaTimeSpan,
        /// Точки произвольного доступа дорожки.
        points: SegmentRandomAccess,
    },
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
    /// Свои флаги кадра по умолчанию, если указаны.
    default_sample_flags: Option<SampleFlags>,
}

impl TrackFragmentHeader {
    /// Читает `tfhd`.
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
        if header.has_flag(TFHD_DEFAULT_SAMPLE_SIZE) {
            cursor.skip(4)?; // default_sample_size: u32
        }
        let default_sample_flags = if header.has_flag(TFHD_DEFAULT_SAMPLE_FLAGS) {
            Some(SampleFlags::new(cursor.read_u32()?))
        } else {
            None
        };
        Ok(Self {
            track_id,
            default_sample_duration,
            default_sample_flags,
        })
    }
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

/// Тип дорожки по `trak/mdia/hdlr`: за заголовком полного бокса (4 байта) и
/// `pre_defined` (4 байта) идёт `handler_type`. Нет бокса или он обрезан —
/// `Unknown`: время дорожки этим не портится, а полоса считается тяжёлой.
fn track_media_of(track: &[u8]) -> TrackMedia {
    let handler_type = unique_child(
        track,
        *b"mdia",
        TimingStructure::IsoTrack,
        TimingStructure::IsoTrack,
    )
    .ok()
    .flatten()
    .and_then(|media| {
        unique_child(
            media,
            *b"hdlr",
            TimingStructure::IsoTrack,
            TimingStructure::IsoTrack,
        )
        .ok()
        .flatten()
    })
    .and_then(|handler| handler.get(8..12));
    match handler_type {
        Some(b"vide") => TrackMedia::Video,
        Some(_) => TrackMedia::NotVideo,
        None => TrackMedia::Unknown,
    }
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

/// Значения по умолчанию одного `trex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackExtendsDefaults {
    /// Номер дорожки.
    track_id: u32,
    /// Длительность кадра.
    default_sample_duration: u32,
    /// Флаги кадра; `None` — бокс обрезан раньше них.
    default_sample_flags: Option<SampleFlags>,
}

/// `mvex`: значения по умолчанию из всех `trex`.
fn collect_track_extends(
    movie_extends: &[u8],
    extends_defaults: &mut Vec<TrackExtendsDefaults>,
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
        // Времени флаги не касаются: `trex`, обрезанный раньше них, по-прежнему годится
        // для времени, просто точки доступа сегментов остаются неизвестными.
        let default_sample_flags = (cursor.remaining_len() >= TREX_SIZE_AND_FLAGS_BYTES)
            .then(|| {
                cursor.skip(4)?; // default_sample_size: u32
                cursor.read_u32().map(SampleFlags::new)
            })
            .transpose()?;
        if extends_defaults
            .iter()
            .any(|known| known.track_id == track_id)
        {
            return Err(SegmentTimingError::Duplicate {
                structure: TimingStructure::IsoTrackExtends,
            });
        }
        extends_defaults.push(TrackExtendsDefaults {
            track_id,
            default_sample_duration,
            default_sample_flags,
        });
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
