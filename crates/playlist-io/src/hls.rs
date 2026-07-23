//! Тонкий compatibility adapter к единственному shared HLS parser owner-у.

use hls_playlist_core::{
    HlsLineNumber, HlsParseErrorKind, HlsParseRequest, HlsParserLimits, HlsPlaylist,
    is_hls_candidate, parse_hls_playlist,
};

use crate::{
    HlsManifestTopology, M3uDocumentSource, M3uLineNumber, M3uParseError, M3uParseErrorKind,
    M3uParserLimits,
};

/// Результат content-first marker scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsCandidate {
    /// Ни одного HLS marker-а не найдено.
    Generic,
    /// Document обязан пройти strict shared HLS validation.
    Hls,
}

/// Классифицирует HLS marker до generic EXTINF interpretation.
pub(crate) fn classify_hls_candidate(text_without_bom: &str) -> HlsCandidate {
    if is_hls_candidate(text_without_bom) {
        HlsCandidate::Hls
    } else {
        HlsCandidate::Generic
    }
}

/// Сохраняет observable S05 topology outcome, не дублируя HLS validation.
pub(crate) fn validate_hls(
    original_text: &str,
    _had_bom: bool,
    source: &M3uDocumentSource,
    limits: M3uParserLimits,
) -> Result<HlsManifestTopology, M3uParseError> {
    let shared_limits = HlsParserLimits::new(
        limits.max_document_bytes(),
        limits.max_line_bytes(),
        limits.max_items(),
        limits.max_items(),
        limits.max_items(),
        128,
    )
    .expect("M3U limits and fixed attribute budget are non-zero");
    let reference_base = source.parsed_network_uri().map(url::Url::as_str);
    let playlist = parse_hls_playlist(HlsParseRequest::new(
        original_text.as_bytes(),
        reference_base,
        shared_limits,
    ))
    .map_err(map_shared_error)?;
    Ok(match playlist {
        HlsPlaylist::Master(_) => HlsManifestTopology::Master,
        HlsPlaylist::Media(_) => HlsManifestTopology::Media,
    })
}

fn map_shared_error(error: hls_playlist_core::HlsParseError) -> M3uParseError {
    let kind = match error.kind() {
        HlsParseErrorKind::DocumentLimitExceeded => M3uParseErrorKind::DocumentLimitExceeded,
        HlsParseErrorKind::InvalidUtf8 => M3uParseErrorKind::InvalidUtf8,
        HlsParseErrorKind::BomNotAllowed => M3uParseErrorKind::HlsBomNotAllowed,
        HlsParseErrorKind::LineLimitExceeded { line } => M3uParseErrorKind::HlsLineLimitExceeded {
            line: map_line(line),
        },
        HlsParseErrorKind::InvalidLineEnding { line } => M3uParseErrorKind::HlsInvalidLineEnding {
            line: map_line(line),
        },
        HlsParseErrorKind::ControlCharacter { line } => M3uParseErrorKind::HlsControlCharacter {
            line: map_line(line),
        },
        HlsParseErrorKind::NotNfc => M3uParseErrorKind::HlsNotNfc,
        HlsParseErrorKind::MissingHeader => M3uParseErrorKind::HlsMissingHeader,
        HlsParseErrorKind::InvalidTagCase { line } => M3uParseErrorKind::HlsInvalidTagCase {
            line: map_line(line),
        },
        HlsParseErrorKind::WhitespaceNotAllowed { line } => {
            M3uParseErrorKind::HlsWhitespaceNotAllowed {
                line: map_line(line),
            }
        }
        HlsParseErrorKind::InvalidTagSyntax { line }
        | HlsParseErrorKind::AttributeLimitExceeded { line } => {
            M3uParseErrorKind::HlsInvalidTagSyntax {
                line: map_line(line),
            }
        }
        HlsParseErrorKind::DuplicateAttribute { line } => {
            M3uParseErrorKind::HlsDuplicateAttribute {
                line: map_line(line),
            }
        }
        HlsParseErrorKind::DuplicateTag { line } => M3uParseErrorKind::HlsDuplicateTag {
            line: map_line(line),
        },
        HlsParseErrorKind::InvalidReference { line } => M3uParseErrorKind::HlsInvalidUri {
            line: map_line(line),
        },
        HlsParseErrorKind::MixedTopology => M3uParseErrorKind::HlsMixedTopology,
        HlsParseErrorKind::UnknownTopology => M3uParseErrorKind::HlsUnknownTopology,
        HlsParseErrorKind::InvalidRequiredStructure { line } => {
            M3uParseErrorKind::HlsInvalidRequiredStructure {
                line: map_line(line),
            }
        }
        HlsParseErrorKind::SegmentLimitExceeded
        | HlsParseErrorKind::VariantLimitExceeded
        | HlsParseErrorKind::RenditionLimitExceeded => {
            M3uParseErrorKind::HlsInvalidRequiredStructure {
                line: M3uLineNumber::from_one_based(1),
            }
        }
    };
    M3uParseError::new(kind)
}

fn map_line(line: HlsLineNumber) -> M3uLineNumber {
    let line = usize::try_from(line.get()).unwrap_or(usize::MAX);
    M3uLineNumber::from_one_based(line)
}
