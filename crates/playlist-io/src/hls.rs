use std::collections::HashSet;

use unicode_normalization::is_nfc;
use url::Url;

use crate::{
    HlsManifestTopology, M3uDocumentSource, M3uLineNumber, M3uParseError, M3uParseErrorKind,
    M3uParserLimits,
};

/// Результат content-first HLS candidate scan.
pub(crate) enum HlsCandidate {
    /// Ни одного case-insensitive EXT-X marker; документ generic.
    Generic,
    /// Документ обязан пройти strict HLS validation.
    Hls,
}

/// Классифицирует HLS marker до generic EXTINF interpretation.
pub(crate) fn classify_hls_candidate(text_without_bom: &str) -> HlsCandidate {
    if text_without_bom.lines().any(|line| {
        line.get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT-X-"))
    }) {
        HlsCandidate::Hls
    } else {
        HlsCandidate::Generic
    }
}

/// Валидирует RFC 8216 text/grammar/topology без выдачи segment rows.
pub(crate) fn validate_hls(
    original_text: &str,
    had_bom: bool,
    source: &M3uDocumentSource,
    limits: M3uParserLimits,
) -> Result<HlsManifestTopology, M3uParseError> {
    if had_bom {
        return Err(M3uParseError::new(M3uParseErrorKind::HlsBomNotAllowed));
    }
    validate_line_endings(original_text)?;
    validate_control_characters(original_text)?;
    if !is_nfc(original_text) {
        return Err(M3uParseError::new(M3uParseErrorKind::HlsNotNfc));
    }

    let lines = collect_hls_lines(original_text, limits)?;
    if lines.first().map(|line| line.text) != Some("#EXTM3U") {
        return Err(M3uParseError::new(M3uParseErrorKind::HlsMissingHeader));
    }

    let mut topology = TopologyEvidence::default();
    let mut pending_stream_inf = None;
    let mut pending_extinf = None;
    let mut has_master_reference = false;
    let mut singleton_tags = HashSet::from(["#EXTM3U"]);

    for line in lines.iter().skip(1) {
        if line.text.is_empty() {
            continue;
        }
        if line.text.starts_with('#') {
            if line
                .text
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT"))
            {
                let parsed_tag = parse_hls_tag(line)?;
                if tag_must_be_unique(parsed_tag.name) && !singleton_tags.insert(parsed_tag.name) {
                    return Err(M3uParseError::new(M3uParseErrorKind::HlsDuplicateTag {
                        line: line.line_number,
                    }));
                }
                validate_tag_value(&parsed_tag, line.line_number, source)?;
                record_topology(&parsed_tag, &mut topology);

                if parsed_tag.name == "#EXT-X-STREAM-INF" {
                    pending_stream_inf = Some(line.line_number);
                } else if parsed_tag.name == "#EXTINF" {
                    pending_extinf = Some(line.line_number);
                }
            }
            continue;
        }

        if line.text.chars().any(char::is_whitespace) {
            return Err(M3uParseError::new(
                M3uParseErrorKind::HlsWhitespaceNotAllowed {
                    line: line.line_number,
                },
            ));
        }
        validate_hls_uri(line.text, source).map_err(|()| {
            M3uParseError::new(M3uParseErrorKind::HlsInvalidUri {
                line: line.line_number,
            })
        })?;

        if pending_stream_inf.take().is_some() {
            has_master_reference = true;
        } else if pending_extinf.take().is_none() {
            return Err(M3uParseError::new(
                M3uParseErrorKind::HlsInvalidRequiredStructure {
                    line: line.line_number,
                },
            ));
        }
    }

    if let Some(line) = pending_stream_inf.or(pending_extinf) {
        return Err(M3uParseError::new(
            M3uParseErrorKind::HlsInvalidRequiredStructure { line },
        ));
    }
    if topology.master && topology.media {
        return Err(M3uParseError::new(M3uParseErrorKind::HlsMixedTopology));
    }
    if topology.master {
        if !has_master_reference && !topology.has_inline_master_reference {
            return Err(M3uParseError::new(
                M3uParseErrorKind::HlsInvalidRequiredStructure {
                    line: M3uLineNumber::from_one_based(1),
                },
            ));
        }
        return Ok(HlsManifestTopology::Master);
    }
    if topology.media {
        if !topology.has_target_duration {
            return Err(M3uParseError::new(
                M3uParseErrorKind::HlsInvalidRequiredStructure {
                    line: M3uLineNumber::from_one_based(1),
                },
            ));
        }
        return Ok(HlsManifestTopology::Media);
    }

    Err(M3uParseError::new(M3uParseErrorKind::HlsUnknownTopology))
}

/// Borrowed validated line.
struct HlsLine<'a> {
    /// One-based source line.
    line_number: M3uLineNumber,
    /// Text without CRLF/LF.
    text: &'a str,
}

/// Parsed HLS tag name/value.
struct ParsedHlsTag<'a> {
    /// Exact case-sensitive tag name.
    name: &'a str,
    /// Optional content after colon.
    value: Option<&'a str>,
}

/// Topology evidence до EXTINF semantic interpretation.
#[derive(Default)]
struct TopologyEvidence {
    /// Master-only tag seen.
    master: bool,
    /// Media/media-segment tag seen.
    media: bool,
    /// Master reference encoded inside tag attribute.
    has_inline_master_reference: bool,
    /// Required Media Playlist target duration присутствует.
    has_target_duration: bool,
}

/// Проверяет только LF и CRLF.
fn validate_line_endings(text: &str) -> Result<(), M3uParseError> {
    let bytes = text.as_bytes();
    let mut line_number = 1usize;

    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'\n' {
            line_number = line_number.saturating_add(1);
        } else if byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err(M3uParseError::new(
                M3uParseErrorKind::HlsInvalidLineEnding {
                    line: M3uLineNumber::from_one_based(line_number),
                },
            ));
        }
    }
    Ok(())
}

/// Проверяет RFC control-character exclusion.
fn validate_control_characters(text: &str) -> Result<(), M3uParseError> {
    let mut line_number = 1usize;

    for character in text.chars() {
        if character == '\n' {
            line_number = line_number.saturating_add(1);
            continue;
        }
        let codepoint = character as u32;
        let forbidden =
            (codepoint <= 0x1f && character != '\r') || (0x7f..=0x9f).contains(&codepoint);
        if forbidden {
            return Err(M3uParseError::new(M3uParseErrorKind::HlsControlCharacter {
                line: M3uLineNumber::from_one_based(line_number),
            }));
        }
    }
    Ok(())
}

/// Материализует только borrowed line descriptors и проверяет line cap.
fn collect_hls_lines(
    text: &str,
    limits: M3uParserLimits,
) -> Result<Vec<HlsLine<'_>>, M3uParseError> {
    text.split('\n')
        .enumerate()
        .map(|(zero_based_line, raw_line)| {
            let text = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let line_number = M3uLineNumber::from_one_based(zero_based_line + 1);
            if text.len() > limits.max_line_bytes() {
                return Err(M3uParseError::new(
                    M3uParseErrorKind::HlsLineLimitExceeded { line: line_number },
                ));
            }
            Ok(HlsLine { line_number, text })
        })
        .collect()
}

/// Разбирает exact case-sensitive HLS tag.
fn parse_hls_tag<'a>(line: &'a HlsLine<'a>) -> Result<ParsedHlsTag<'a>, M3uParseError> {
    let (name, value) = match line.text.split_once(':') {
        Some((name, value)) => (name, Some(value)),
        None => (line.text, None),
    };

    if name
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT-X-"))
        && !name.starts_with("#EXT-X-")
    {
        return Err(M3uParseError::new(M3uParseErrorKind::HlsInvalidTagCase {
            line: line.line_number,
        }));
    }

    let valid_name = name == "#EXTINF"
        || name == "#EXTM3U"
        || name.strip_prefix("#EXT-X-").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if !valid_name {
        return Err(M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax {
            line: line.line_number,
        }));
    }

    Ok(ParsedHlsTag { name, value })
}

/// Записывает topology evidence по RFC tag families.
fn record_topology(tag: &ParsedHlsTag<'_>, topology: &mut TopologyEvidence) {
    if matches!(
        tag.name,
        "#EXT-X-MEDIA"
            | "#EXT-X-STREAM-INF"
            | "#EXT-X-I-FRAME-STREAM-INF"
            | "#EXT-X-SESSION-DATA"
            | "#EXT-X-SESSION-KEY"
    ) {
        topology.master = true;
        topology.has_inline_master_reference |= tag.name == "#EXT-X-I-FRAME-STREAM-INF"
            && tag
                .value
                .and_then(|value| find_attribute_value(value, "URI"))
                .is_some();
    }

    if tag.name == "#EXTINF"
        || matches!(
            tag.name,
            "#EXT-X-BYTERANGE"
                | "#EXT-X-DISCONTINUITY"
                | "#EXT-X-KEY"
                | "#EXT-X-MAP"
                | "#EXT-X-PROGRAM-DATE-TIME"
                | "#EXT-X-DATERANGE"
                | "#EXT-X-TARGETDURATION"
                | "#EXT-X-MEDIA-SEQUENCE"
                | "#EXT-X-DISCONTINUITY-SEQUENCE"
                | "#EXT-X-ENDLIST"
                | "#EXT-X-PLAYLIST-TYPE"
                | "#EXT-X-I-FRAMES-ONLY"
        )
    {
        topology.media = true;
    }
    topology.has_target_duration |= tag.name == "#EXT-X-TARGETDURATION";
}

/// Валидирует известную value grammar и generic whitespace invariant.
fn validate_tag_value(
    tag: &ParsedHlsTag<'_>,
    line_number: M3uLineNumber,
    source: &M3uDocumentSource,
) -> Result<(), M3uParseError> {
    if tag.name == "#EXTINF" {
        return validate_extinf(tag.value, line_number);
    }
    if is_attribute_list_tag(tag.name) {
        let value = tag.value.ok_or_else(|| {
            M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax { line: line_number })
        })?;
        validate_attribute_list(value, line_number)?;
        return validate_uri_attribute(tag.name, value, line_number, source);
    }

    if tag
        .value
        .is_some_and(|value| value.chars().any(char::is_whitespace))
    {
        return Err(M3uParseError::new(
            M3uParseErrorKind::HlsWhitespaceNotAllowed { line: line_number },
        ));
    }
    Ok(())
}

/// Валидирует URI attributes известных RFC tags без сохранения child locators.
fn validate_uri_attribute(
    tag_name: &str,
    attribute_list: &str,
    line_number: M3uLineNumber,
    source: &M3uDocumentSource,
) -> Result<(), M3uParseError> {
    let uri_attribute = find_attribute_value(attribute_list, "URI");
    let uri_is_required = matches!(
        tag_name,
        "#EXT-X-MAP" | "#EXT-X-I-FRAME-STREAM-INF" | "#EXT-X-SESSION-KEY"
    );

    let Some(encoded_uri) = uri_attribute else {
        return if uri_is_required {
            Err(M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax {
                line: line_number,
            }))
        } else {
            Ok(())
        };
    };
    let raw_uri = encoded_uri
        .strip_prefix('"')
        .and_then(|without_opening_quote| without_opening_quote.strip_suffix('"'))
        .ok_or_else(|| {
            M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax { line: line_number })
        })?;

    validate_hls_uri(raw_uri, source)
        .map_err(|()| M3uParseError::new(M3uParseErrorKind::HlsInvalidUri { line: line_number }))
}

/// Перечисляет known attribute-list tags initial classifier profile.
fn is_attribute_list_tag(name: &str) -> bool {
    matches!(
        name,
        "#EXT-X-KEY"
            | "#EXT-X-MAP"
            | "#EXT-X-DATERANGE"
            | "#EXT-X-MEDIA"
            | "#EXT-X-STREAM-INF"
            | "#EXT-X-I-FRAME-STREAM-INF"
            | "#EXT-X-SESSION-DATA"
            | "#EXT-X-SESSION-KEY"
            | "#EXT-X-START"
    )
}

/// Перечисляет RFC tags, которые не могут повторяться в одном Playlist.
fn tag_must_be_unique(name: &str) -> bool {
    matches!(
        name,
        "#EXTM3U"
            | "#EXT-X-VERSION"
            | "#EXT-X-TARGETDURATION"
            | "#EXT-X-MEDIA-SEQUENCE"
            | "#EXT-X-DISCONTINUITY-SEQUENCE"
            | "#EXT-X-ENDLIST"
            | "#EXT-X-PLAYLIST-TYPE"
            | "#EXT-X-I-FRAMES-ONLY"
            | "#EXT-X-INDEPENDENT-SEGMENTS"
            | "#EXT-X-START"
    )
}

/// Валидирует HLS EXTINF duration до использования как topology marker.
fn validate_extinf(value: Option<&str>, line_number: M3uLineNumber) -> Result<(), M3uParseError> {
    let value = value.ok_or_else(|| {
        M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax { line: line_number })
    })?;
    let (duration, _) = value.split_once(',').ok_or_else(|| {
        M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax { line: line_number })
    })?;

    if duration.is_empty()
        || duration.chars().any(char::is_whitespace)
        || duration
            .parse::<f64>()
            .ok()
            .is_none_or(|parsed| !parsed.is_finite() || parsed.is_sign_negative())
    {
        return Err(M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax {
            line: line_number,
        }));
    }
    Ok(())
}

/// Валидирует comma-separated attribute list, quotes и duplicate names.
fn validate_attribute_list(value: &str, line_number: M3uLineNumber) -> Result<(), M3uParseError> {
    let attributes = split_attribute_list(value).ok_or_else(|| {
        M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax { line: line_number })
    })?;
    let mut names = HashSet::with_capacity(attributes.len());

    for attribute in attributes {
        let (name, attribute_value) = attribute.split_once('=').ok_or_else(|| {
            M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax { line: line_number })
        })?;
        if has_unquoted_whitespace(attribute) {
            return Err(M3uParseError::new(
                M3uParseErrorKind::HlsWhitespaceNotAllowed { line: line_number },
            ));
        }
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
            || !is_valid_attribute_value(attribute_value)
        {
            return Err(M3uParseError::new(M3uParseErrorKind::HlsInvalidTagSyntax {
                line: line_number,
            }));
        }
        if !names.insert(name) {
            return Err(M3uParseError::new(
                M3uParseErrorKind::HlsDuplicateAttribute { line: line_number },
            ));
        }
    }
    Ok(())
}

/// Проверяет quoted/unquoted outer grammar без format-specific attribute semantics.
fn is_valid_attribute_value(value: &str) -> bool {
    if let Some(quoted_value) = value
        .strip_prefix('"')
        .and_then(|without_opening_quote| without_opening_quote.strip_suffix('"'))
    {
        return !quoted_value
            .chars()
            .any(|character| matches!(character, '"' | '\r' | '\n'));
    }

    !value.is_empty()
        && !value.chars().any(|character| {
            matches!(character, '"' | ',' | '\r' | '\n') || character.is_whitespace()
        })
}

/// Находит exact attribute value после общей attribute-list validation.
fn find_attribute_value<'attribute_list>(
    attribute_list: &'attribute_list str,
    expected_name: &str,
) -> Option<&'attribute_list str> {
    split_attribute_list(attribute_list)?
        .into_iter()
        .find_map(|attribute| {
            let (name, value) = attribute.split_once('=')?;
            (name == expected_name).then_some(value)
        })
}

/// Делит attribute list только по commas вне quoted string.
fn split_attribute_list(value: &str) -> Option<Vec<&str>> {
    let mut attributes = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;

    for (index, character) in value.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                attributes.push(value.get(start..index)?);
                start = index + 1;
            }
            '\r' | '\n' => return None,
            _ => {}
        }
    }
    if quoted {
        return None;
    }
    attributes.push(value.get(start..)?);
    if attributes.iter().any(|attribute| attribute.is_empty()) {
        return None;
    }
    Some(attributes)
}

/// Находит whitespace только вне quoted value.
fn has_unquoted_whitespace(value: &str) -> bool {
    let mut quoted = false;
    for character in value.chars() {
        if character == '"' {
            quoted = !quoted;
        } else if !quoted && character.is_whitespace() {
            return true;
        }
    }
    quoted
}

/// Проверяет URI syntax/base resolution без сохранения child URI.
fn validate_hls_uri(raw_uri: &str, source: &M3uDocumentSource) -> Result<(), ()> {
    if let Ok(absolute_uri) = Url::parse(raw_uri) {
        return (!absolute_uri.cannot_be_a_base()).then_some(()).ok_or(());
    }

    if let Some(base_uri) = source.parsed_network_uri() {
        return base_uri.join(raw_uri).map(|_| ()).map_err(|_| ());
    }

    if raw_uri.is_empty() {
        return Err(());
    }
    Ok(())
}
