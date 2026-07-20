//! URI/base resolution для XSPF text и `xml:base`.

use url::Url;

use crate::PlaylistDocumentSource;

use super::error::{XspfParseError, XspfParseErrorKind};
use super::model::XspfLocationCandidate;

/// Строит initial base из retrieval URI либо exact local document path.
pub(super) fn document_base(source: &PlaylistDocumentSource) -> Result<Url, XspfParseError> {
    // Network source уже validated как hierarchical base.
    if let Some(network_uri) = source.parsed_network_uri() {
        let mut document_uri = network_uri.clone();
        // Fragment не является частью retrieval identity для relative resolution.
        document_uri.set_fragment(None);
        return Ok(document_uri);
    }

    // Local XML Base требует абсолютный reversible file URI без filesystem I/O.
    let local_path = source
        .expose_local_path()
        .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::DocumentBaseUnavailable))?;
    Url::from_file_path(local_path)
        .map_err(|()| XspfParseError::new(XspfParseErrorKind::DocumentBaseUnavailable))
}

/// Применяет inherited `xml:base` к текущему element scope.
pub(super) fn resolve_element_base(
    parent_base: &Url,
    raw_xml_base: Option<&str>,
) -> Result<Url, XspfParseError> {
    match raw_xml_base {
        Some(reference) => resolve_reference(parent_base, reference),
        None => Ok(parent_base.clone()),
    }
}

/// Разрешает XSPF location и сохраняет canonical percent-encoded serialization.
pub(super) fn resolve_location(
    element_base: &Url,
    raw_location: &str,
) -> Result<XspfLocationCandidate, XspfParseError> {
    let resolved_uri = resolve_reference(element_base, raw_location)?;
    Ok(XspfLocationCandidate::new(resolved_uri.into()))
}

/// Валидирует absolute application URI без document-relative guessing.
pub(super) fn validate_application_uri(raw_application: &str) -> Result<(), XspfParseError> {
    let normalized_application = trim_xml_whitespace(raw_application);
    validate_reference_spelling(normalized_application)?;
    Url::parse(normalized_application)
        .map(|_| ())
        .map_err(|_| XspfParseError::new(XspfParseErrorKind::InvalidUri))
}

/// Разрешает URI reference после XML Schema-style outer whitespace collapse.
fn resolve_reference(base: &Url, raw_reference: &str) -> Result<Url, XspfParseError> {
    let normalized_reference = trim_xml_whitespace(raw_reference);
    if normalized_reference.is_empty() {
        return Err(XspfParseError::new(XspfParseErrorKind::InvalidUri));
    }
    validate_reference_spelling(normalized_reference)?;
    base.join(normalized_reference)
        .map_err(|_| XspfParseError::new(XspfParseErrorKind::InvalidUri))
}

/// Reject raw ASCII whitespace/control и malformed percent triplets.
fn validate_reference_spelling(reference: &str) -> Result<(), XspfParseError> {
    let reference_bytes = reference.as_bytes();
    let mut byte_index = 0usize;

    while byte_index < reference_bytes.len() {
        let current_byte = reference_bytes[byte_index];
        if current_byte.is_ascii_control() || current_byte == b' ' {
            return Err(XspfParseError::new(XspfParseErrorKind::InvalidUri));
        }
        if current_byte == b'%' {
            let first_hex = reference_bytes.get(byte_index + 1).copied();
            let second_hex = reference_bytes.get(byte_index + 2).copied();
            if !matches!(first_hex, Some(value) if value.is_ascii_hexdigit())
                || !matches!(second_hex, Some(value) if value.is_ascii_hexdigit())
            {
                return Err(XspfParseError::new(XspfParseErrorKind::InvalidUri));
            }
            byte_index += 3;
            continue;
        }
        byte_index += 1;
    }

    Ok(())
}

/// XML Schema lexical values collapse only XML markup whitespace at the edges.
pub(super) fn trim_xml_whitespace(value: &str) -> &str {
    value.trim_matches(matches_xml_whitespace)
}

/// XML 1.0 markup whitespace set не зависит от Unicode locale.
pub(super) const fn matches_xml_whitespace(character: char) -> bool {
    matches!(character, '\u{20}' | '\u{9}' | '\u{d}' | '\u{a}')
}
