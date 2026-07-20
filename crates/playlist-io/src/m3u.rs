use std::num::NonZeroU32;

use playlist_core::PlaylistImportSourceKind;

use crate::{
    AdaptiveManifestReference, GenericM3uEntryDraft, GenericM3uPreview,
    LocalHlsManifestUnsupported, M3uDocument, M3uDocumentSource, M3uDurationHint, M3uExtInfHint,
    M3uImportIssueKind, M3uLineNumber, M3uParseError, M3uParseErrorKind, M3uParserLimits,
    hls::{HlsCandidate, classify_hls_candidate, validate_hls},
    issue::IssueCollector,
    locator::{
        LocatorResolutionError, build_import_draft, duration_from_seconds, resolve_generic_locator,
    },
};

/// Declared generic container family до content-first HLS distinction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M3uDeclaredFormat {
    /// Legacy/generic UTF-8 M3U с optional BOM warning.
    M3u,
    /// UTF-8 M3U8; BOM запрещён даже для non-HLS content.
    M3u8,
}

impl M3uDeclaredFormat {
    /// Переводит parser intent в neutral provenance vocabulary.
    const fn provenance_kind(self) -> PlaylistImportSourceKind {
        match self {
            Self::M3u => PlaylistImportSourceKind::M3u,
            Self::M3u8 => PlaylistImportSourceKind::M3u8,
        }
    }
}

/// Self-documenting parse request без hidden source/config defaults.
pub struct M3uParseRequest<'document> {
    /// Caller-owned bytes; parser не читает их повторно из I/O.
    document_bytes: &'document [u8],
    /// Manifest/import identity и base-resolution owner.
    source: M3uDocumentSource,
    /// Declared M3U/M3U8 family до content classification.
    declared_format: M3uDeclaredFormat,
    /// Explicit bounded parse profile.
    limits: M3uParserLimits,
}

impl<'document> M3uParseRequest<'document> {
    /// Создаёт полный parse intent без positional flags.
    pub const fn new(
        document_bytes: &'document [u8],
        source: M3uDocumentSource,
        declared_format: M3uDeclaredFormat,
        limits: M3uParserLimits,
    ) -> Self {
        Self {
            document_bytes,
            source,
            declared_format,
            limits,
        }
    }
}

/// Единственная S05 entry point: parse bytes без hidden I/O.
pub fn parse_m3u_document(request: M3uParseRequest<'_>) -> Result<M3uDocument, M3uParseError> {
    let M3uParseRequest {
        document_bytes,
        source,
        declared_format,
        limits,
    } = request;

    if document_bytes.len() > limits.max_document_bytes() {
        return Err(M3uParseError::new(M3uParseErrorKind::DocumentLimitExceeded));
    }

    let original_text = std::str::from_utf8(document_bytes)
        .map_err(|_| M3uParseError::new(M3uParseErrorKind::InvalidUtf8))?;
    let (text_without_bom, had_bom) = match original_text.strip_prefix('\u{feff}') {
        Some(text_without_bom) => (text_without_bom, true),
        None => (original_text, false),
    };

    if matches!(classify_hls_candidate(text_without_bom), HlsCandidate::Hls) {
        let topology = validate_hls(original_text, had_bom, &source, limits)?;
        return if source.is_network() {
            Ok(M3uDocument::AdaptiveManifestReference(
                AdaptiveManifestReference::new(source, topology),
            ))
        } else {
            Ok(M3uDocument::LocalHlsManifestUnsupported(
                LocalHlsManifestUnsupported::new(source, topology),
            ))
        };
    }
    if had_bom && declared_format == M3uDeclaredFormat::M3u8 {
        return Err(M3uParseError::new(
            M3uParseErrorKind::GenericM3u8BomNotAllowed,
        ));
    }

    Ok(M3uDocument::Generic(parse_generic_m3u(
        text_without_bom,
        had_bom,
        &source,
        declared_format,
        limits,
    )))
}

/// Generic tolerant pass с bounded partial preview.
fn parse_generic_m3u(
    text: &str,
    had_bom: bool,
    source: &M3uDocumentSource,
    declared_format: M3uDeclaredFormat,
    limits: M3uParserLimits,
) -> GenericM3uPreview {
    let mut issues = IssueCollector::new(limits.max_issues());
    if had_bom {
        issues.push(
            M3uLineNumber::from_one_based(1),
            M3uImportIssueKind::Utf8BomIgnored,
        );
    }

    let mut entries = Vec::new();
    let mut pending_extinf: Option<(M3uLineNumber, M3uExtInfHint)> = None;
    let mut truncated_by_item_limit = false;

    for (zero_based_line, raw_line) in text.split('\n').enumerate() {
        let line_number = M3uLineNumber::from_one_based(zero_based_line + 1);
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if line.len() > limits.max_line_bytes() {
            issues.push(line_number, M3uImportIssueKind::LineLimitExceeded);
            if let Some((extinf_line, _)) = pending_extinf.take() {
                issues.push(extinf_line, M3uImportIssueKind::OrphanedExtInf);
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if line == "#EXTM3U" {
            continue;
        }
        if let Some(extinf_value) = line.strip_prefix("#EXTINF:") {
            if let Some((previous_line, _)) = pending_extinf.take() {
                issues.push(previous_line, M3uImportIssueKind::OrphanedExtInf);
            }
            match parse_generic_extinf(extinf_value) {
                Some(extinf_hint) => pending_extinf = Some((line_number, extinf_hint)),
                None => issues.push(line_number, M3uImportIssueKind::MalformedExtInf),
            }
            continue;
        }
        if line.starts_with("#EXT") {
            issues.push(line_number, M3uImportIssueKind::UnsupportedDirective);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        if entries.len() == limits.max_items() {
            issues.push(line_number, M3uImportIssueKind::ItemLimitExceeded);
            truncated_by_item_limit = true;
            break;
        }

        let extinf_hint = pending_extinf.take().map(|(_, hint)| hint);
        let resolved_locator = match resolve_generic_locator(line, source) {
            Ok(resolved_locator) => resolved_locator,
            Err(LocatorResolutionError::Malformed) => {
                issues.push(line_number, M3uImportIssueKind::MalformedLocator);
                continue;
            }
            Err(LocatorResolutionError::UnsupportedScheme) => {
                issues.push(line_number, M3uImportIssueKind::UnsupportedLocatorScheme);
                continue;
            }
        };

        let source_ordinal = NonZeroU32::new(
            u32::try_from(entries.len() + 1).expect("item cap is lower than u32::MAX"),
        )
        .expect("source ordinal starts at one");
        let import_draft = match build_import_draft(
            resolved_locator,
            source,
            declared_format.provenance_kind(),
            source_ordinal,
            extinf_hint.as_ref(),
        ) {
            Ok(import_draft) => import_draft,
            Err(_) => {
                issues.push(line_number, M3uImportIssueKind::ImportDraftRejected);
                continue;
            }
        };
        entries.push(GenericM3uEntryDraft::new(
            line_number,
            import_draft,
            extinf_hint,
        ));
    }

    if let Some((extinf_line, _)) = pending_extinf {
        issues.push(extinf_line, M3uImportIssueKind::OrphanedExtInf);
    }

    GenericM3uPreview::new(entries, issues.finish(), truncated_by_item_limit)
}

/// Разбирает declared signed decimal и title.
fn parse_generic_extinf(value: &str) -> Option<M3uExtInfHint> {
    let (duration_text, display_title) = value.split_once(',')?;
    if duration_text.is_empty() || duration_text.chars().any(char::is_whitespace) {
        return None;
    }

    let parsed_seconds = duration_text.parse::<f64>().ok()?;
    if !parsed_seconds.is_finite() {
        return None;
    }
    let duration = if parsed_seconds.is_sign_negative() {
        M3uDurationHint::Unknown
    } else {
        M3uDurationHint::Known(duration_from_seconds(parsed_seconds)?)
    };
    let display_title = (!display_title.is_empty()).then(|| display_title.to_owned());
    Some(M3uExtInfHint::new(duration, display_title))
}
