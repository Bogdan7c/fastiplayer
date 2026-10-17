//! Разбор заголовков syncframe-ов AC-3 (Dolby Digital) и E-AC-3 (Dolby Digital Plus).
//!
//! Crate не декодирует звук и не зависит ни от декодера, ни от контейнера. Он
//! отвечает только на вопросы, которые нужны demuxer-ам до декодера:
//!
//! - где в байтах начинается syncframe (синхрослово `0x0B77`) и сколько он занимает;
//! - какой это кодек (AC-3 или E-AC-3 — по `bsid`), частота, число сэмплов, каналы;
//! - тип и номер substream-а E-AC-3;
//! - как собрать из подряд идущих syncframe-ов один пакет для декодера
//!   («access unit»: независимый substream 0 + его зависимые substream-ы — та же
//!   форма, что у пакетов Matroska/MP4).
//!
//! Спецификация: ATSC A/52:2018 (раздел 5.3 — syncinfo/BSI AC-3, Annex E — E-AC-3).
//!
//! API рассчитан на потоковых потребителей: MPEG-TS (кадры режутся границами PES)
//! и будущий demuxer «сырых» `.ac3`/`.eac3` файлов (мусор перед первым кадром,
//! неполный буфер в конце чтения). Поэтому неполные данные — это отдельный
//! результат «нужно больше байт», а не ошибка.

#![forbid(unsafe_code)]

mod access_unit;
mod bit_reader;
mod channels;
mod error;
mod header;
mod search;
mod tables;

pub use access_unit::{
    AccessUnit, AccessUnitScan, MAX_SYNCFRAMES_PER_ACCESS_UNIT, StreamContinuation,
    scan_access_unit,
};
pub use channels::ChannelLocations;
pub use error::SyncframeHeaderError;
pub use header::{
    AudioCodingMode, DependentChannelMap, Eac3StreamType, Eac3Substream, LowFrequencyEffects,
    SubstreamId, SyncframeCodec, SyncframeHeader, SyncframeLayout, parse_syncframe_header,
};
pub use search::{SYNC_WORD, SyncframeSearch, locate_syncframe};
