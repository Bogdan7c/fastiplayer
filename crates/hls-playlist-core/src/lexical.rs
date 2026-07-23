use unicode_normalization::is_nfc;

use crate::{
    HlsLineNumber, HlsParseError, HlsParseErrorKind, HlsParserLimits,
    parser::{Line, Tag},
};

/// Проверяет общие RFC text rules до структурных allocations.
pub(super) fn validate_text(text: &str) -> Result<(), HlsParseError> {
    if !is_nfc(text) {
        return Err(HlsParseError::new(HlsParseErrorKind::NotNfc));
    }
    let bytes = text.as_bytes();
    let mut line = HlsLineNumber::from_index(0);
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err(HlsParseError::new(HlsParseErrorKind::InvalidLineEnding {
                line,
            }));
        }
        if byte == b'\n' {
            line = HlsLineNumber::from_index(line.get() as usize);
        } else if (byte < 0x20 && byte != b'\r' && byte != b'\t') || byte == 0x7f {
            return Err(HlsParseError::new(HlsParseErrorKind::ControlCharacter {
                line,
            }));
        }
    }
    Ok(())
}

/// Материализует только bounded borrowed descriptors физических строк.
pub(super) fn collect_lines(
    text: &str,
    limits: HlsParserLimits,
) -> Result<Vec<Line<'_>>, HlsParseError> {
    let mut lines = Vec::new();
    for (index, physical) in text.split('\n').enumerate() {
        let line_text = physical.strip_suffix('\r').unwrap_or(physical);
        let number = HlsLineNumber::from_index(index);
        if line_text.len() > limits.max_line_bytes() {
            return Err(HlsParseError::new(HlsParseErrorKind::LineLimitExceeded {
                line: number,
            }));
        }
        lines.push(Line {
            number,
            text: line_text,
        });
    }
    Ok(lines)
}

/// Разбирает exact case-sensitive identity HLS tag.
pub(super) fn parse_tag(line: Line<'_>) -> Result<Tag<'_>, HlsParseError> {
    let (name, value) = line
        .text
        .split_once(':')
        .map_or((line.text, None), |(name, value)| (name, Some(value)));
    if name
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT-X-"))
        && !name.starts_with("#EXT-X-")
    {
        return Err(HlsParseError::new(HlsParseErrorKind::InvalidTagCase {
            line: line.number,
        }));
    }
    let valid = name == "#EXTINF"
        || name == "#EXTM3U"
        || name.strip_prefix("#EXT-X-").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if !valid {
        return Err(HlsParseError::new(HlsParseErrorKind::InvalidTagSyntax {
            line: line.number,
        }));
    }
    Ok(Tag {
        name,
        value,
        line: line.number,
    })
}
