//! Разбор ID3v2-тегов перед packed-сегментом (RFC 8216 §3.4 + ID3v2.3/2.4).

use super::PackedAudioError;

/// Владелец `PRIV`-кадра с 33-битной MPEG-TS временной меткой первого кадра сегмента.
const TRANSPORT_STREAM_TIMESTAMP_OWNER: &[u8] = b"com.apple.streaming.transportStreamTimestamp";
/// Размер заголовка ID3v2-тега.
const ID3_HEADER_BYTES: usize = 10;
/// Размер заголовка кадра ID3v2.3/2.4.
const ID3_FRAME_HEADER_BYTES: usize = 10;
/// Размер необязательного footer-а ID3v2.4.
const ID3_FOOTER_BYTES: usize = 10;
/// Флаг unsynchronisation в заголовке тега.
const FLAG_UNSYNCHRONISATION: u8 = 0x80;
/// Флаг extended header в заголовке тега.
const FLAG_EXTENDED_HEADER: u8 = 0x40;
/// Флаг footer-а (только ID3v2.4).
const FLAG_FOOTER: u8 = 0x10;

/// 33-битная метка 90 кГц из `PRIV`; отдельный тип не даёт перепутать такты с секундами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PackedAudioTimestamp(u64);

impl PackedAudioTimestamp {
    /// Значение в тактах 90 кГц.
    #[must_use]
    pub(crate) const fn ticks(self) -> u64 {
        self.0
    }
}

/// Итог разбора всех подряд идущих ID3-тегов в начале сегмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Id3Prefix {
    /// Сколько байт занимают теги; аудио-кадры начинаются сразу за ними.
    pub(crate) length: usize,
    /// Первая найденная `transportStreamTimestamp`-метка.
    pub(crate) transport_stream_timestamp: Option<PackedAudioTimestamp>,
}

/// Пропускает ID3-теги в начале сегмента и достаёт timestamp, если он есть.
///
/// Сегмент без ID3 - не ошибка: длина префикса 0, метки нет (PTS возьмётся из `EXTINF`).
pub(super) fn parse_id3_prefix(segment: &[u8]) -> Result<Id3Prefix, PackedAudioError> {
    let mut cursor = 0_usize;
    let mut timestamp = None;
    while let Some(rest) = segment
        .get(cursor..)
        .filter(|rest| rest.starts_with(b"ID3"))
    {
        let header = rest
            .get(..ID3_HEADER_BYTES)
            .ok_or(PackedAudioError::TruncatedId3)?;
        let major_version = header[3];
        let flags = header[5];
        let body_length = synchsafe(&header[6..10]);
        let footer = if major_version >= 4 && flags & FLAG_FOOTER != 0 {
            ID3_FOOTER_BYTES
        } else {
            0
        };
        let total = ID3_HEADER_BYTES
            .checked_add(body_length)
            .and_then(|length| length.checked_add(footer))
            .ok_or(PackedAudioError::TruncatedId3)?;
        let body = rest
            .get(ID3_HEADER_BYTES..ID3_HEADER_BYTES + body_length)
            .ok_or(PackedAudioError::TruncatedId3)?;
        if timestamp.is_none() {
            timestamp = find_timestamp(body, major_version, flags);
        }
        cursor += total;
        if cursor > segment.len() {
            return Err(PackedAudioError::TruncatedId3);
        }
    }
    Ok(Id3Prefix {
        length: cursor,
        transport_stream_timestamp: timestamp,
    })
}

/// Ищет нужный `PRIV` среди кадров тела тега. Некорректные кадры не считаются фатальными:
/// без метки сегмент всё равно проигрывается по `EXTINF`.
fn find_timestamp(body: &[u8], major_version: u8, flags: u8) -> Option<PackedAudioTimestamp> {
    // Кадры существуют только в ID3v2.3/2.4 (в v2.2 идентификаторы трёхбайтные, PRIV нет).
    if !(3..=4).contains(&major_version) {
        return None;
    }
    let mut cursor = 0_usize;
    if flags & FLAG_EXTENDED_HEADER != 0 {
        let size_field = body.get(..4)?;
        // v2.4: размер synchsafe и включает само поле; v2.3: обычный и не включает его.
        cursor = if major_version >= 4 {
            synchsafe(size_field)
        } else {
            u32::from_be_bytes(size_field.try_into().ok()?) as usize + 4
        };
    }
    while let Some(frame_header) = body.get(cursor..cursor.checked_add(ID3_FRAME_HEADER_BYTES)?) {
        // Нулевой первый байт - начало padding-а, кадров больше нет.
        if frame_header[0] == 0 {
            return None;
        }
        let frame_size_field = &frame_header[4..8];
        let frame_size = if major_version >= 4 {
            synchsafe(frame_size_field)
        } else {
            u32::from_be_bytes(frame_size_field.try_into().ok()?) as usize
        };
        let payload_start = cursor + ID3_FRAME_HEADER_BYTES;
        let payload = body.get(payload_start..payload_start.checked_add(frame_size)?)?;
        if &frame_header[..4] == b"PRIV"
            && let Some(timestamp) =
                parse_priv_payload(payload, flags & FLAG_UNSYNCHRONISATION != 0)
        {
            return Some(timestamp);
        }
        cursor = payload_start + frame_size;
    }
    None
}

/// `PRIV`: `owner\0` + данные; для нашего владельца данные - 8 байт big-endian, значимы 33 бита.
fn parse_priv_payload(payload: &[u8], unsynchronised: bool) -> Option<PackedAudioTimestamp> {
    let owner_end = payload.iter().position(|byte| *byte == 0)?;
    if &payload[..owner_end] != TRANSPORT_STREAM_TIMESTAMP_OWNER {
        return None;
    }
    let data = payload.get(owner_end + 1..)?;
    let decoded;
    let data = if unsynchronised {
        decoded = remove_unsynchronisation(data);
        decoded.as_slice()
    } else {
        data
    };
    let raw: [u8; 8] = data.get(..8)?.try_into().ok()?;
    Some(PackedAudioTimestamp(
        u64::from_be_bytes(raw) & ((1_u64 << 33) - 1),
    ))
}

/// Убирает вставленный при unsynchronisation нулевой байт после каждого `0xFF`.
fn remove_unsynchronisation(data: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(data.len());
    let mut previous_was_ff = false;
    for &byte in data {
        if previous_was_ff && byte == 0 {
            previous_was_ff = false;
            continue;
        }
        previous_was_ff = byte == 0xff;
        decoded.push(byte);
    }
    decoded
}

/// ID3 synchsafe: по 7 значимых бит в каждом байте.
fn synchsafe(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0_usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7f))
}
