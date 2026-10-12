//! Packed audio HLS (RFC 8216 §3.4): сегменты `.aac` (ADTS) и `.mp3` без контейнера.
//!
//! # Архитектура
//!
//! Packed-сегмент - это голый elementary stream: перед ним (и иногда между кадрами) стоит
//! ID3v2-тег, а единственная временная метка лежит в `PRIV`-кадре
//! `com.apple.streaming.transportStreamTimestamp`. Собственных timestamp-ов у ADTS/MP3 нет.
//!
//! Весь остальной HLS-стек (probe контейнера, VOD seek-index, live-якоря, discontinuity,
//! ребейз таймстемпов по origin эпохи) уже умеет работать с MPEG-TS и доказан на нём. Поэтому
//! packed audio **нормализуется в одно-программный MPEG-TS на границе источника сегментов**
//! (`HlsEpochSegmentSource`): после скачивания и расшифровки байты сегмента оборачиваются в
//! PAT + PMT + один PES с PTS, и дальше демуксер видит обычный audio-only TS. Контейнер
//! определяется по содержимому (`ID3` либо sync-слово кадра), а не по расширению URI.
//!
//! Владение: этот модуль знает только форму packed-сегмента и TS-обёртку. Он не знает про
//! сеть, ключи, seek и live - это остаётся у источника и демуксеров.
//!
//! # Откуда берётся PTS первого кадра
//!
//! 1. ID3 `PRIV` `transportStreamTimestamp` (33-битный счётчик 90 кГц) - авторитетный источник.
//! 2. Если тега нет - позиция сегмента на таймлайне манифеста (сумма `EXTINF`), которую
//!    передаёт вызывающий как [`SegmentTimestampFallback`].
//!
//! PTS следующих кадров внутри сегмента накапливает MPEG-TS демуксер по числу сэмплов кадра.
//! Между сегментами непрерывность обеспечивает следующий `PRIV` либо `EXTINF`; границы эпох
//! (`EXT-X-DISCONTINUITY`) демуксер и без того ребейзит по origin эпохи.
//!
//! # Границы поддержки
//!
//! Поддержаны AAC в ADTS и MPEG audio (MP1/MP2/MP3). AC-3/E-AC-3 packed честно отвергаются
//! типизированной ошибкой [`PackedAudioError::UnsupportedCodec`].

mod id3;
mod transport_stream;

use std::time::Duration;

use bytes::Bytes;

pub(crate) use self::id3::PackedAudioTimestamp;
use self::transport_stream::{PackedAudioCodec, wrap_in_transport_stream};

/// Сколько первых байт сегмента хватает, чтобы отличить packed audio от TS и fMP4.
pub(crate) const PACKED_AUDIO_SNIFF_PREFIX_BYTES: usize = 3;

/// Тактов MPEG-TS часов (90 кГц) в секунде.
const MPEG_TS_TICKS_PER_SECOND: u64 = 90_000;

/// Маска 33-битного PES/PRIV timestamp-а.
const TIMESTAMP_33_BIT_MASK: u64 = (1_u64 << 33) - 1;

/// Почему packed-сегмент не удалось превратить в audio-only TS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PackedAudioError {
    /// ID3-тег объявляет размер больше, чем есть байт в сегменте.
    #[error("packed audio: ID3 tag cropped")]
    TruncatedId3,
    /// После ID3-тегов нет sync-слова ADTS/MPEG audio.
    #[error("packed audio: no sync word after ID3")]
    MissingAudioSync,
    /// Распознан кадр AC-3/E-AC-3, который профиль не поддерживает.
    #[error("packed audio: AC-3/E-AC-3 not supported")]
    UnsupportedCodec,
}

/// Откуда брать PTS, если в сегменте нет ID3 `PRIV`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentTimestampFallback {
    /// Начало сегмента на таймлайне манифеста (сумма предыдущих `EXTINF`).
    ManifestTimelineStart(Duration),
    /// Манифестной позиции нет: первый кадр получает PTS 0.
    Zero,
}

impl SegmentTimestampFallback {
    fn ticks(self) -> u64 {
        match self {
            Self::ManifestTimelineStart(start) => start
                .as_secs()
                .saturating_mul(MPEG_TS_TICKS_PER_SECOND)
                .saturating_add(
                    u64::from(start.subsec_nanos()) * MPEG_TS_TICKS_PER_SECOND / 1_000_000_000,
                ),
            Self::Zero => 0,
        }
    }
}

/// Похоже ли начало тела сегмента на packed audio.
///
/// TS начинается с `0x47`, fMP4 - с размера box-а (`00 00 ..`), поэтому ложных срабатываний
/// на уже поддерживаемых контейнерах нет. Короткий префикс (меньше трёх байт) packed не считается.
#[must_use]
pub(crate) fn looks_like_packed_audio(prefix: &[u8]) -> bool {
    match prefix {
        [b'I', b'D', b'3', ..] => true,
        // ADTS/MPEG audio: 12 бит sync; AC-3: 0x0B77 (распознаём, чтобы отвергнуть честно).
        [0xff, second, _, ..] => second & 0xe0 == 0xe0,
        [0x0b, 0x77, _, ..] => true,
        _ => false,
    }
}

/// Оборачивает целый packed-сегмент в audio-only MPEG-TS.
///
/// # Errors
/// Возвращает [`PackedAudioError`], если ID3-тег обрезан, за ним нет аудио-кадра либо кодек
/// (AC-3/E-AC-3) не поддержан.
pub(crate) fn normalize_to_transport_stream(
    segment: &[u8],
    fallback: SegmentTimestampFallback,
) -> Result<Bytes, PackedAudioError> {
    let prefix = id3::parse_id3_prefix(segment)?;
    let elementary = segment
        .get(prefix.length..)
        .ok_or(PackedAudioError::TruncatedId3)?;
    let codec = PackedAudioCodec::detect(elementary)?;
    let first_pts = prefix
        .transport_stream_timestamp
        .map_or_else(|| fallback.ticks(), PackedAudioTimestamp::ticks)
        & TIMESTAMP_33_BIT_MASK;
    Ok(Bytes::from(wrap_in_transport_stream(
        elementary, codec, first_pts,
    )))
}

#[cfg(test)]
mod tests;
