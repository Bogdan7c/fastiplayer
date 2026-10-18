//! Разбор фрагментов `moof`/`traf` (`tfhd`, `tfdt`, `trun`) и выборка сэмплов из `mdat`.

use super::boxes::{BoxIter, Mp4Box, Reader};
use super::init::{TextTrackInit, parse_moov};
use super::{MAX_SAMPLES_PER_SEGMENT, TextFmp4Error, TextSample};

/// Содержимое одного ресурса: инициализация (если в нём был `moov`) и сэмплы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextMp4Content {
    /// Инициализация, найденная в самом ресурсе (`moov`); `None` у обычного медиасегмента.
    pub(crate) init: Option<TextTrackInit>,
    /// Сэмплы всех подходящих `traf` в порядке следования в ресурсе.
    pub(crate) samples: Vec<TextSample>,
}

/// Флаги `tfhd`.
const TFHD_BASE_DATA_OFFSET: u32 = 0x0000_0001;
const TFHD_SAMPLE_DESCRIPTION_INDEX: u32 = 0x0000_0002;
const TFHD_DEFAULT_SAMPLE_DURATION: u32 = 0x0000_0008;
const TFHD_DEFAULT_SAMPLE_SIZE: u32 = 0x0000_0010;
const TFHD_DEFAULT_SAMPLE_FLAGS: u32 = 0x0000_0020;
/// Флаги `trun`.
const TRUN_DATA_OFFSET: u32 = 0x0000_0001;
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x0000_0004;
const TRUN_SAMPLE_DURATION: u32 = 0x0000_0100;
const TRUN_SAMPLE_SIZE: u32 = 0x0000_0200;
const TRUN_SAMPLE_FLAGS: u32 = 0x0000_0400;
const TRUN_SAMPLE_COMPOSITION_OFFSET: u32 = 0x0000_0800;

/// Читает ресурс целиком: init из `moov` (или `known_init`) и сэмплы всех фрагментов.
///
/// `known_init` - инициализация, скачанная отдельно (SegmentTemplate `initialization`).
/// Если ресурс содержит собственный `moov`, он имеет приоритет. Ресурс без `moof` - это
/// `UnsupportedLayout` (либо чистый init-сегмент, когда `moov` есть, а сэмплов нет).
pub(crate) fn parse_text_mp4(
    bytes: &[u8],
    known_init: Option<&TextTrackInit>,
) -> Result<TextMp4Content, TextFmp4Error> {
    let mut init = known_init.copied();
    let mut found_own_init = false;
    let mut samples = Vec::new();
    let mut layout = SampleLayout::default();
    let mut saw_fragment = false;
    for top_level in BoxIter::new(bytes, 0) {
        let top_level = top_level?;
        match &top_level.kind {
            b"moov" => {
                init = Some(parse_moov(top_level.payload)?);
                found_own_init = true;
            }
            b"moof" => {
                saw_fragment = true;
                let track = init.as_ref().ok_or(TextFmp4Error::MissingInit)?;
                read_fragment(bytes, &top_level, track, &mut layout, &mut samples)?;
            }
            _ => {}
        }
    }
    if !saw_fragment && !found_own_init {
        return Err(TextFmp4Error::UnsupportedLayout);
    }
    Ok(TextMp4Content {
        init: found_own_init.then_some(init).flatten(),
        samples,
    })
}

/// Учёт раскладки байтов сэмплов в пределах одного ресурса.
///
/// Инвариант (защита от усиления памяти): диапазоны сэмплов идут строго вперёд и не
/// пересекаются, поэтому суммарно скопированные байты никогда не превышают длину ресурса.
#[derive(Debug, Default)]
struct SampleLayout {
    /// Первая позиция, с которой может начинаться следующий сэмпл.
    next_allowed_position: u64,
    /// Сумма размеров уже принятых сэмплов.
    total_sample_bytes: u64,
}

/// Параметры `tfhd`, нужные для раскладки сэмплов.
struct TrackFragmentHeader {
    track_id: u32,
    base_data_offset: Option<u64>,
    default_sample_duration: Option<u32>,
    default_sample_size: Option<u32>,
}

/// Читает один `moof` и дописывает сэмплы нужной дорожки.
fn read_fragment(
    bytes: &[u8],
    moof: &Mp4Box<'_>,
    track: &TextTrackInit,
    layout: &mut SampleLayout,
    samples: &mut Vec<TextSample>,
) -> Result<(), TextFmp4Error> {
    for child in BoxIter::new(moof.payload, moof.payload_start) {
        let child = child?;
        if &child.kind == b"traf" {
            read_track_fragment(bytes, moof, &child, track, layout, samples)?;
        }
    }
    Ok(())
}

/// Читает `traf`: `tfhd`, `tfdt`, затем каждый `trun`.
fn read_track_fragment(
    bytes: &[u8],
    moof: &Mp4Box<'_>,
    traf: &Mp4Box<'_>,
    track: &TextTrackInit,
    layout: &mut SampleLayout,
    samples: &mut Vec<TextSample>,
) -> Result<(), TextFmp4Error> {
    let mut header = None;
    let mut base_decode_time = None;
    let mut cursor_decode_time: Option<u64> = None;
    let mut next_data_offset: Option<u64> = None;
    for child in BoxIter::new(traf.payload, traf.payload_start) {
        let child = child?;
        match &child.kind {
            b"tfhd" => header = Some(read_tfhd(child.payload)?),
            b"tfdt" => base_decode_time = Some(read_tfdt(child.payload)?),
            b"trun" => {
                let header = header.as_ref().ok_or(TextFmp4Error::InvalidBox)?;
                if header.track_id != track.track_id {
                    // Чужая дорожка (в одном moof могут быть и другие traf).
                    return Ok(());
                }
                let start_time = cursor_decode_time.or(base_decode_time).unwrap_or(0);
                let run = read_trun(child.payload)?;
                let produced = materialize_run(
                    bytes,
                    moof.start,
                    header,
                    track,
                    &run,
                    start_time,
                    &mut next_data_offset,
                    layout,
                    samples,
                )?;
                cursor_decode_time = Some(produced);
            }
            _ => {}
        }
    }
    Ok(())
}

fn read_tfhd(payload: &[u8]) -> Result<TrackFragmentHeader, TextFmp4Error> {
    let mut reader = Reader::new(payload);
    let (_, flags) = reader.full_box_header()?;
    let track_id = reader.u32()?;
    let base_data_offset = if flags & TFHD_BASE_DATA_OFFSET != 0 {
        Some(reader.u64()?)
    } else {
        None
    };
    if flags & TFHD_SAMPLE_DESCRIPTION_INDEX != 0 {
        reader.skip(4)?;
    }
    let default_sample_duration = if flags & TFHD_DEFAULT_SAMPLE_DURATION != 0 {
        Some(reader.u32()?)
    } else {
        None
    };
    let default_sample_size = if flags & TFHD_DEFAULT_SAMPLE_SIZE != 0 {
        Some(reader.u32()?)
    } else {
        None
    };
    if flags & TFHD_DEFAULT_SAMPLE_FLAGS != 0 {
        reader.skip(4)?;
    }
    Ok(TrackFragmentHeader {
        track_id,
        base_data_offset,
        default_sample_duration,
        default_sample_size,
    })
}

fn read_tfdt(payload: &[u8]) -> Result<u64, TextFmp4Error> {
    let mut reader = Reader::new(payload);
    let (version, _) = reader.full_box_header()?;
    if version == 1 {
        reader.u64()
    } else {
        reader.u32().map(u64::from)
    }
}

/// Один `trun` с уже прочитанными полями сэмплов.
struct TrackRun {
    data_offset: Option<i32>,
    samples: Vec<RunSample>,
}

/// Поля одного сэмпла `trun` (отсутствующие - `None`).
struct RunSample {
    duration: Option<u32>,
    size: Option<u32>,
}

fn read_trun(payload: &[u8]) -> Result<TrackRun, TextFmp4Error> {
    let mut reader = Reader::new(payload);
    let (_, flags) = reader.full_box_header()?;
    let sample_count = usize::try_from(reader.u32()?).map_err(|_| TextFmp4Error::Overflow)?;
    if sample_count > MAX_SAMPLES_PER_SEGMENT {
        return Err(TextFmp4Error::TooManySamples);
    }
    let data_offset = if flags & TRUN_DATA_OFFSET != 0 {
        Some(reader.i32()?)
    } else {
        None
    };
    if flags & TRUN_FIRST_SAMPLE_FLAGS != 0 {
        reader.skip(4)?;
    }
    let mut samples = Vec::with_capacity(sample_count.min(1024));
    for _ in 0..sample_count {
        let duration = if flags & TRUN_SAMPLE_DURATION != 0 {
            Some(reader.u32()?)
        } else {
            None
        };
        let size = if flags & TRUN_SAMPLE_SIZE != 0 {
            Some(reader.u32()?)
        } else {
            None
        };
        if flags & TRUN_SAMPLE_FLAGS != 0 {
            reader.skip(4)?;
        }
        if flags & TRUN_SAMPLE_COMPOSITION_OFFSET != 0 {
            reader.skip(4)?;
        }
        samples.push(RunSample { duration, size });
    }
    Ok(TrackRun {
        data_offset,
        samples,
    })
}

/// Раскладывает сэмплы одного `trun`; возвращает время декодирования после последнего.
#[allow(clippy::too_many_arguments)]
fn materialize_run(
    bytes: &[u8],
    moof_start: usize,
    header: &TrackFragmentHeader,
    track: &TextTrackInit,
    run: &TrackRun,
    start_decode_time: u64,
    next_data_offset: &mut Option<u64>,
    layout: &mut SampleLayout,
    samples: &mut Vec<TextSample>,
) -> Result<u64, TextFmp4Error> {
    if samples.len().saturating_add(run.samples.len()) > MAX_SAMPLES_PER_SEGMENT {
        return Err(TextFmp4Error::TooManySamples);
    }
    // Базовый адрес: явный `base_data_offset`, иначе начало `moof` (default-base-is-moof и
    // единственный практически встречающийся вариант для DASH).
    let base = match header.base_data_offset {
        Some(explicit) => explicit,
        None => u64::try_from(moof_start).map_err(|_| TextFmp4Error::Overflow)?,
    };
    let mut data_position = match (run.data_offset, *next_data_offset) {
        (Some(offset), _) => base
            .checked_add_signed(i64::from(offset))
            .ok_or(TextFmp4Error::Overflow)?,
        (None, Some(continued)) => continued,
        (None, None) => return Err(TextFmp4Error::MissingDataOffset),
    };
    let mut decode_time = start_decode_time;
    for sample in &run.samples {
        let duration = sample
            .duration
            .or(header.default_sample_duration)
            .or(track.default_sample_duration)
            .ok_or(TextFmp4Error::MissingSampleDuration)?;
        let size = sample
            .size
            .or(header.default_sample_size)
            .or(track.default_sample_size)
            .ok_or(TextFmp4Error::MissingSampleSize)?;
        let size = u64::from(size);
        let end = data_position
            .checked_add(size)
            .ok_or(TextFmp4Error::Overflow)?;
        let start = usize::try_from(data_position).map_err(|_| TextFmp4Error::Overflow)?;
        let end_index = usize::try_from(end).map_err(|_| TextFmp4Error::Overflow)?;
        // Сэмплы не должны пересекаться и идти назад: иначе много `trun` на один диапазон
        // раздули бы копии `to_vec()` во много раз больше самого сегмента.
        if data_position < layout.next_allowed_position {
            return Err(TextFmp4Error::OverlappingSamples);
        }
        layout.total_sample_bytes = layout
            .total_sample_bytes
            .checked_add(size)
            .ok_or(TextFmp4Error::Overflow)?;
        let segment_length = u64::try_from(bytes.len()).map_err(|_| TextFmp4Error::Overflow)?;
        if layout.total_sample_bytes > segment_length {
            return Err(TextFmp4Error::SamplesExceedSegment);
        }
        layout.next_allowed_position = end;
        let data = bytes
            .get(start..end_index)
            .ok_or(TextFmp4Error::SampleOutOfBounds)?;
        samples.push(TextSample {
            decode_time,
            duration: u64::from(duration),
            data: data.to_vec(),
        });
        decode_time = decode_time
            .checked_add(u64::from(duration))
            .ok_or(TextFmp4Error::Overflow)?;
        data_position = end;
    }
    *next_data_offset = Some(data_position);
    Ok(decode_time)
}
