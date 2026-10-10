//! Explicit CUE text encoding boundary.
//!
//! Порядок: BOM (UTF-8 / UTF-16LE / UTF-16BE) строго по BOM; без BOM — UTF-8, а при
//! невалидном UTF-8 — запасной вариант Windows-1251 (решение владельца B1, обоснование
//! и границы — в `crate::text_encoding`). BOM-less UTF-16 по-прежнему не угадывается:
//! такой файл содержит NUL и отвергается.

use crate::text_encoding::{LegacyFallback, PlaylistTextEncoding, decode_utf8_or_windows_1251};

use super::{CueParseError, CueParseErrorKind, CueTextEncoding};

/// Декодирует только explicit S12 encoding profile.
pub(super) fn decode_cue_text(
    document_bytes: &[u8],
) -> Result<(String, CueTextEncoding), CueParseError> {
    if let Some(utf8_bytes) = document_bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        let decoded = std::str::from_utf8(utf8_bytes)
            .map_err(|_| unsupported_encoding())?
            .to_owned();
        return Ok((decoded, CueTextEncoding::Utf8WithBom));
    }
    if let Some(utf16_bytes) = document_bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(utf16_bytes, true)
            .map(|decoded| (decoded, CueTextEncoding::Utf16LittleEndianWithBom));
    }
    if let Some(utf16_bytes) = document_bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(utf16_bytes, false)
            .map(|decoded| (decoded, CueTextEncoding::Utf16BigEndianWithBom));
    }
    let (decoded, detected) =
        decode_utf8_or_windows_1251(document_bytes, LegacyFallback::Windows1251)
            .map_err(|_| unsupported_encoding())?;
    // NUL означает бинарный файл либо UTF-16 без BOM — не плейлист в однобайтной кодировке.
    if decoded.contains('\0') {
        return Err(unsupported_encoding());
    }
    let encoding = match detected {
        PlaylistTextEncoding::Windows1251 => CueTextEncoding::Windows1251,
        // BOM здесь уже отсечён выше, поэтому валидный UTF-8 — всегда без BOM.
        PlaylistTextEncoding::Utf8 | PlaylistTextEncoding::Utf8WithBom => CueTextEncoding::Utf8,
    };
    Ok((decoded.into_owned(), encoding))
}

/// Декодирует BOM-stripped UTF-16 с explicit byte order.
fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, CueParseError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(unsupported_encoding());
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let encoded = [pair[0], pair[1]];
            if little_endian {
                u16::from_le_bytes(encoded)
            } else {
                u16::from_be_bytes(encoded)
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| unsupported_encoding())
}

/// Создаёт единый typed encoding failure.
fn unsupported_encoding() -> CueParseError {
    CueParseError::new(CueParseErrorKind::UnsupportedOrInvalidEncoding)
}
