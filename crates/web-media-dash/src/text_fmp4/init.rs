//! Разбор `moov`: таймскейл, тип записи `stsd` и умолчания `trex` текстовой дорожки.

use std::num::NonZeroU32;

use super::TextFmp4Error;
use super::boxes::{BoxIter, Reader, find_child};

/// Формат сэмплов текстовой дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextSampleFormat {
    /// `stpp`: сэмпл - документ TTML (XMLSubtitleSampleEntry).
    Stpp,
    /// `wvtt`: сэмпл - набор боксов `vttc`/`vtte` (WebVTTSampleEntry).
    Wvtt,
}

/// Инициализация текстовой дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextTrackInit {
    /// Идентификатор дорожки (`tkhd`), по нему отбираются `traf`.
    pub(crate) track_id: u32,
    /// Единиц времени в секунде (`mdhd`).
    pub(crate) timescale: NonZeroU32,
    /// Формат сэмплов.
    pub(crate) format: TextSampleFormat,
    /// Длительность сэмпла по умолчанию (`trex`), если задана.
    pub(crate) default_sample_duration: Option<u32>,
    /// Размер сэмпла по умолчанию (`trex`), если задан (ненулевой).
    pub(crate) default_sample_size: Option<u32>,
}

/// Умолчания одного `trex`.
struct TrexDefaults {
    track_id: u32,
    duration: Option<u32>,
    size: Option<u32>,
}

/// Разбирает содержимое `moov` и возвращает первую дорожку `stpp`/`wvtt`.
pub(super) fn parse_moov(moov_payload: &[u8]) -> Result<TextTrackInit, TextFmp4Error> {
    let trex_defaults = collect_trex_defaults(moov_payload)?;
    for child in BoxIter::new(moov_payload, 0) {
        let child = child?;
        if &child.kind != b"trak" {
            continue;
        }
        if let Some((track_id, timescale, format)) = parse_trak(child.payload)? {
            let trex = trex_defaults.iter().find(|trex| trex.track_id == track_id);
            return Ok(TextTrackInit {
                track_id,
                timescale,
                format,
                default_sample_duration: trex.and_then(|trex| trex.duration),
                default_sample_size: trex.and_then(|trex| trex.size),
            });
        }
    }
    Err(TextFmp4Error::NoTextTrack)
}

/// Умолчания каждого `trex` внутри `mvex`.
fn collect_trex_defaults(moov_payload: &[u8]) -> Result<Vec<TrexDefaults>, TextFmp4Error> {
    let mut defaults = Vec::new();
    let Some(mvex) = find_child(moov_payload, *b"mvex")? else {
        return Ok(defaults);
    };
    for child in BoxIter::new(mvex.payload, 0) {
        let child = child?;
        if &child.kind != b"trex" {
            continue;
        }
        let mut reader = Reader::new(child.payload);
        reader.full_box_header()?;
        let track_id = reader.u32()?;
        // default_sample_description_index пропускаем.
        reader.skip(4)?;
        let duration = reader.u32()?;
        let size = reader.u32()?;
        // Ноль в `trex` - «не задано» (значение обязано прийти из `tfhd`/`trun`).
        // Флаги по умолчанию не нужны: читатель не различает sync-сэмплы.
        defaults.push(TrexDefaults {
            track_id,
            duration: (duration != 0).then_some(duration),
            size: (size != 0).then_some(size),
        });
    }
    Ok(defaults)
}

/// Разбирает `trak`; `None`, если запись `stsd` не `stpp`/`wvtt`.
fn parse_trak(
    trak_payload: &[u8],
) -> Result<Option<(u32, NonZeroU32, TextSampleFormat)>, TextFmp4Error> {
    let Some(tkhd) = find_child(trak_payload, *b"tkhd")? else {
        return Ok(None);
    };
    let Some(mdia) = find_child(trak_payload, *b"mdia")? else {
        return Ok(None);
    };
    let Some(mdhd) = find_child(mdia.payload, *b"mdhd")? else {
        return Ok(None);
    };
    let Some(minf) = find_child(mdia.payload, *b"minf")? else {
        return Ok(None);
    };
    let Some(stbl) = find_child(minf.payload, *b"stbl")? else {
        return Ok(None);
    };
    let Some(stsd) = find_child(stbl.payload, *b"stsd")? else {
        return Ok(None);
    };
    let Some(format) = first_sample_entry_format(stsd.payload)? else {
        return Ok(None);
    };
    let track_id = read_track_id(tkhd.payload)?;
    let timescale = read_timescale(mdhd.payload)?;
    Ok(Some((track_id, timescale, format)))
}

/// Формат первой записи `stsd`; `None` для не-текстовых записей.
fn first_sample_entry_format(
    stsd_payload: &[u8],
) -> Result<Option<TextSampleFormat>, TextFmp4Error> {
    let mut reader = Reader::new(stsd_payload);
    reader.full_box_header()?;
    let entry_count = reader.u32()?;
    if entry_count == 0 {
        return Ok(None);
    }
    // Записи - обычные боксы; нужен только тип первой.
    let entries = reader.take(reader.remaining())?;
    let Some(entry) = BoxIter::new(entries, 0).next() else {
        return Ok(None);
    };
    Ok(match &entry?.kind {
        b"stpp" => Some(TextSampleFormat::Stpp),
        b"wvtt" => Some(TextSampleFormat::Wvtt),
        _ => None,
    })
}

/// `track_ID` из `tkhd` (версия 0: 32-битные времена, версия 1: 64-битные).
fn read_track_id(tkhd_payload: &[u8]) -> Result<u32, TextFmp4Error> {
    let mut reader = Reader::new(tkhd_payload);
    let (version, _) = reader.full_box_header()?;
    // creation_time + modification_time.
    reader.skip(if version == 1 { 16 } else { 8 })?;
    reader.u32()
}

/// Таймскейл из `mdhd`.
fn read_timescale(mdhd_payload: &[u8]) -> Result<NonZeroU32, TextFmp4Error> {
    let mut reader = Reader::new(mdhd_payload);
    let (version, _) = reader.full_box_header()?;
    reader.skip(if version == 1 { 16 } else { 8 })?;
    NonZeroU32::new(reader.u32()?).ok_or(TextFmp4Error::ZeroTimescale)
}
