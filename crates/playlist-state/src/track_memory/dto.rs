//! Дисковый формат `track-memory.json` (schema v1) и его строгое преобразование.
//!
//! Формат приватный для модуля. Пример:
//! ```json
//! {
//!   "schema_version": 1,
//!   "entries": [
//!     {
//!       "video_fingerprint_sha256": "<64 hex>",
//!       "last_used": 3,
//!       "audio": { "language": "jpn", "title": null, "codec_id": "A_AC3", "ordinal_among_kind": 1 },
//!       "subtitles": { "kind": "embedded", "track": { ... } },
//!       "subtitle_sync_offset_ms": -250
//!     }
//!   ]
//! }
//! ```
//! `subtitles`: `null` (не выбирал) | `{"kind":"off"}` | `{"kind":"embedded","track":{..}}` |
//! `{"kind":"file","path":{"encoding":"utf8","value":".."}}`. Не-UTF-8 путь Unix лежит как
//! `{"encoding":"unix_bytes_hex","hex":".."}`, Windows — `wide_le_hex`.
//! Записи отсортированы от самой давней к самой свежей.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::StateSerializationError;

use super::hint::RememberedTrackHint;
use super::key::VideoKey;
use super::memory::{
    RecencyStamp, RememberedFilePath, RememberedSubtitleChoice, SubtitleSyncOffset, TrackMemory,
    TrackMemoryCapacity, TrackMemoryEntry,
};

/// Текущая версия схемы файла.
pub const CURRENT_TRACK_MEMORY_SCHEMA_VERSION: u64 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackMemoryFileV1 {
    schema_version: u64,
    entries: Vec<EntryV1>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryV1 {
    video_fingerprint_sha256: String,
    last_used: u64,
    /// Отсутствие поля читается как `None`: «не выбирал» и «поле не записано» совпадают.
    #[serde(default)]
    audio: Option<TrackHintV1>,
    #[serde(default)]
    subtitles: Option<SubtitleChoiceV1>,
    subtitle_sync_offset_ms: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackHintV1 {
    language: Option<String>,
    title: Option<String>,
    codec_id: String,
    ordinal_among_kind: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SubtitleChoiceV1 {
    Off,
    Embedded { track: TrackHintV1 },
    File { path: FilePathV1 },
}

/// Путь, обратимый и пригодный для JSON; байты и `u16` — hex-строкой (компактно и ограниченно).
#[derive(Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
enum FilePathV1 {
    Utf8 { value: String },
    UnixBytesHex { hex: String },
    WideLeHex { hex: String },
}

/// Почему payload отвергнут: структура не та или значения нарушают инварианты.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InvalidTrackMemoryPayload {
    /// Не разбирается как схема v1 (синтаксис, неизвестные поля, типы).
    Schema,
    /// Схема соблюдена, значения недопустимы (диапазон, длина, дубликаты, hex).
    DomainValue,
}

// --------------------------------------------------------------------------------------
// Память -> байты
// --------------------------------------------------------------------------------------

/// Сериализует память в детерминированный pretty JSON с завершающим переводом строки.
pub(super) fn serialize_track_memory(
    memory: &TrackMemory,
    maximum_bytes: u64,
) -> Result<Vec<u8>, StateSerializationError> {
    let entries = memory
        .entries_oldest_first()
        .into_iter()
        .map(|(key, entry)| entry_to_dto(key, entry))
        .collect::<Result<Vec<_>, _>>()?;
    let mut bytes = serde_json::to_vec_pretty(&TrackMemoryFileV1 {
        schema_version: CURRENT_TRACK_MEMORY_SCHEMA_VERSION,
        entries,
    })
    .map_err(|_| StateSerializationError::JsonEncodingFailed)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > maximum_bytes {
        return Err(StateSerializationError::SerializedStateTooLarge);
    }
    Ok(bytes)
}

fn entry_to_dto(
    key: VideoKey,
    entry: &TrackMemoryEntry,
) -> Result<EntryV1, StateSerializationError> {
    let subtitles = match entry.subtitles() {
        None => None,
        Some(choice) => Some(choice_to_dto(choice)?),
    };
    Ok(EntryV1 {
        video_fingerprint_sha256: key.to_hex(),
        last_used: entry.last_used().get(),
        audio: entry.audio().map(hint_to_dto),
        subtitles,
        subtitle_sync_offset_ms: entry.subtitle_sync_offset().millis(),
    })
}

fn hint_to_dto(hint: &RememberedTrackHint) -> TrackHintV1 {
    TrackHintV1 {
        language: hint.language().map(str::to_owned),
        title: hint.title().map(str::to_owned),
        codec_id: hint.codec_id().to_owned(),
        ordinal_among_kind: hint.ordinal_among_kind(),
    }
}

fn choice_to_dto(
    choice: &RememberedSubtitleChoice,
) -> Result<SubtitleChoiceV1, StateSerializationError> {
    Ok(match choice {
        RememberedSubtitleChoice::Off => SubtitleChoiceV1::Off,
        RememberedSubtitleChoice::Embedded(hint) => SubtitleChoiceV1::Embedded {
            track: hint_to_dto(hint),
        },
        RememberedSubtitleChoice::File(path) => SubtitleChoiceV1::File {
            path: path_to_dto(path)?,
        },
    })
}

fn path_to_dto(path: &RememberedFilePath) -> Result<FilePathV1, StateSerializationError> {
    if let Some(text) = path.as_path().to_str() {
        return Ok(FilePathV1::Utf8 {
            value: text.to_owned(),
        });
    }
    non_utf8_path_to_dto(path)
}

#[cfg(unix)]
fn non_utf8_path_to_dto(path: &RememberedFilePath) -> Result<FilePathV1, StateSerializationError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(FilePathV1::UnixBytesHex {
        hex: encode_hex(path.as_path().as_os_str().as_bytes()),
    })
}

#[cfg(windows)]
fn non_utf8_path_to_dto(path: &RememberedFilePath) -> Result<FilePathV1, StateSerializationError> {
    use std::os::windows::ffi::OsStrExt;
    let little_endian_bytes: Vec<u8> = path
        .as_path()
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    Ok(FilePathV1::WideLeHex {
        hex: encode_hex(&little_endian_bytes),
    })
}

#[cfg(not(any(unix, windows)))]
fn non_utf8_path_to_dto(_path: &RememberedFilePath) -> Result<FilePathV1, StateSerializationError> {
    Err(StateSerializationError::UnsupportedNativePathEncoding)
}

// --------------------------------------------------------------------------------------
// Байты -> память
// --------------------------------------------------------------------------------------

/// Разбирает и проверяет payload. Любое нарушение схемы или инварианта — одна общая ошибка:
/// файл целиком считается повреждённым.
pub(super) fn restore_track_memory(
    payload: &[u8],
    capacity: TrackMemoryCapacity,
) -> Result<TrackMemory, InvalidTrackMemoryPayload> {
    let file: TrackMemoryFileV1 =
        serde_json::from_slice(payload).map_err(|_| InvalidTrackMemoryPayload::Schema)?;
    if file.schema_version != CURRENT_TRACK_MEMORY_SCHEMA_VERSION {
        return Err(InvalidTrackMemoryPayload::Schema);
    }
    // Ранний отказ до аллокаций под записи: файл с «лишними» записями заведомо чужой.
    if file.entries.len() > super::memory::TRACK_MEMORY_HARD_MAX_ENTRIES {
        return Err(InvalidTrackMemoryPayload::DomainValue);
    }
    let restored = file
        .entries
        .into_iter()
        .map(entry_from_dto)
        .collect::<Result<Vec<_>, _>>()?;
    TrackMemory::restore(capacity, restored).map_err(|_| InvalidTrackMemoryPayload::DomainValue)
}

fn entry_from_dto(dto: EntryV1) -> Result<(VideoKey, TrackMemoryEntry), InvalidTrackMemoryPayload> {
    let key = VideoKey::from_hex(&dto.video_fingerprint_sha256)
        .ok_or(InvalidTrackMemoryPayload::DomainValue)?;
    let audio = dto.audio.map(hint_from_dto).transpose()?;
    // Путь, записанный на другой платформе (нечитаемый здесь), даёт `None`: «выбор не
    // сохранился», а не повреждение всего файла.
    let subtitles = match dto.subtitles {
        None => None,
        Some(choice) => choice_from_dto(choice)?,
    };
    let subtitle_sync_offset = SubtitleSyncOffset::from_millis(dto.subtitle_sync_offset_ms)
        .map_err(|_| InvalidTrackMemoryPayload::DomainValue)?;
    Ok((
        key,
        TrackMemoryEntry::restored(
            RecencyStamp::from_persisted(dto.last_used),
            audio,
            subtitles,
            subtitle_sync_offset,
        ),
    ))
}

fn hint_from_dto(dto: TrackHintV1) -> Result<RememberedTrackHint, InvalidTrackMemoryPayload> {
    RememberedTrackHint::new(
        dto.language,
        dto.title,
        dto.codec_id,
        dto.ordinal_among_kind,
    )
    .map_err(|_| InvalidTrackMemoryPayload::DomainValue)
}

/// `Ok(None)` — выбор — файл с путём чужой платформы, молча терять его можно, ломать файл нет.
fn choice_from_dto(
    dto: SubtitleChoiceV1,
) -> Result<Option<RememberedSubtitleChoice>, InvalidTrackMemoryPayload> {
    Ok(match dto {
        SubtitleChoiceV1::Off => Some(RememberedSubtitleChoice::Off),
        SubtitleChoiceV1::Embedded { track } => {
            Some(RememberedSubtitleChoice::Embedded(hint_from_dto(track)?))
        }
        SubtitleChoiceV1::File { path } => match path_from_dto(path)? {
            Some(path) => Some(RememberedSubtitleChoice::File(
                RememberedFilePath::new(path)
                    .map_err(|_| InvalidTrackMemoryPayload::DomainValue)?,
            )),
            None => None,
        },
    })
}

/// `Ok(None)` — путь валиден, но записан в кодировке другой платформы.
fn path_from_dto(dto: FilePathV1) -> Result<Option<PathBuf>, InvalidTrackMemoryPayload> {
    match dto {
        FilePathV1::Utf8 { value } => Ok(Some(PathBuf::from(value))),
        FilePathV1::UnixBytesHex { hex } => {
            let bytes = decode_hex(&hex).ok_or(InvalidTrackMemoryPayload::DomainValue)?;
            Ok(path_from_unix_bytes(bytes))
        }
        FilePathV1::WideLeHex { hex } => {
            let bytes = decode_hex(&hex).ok_or(InvalidTrackMemoryPayload::DomainValue)?;
            if !bytes.len().is_multiple_of(2) {
                return Err(InvalidTrackMemoryPayload::DomainValue);
            }
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            Ok(path_from_wide_units(&units))
        }
    }
}

#[cfg(unix)]
fn path_from_unix_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn path_from_unix_bytes(_bytes: Vec<u8>) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn path_from_wide_units(units: &[u16]) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_wide(units)))
}

#[cfg(not(windows))]
fn path_from_wide_units(_units: &[u16]) -> Option<PathBuf> {
    None
}

// --------------------------------------------------------------------------------------
// hex переменной длины (для байтов пути)
// --------------------------------------------------------------------------------------

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_hex(encoded: &str) -> Option<Vec<u8>> {
    let raw = encoded.as_bytes();
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    raw.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Some((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?))
        .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
