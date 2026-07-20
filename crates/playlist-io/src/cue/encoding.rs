//! Explicit CUE text encoding boundary без legacy code-page guessing.

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
    let decoded = std::str::from_utf8(document_bytes)
        .map_err(|_| unsupported_encoding())?
        .to_owned();
    if decoded.contains('\0') {
        return Err(unsupported_encoding());
    }
    Ok((decoded, CueTextEncoding::Utf8))
}

/// Декодирует BOM-stripped UTF-16 с explicit byte order.
fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, CueParseError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(unsupported_encoding());
    }
    let units = bytes
        .chunks_exact(2)
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
