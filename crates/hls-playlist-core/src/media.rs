use url::Url;

use crate::{
    ByteRange, ExactReference, HlsKeyDeclaration, HlsKeyFormat, HlsKeyMethod, HlsLineNumber,
    HlsParseError, HlsParseErrorKind, InitializationMap,
    attribute::Attributes,
    parser::{parse_u64, validate_reference},
};

/// Разбирает одну `EXT-X-KEY` declaration без применения profile policy.
pub(crate) fn parse_key(
    attributes: &Attributes<'_>,
    line: HlsLineNumber,
    base: Option<&Url>,
    declaration_sequence: u64,
) -> Result<HlsKeyDeclaration, HlsParseError> {
    let method = match attributes.raw("METHOD").ok_or_else(|| syntax(line))? {
        "NONE" => HlsKeyMethod::None,
        "AES-128" => HlsKeyMethod::Aes128,
        "SAMPLE-AES" => HlsKeyMethod::SampleAes,
        other => HlsKeyMethod::Other(other.into()),
    };
    if matches!(method, HlsKeyMethod::None) {
        if attributes.len() != 1 {
            return Err(required(line));
        }
        return Ok(HlsKeyDeclaration {
            method,
            key_format: HlsKeyFormat::ImplicitIdentity,
            key_format_versions: None,
            uri: None,
            explicit_iv: None,
            declaration_sequence,
        });
    }
    let uri = attributes
        .quoted("URI")
        .ok_or_else(|| required(line))
        .and_then(|reference| {
            validate_reference(reference, line, base)?;
            Ok(ExactReference::new(reference))
        })?;
    let key_format = match attributes.raw("KEYFORMAT") {
        None => HlsKeyFormat::ImplicitIdentity,
        Some(_) => match attributes.quoted("KEYFORMAT").ok_or_else(|| syntax(line))? {
            "identity" => HlsKeyFormat::Identity,
            other => HlsKeyFormat::Other(other.into()),
        },
    };
    let explicit_iv = attributes
        .raw("IV")
        .map(|value| parse_iv(value, line))
        .transpose()?;
    let key_format_versions = match attributes.raw("KEYFORMATVERSIONS") {
        None => None,
        Some(_) => Some(
            attributes
                .quoted("KEYFORMATVERSIONS")
                .ok_or_else(|| syntax(line))?
                .into(),
        ),
    };
    Ok(HlsKeyDeclaration {
        method,
        key_format,
        key_format_versions,
        uri: Some(uri),
        explicit_iv,
        declaration_sequence,
    })
}

/// Разбирает `EXT-X-MAP` и фиксирует действующий key state на этой границе.
pub(crate) fn parse_map(
    attributes: &Attributes<'_>,
    line: HlsLineNumber,
    base: Option<&Url>,
    key: Option<HlsKeyDeclaration>,
) -> Result<InitializationMap, HlsParseError> {
    let reference = attributes.quoted("URI").ok_or_else(|| required(line))?;
    validate_reference(reference, line, base)?;
    let byte_range = match attributes.raw("BYTERANGE") {
        None => None,
        Some(_) => Some(parse_byte_range(
            attributes.quoted("BYTERANGE").ok_or_else(|| syntax(line))?,
            line,
        )?),
    };
    Ok(InitializationMap {
        uri: ExactReference::new(reference),
        byte_range,
        key,
    })
}

/// Разбирает `n[@o]` без преждевременного разрешения implicit offset.
pub(crate) fn parse_byte_range(
    value: &str,
    line: HlsLineNumber,
) -> Result<ByteRange, HlsParseError> {
    let (length, offset) = value
        .split_once('@')
        .map_or((value, None), |(length, offset)| (length, Some(offset)));
    let length = parse_u64(length, line)?;
    if length == 0 {
        return Err(syntax(line));
    }
    Ok(ByteRange {
        length,
        offset: offset.map(|offset| parse_u64(offset, line)).transpose()?,
    })
}

fn parse_iv(value: &str, line: HlsLineNumber) -> Result<[u8; 16], HlsParseError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| syntax(line))?;
    if hex.is_empty() || hex.len() > 32 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(syntax(line));
    }
    let mut padded = [0u8; 16];
    let mut normalized = String::with_capacity(hex.len() + 1);
    if hex.len() % 2 == 1 {
        normalized.push('0');
    }
    normalized.push_str(hex);
    let start = 16 - normalized.len() / 2;
    for (index, pair) in normalized.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).map_err(|_| syntax(line))?;
        padded[start + index] = u8::from_str_radix(text, 16).map_err(|_| syntax(line))?;
    }
    Ok(padded)
}

fn syntax(line: HlsLineNumber) -> HlsParseError {
    HlsParseError::new(HlsParseErrorKind::InvalidTagSyntax { line })
}

fn required(line: HlsLineNumber) -> HlsParseError {
    HlsParseError::new(HlsParseErrorKind::InvalidRequiredStructure { line })
}
