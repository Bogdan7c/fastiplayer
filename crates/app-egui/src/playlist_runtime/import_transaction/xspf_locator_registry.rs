//! App-owned ordered XSPF location admission registry.

use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistImportAvailability,
    PlaylistImportProvenance, PlaylistSingleImportDraft,
};
use playlist_io::XspfLocationCandidate;
use url::Url;

use crate::url_service_adapter::{StartupUrlClassification, classify_startup_url};

/// Safe reason одного rejected fallback candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XspfLocationFallbackIssue {
    /// Candidate не является absolute URL.
    InvalidAbsoluteUri,
    /// `file:` URI нельзя обратимо представить native path-ом.
    UnsupportedFileUri,
    /// Scheme/service registry не допускает candidate.
    UnsupportedServiceLocator,
    /// Domain locator/import payload отверг candidate без secret reflection.
    InvalidDurableLocator,
}

/// First-admissible result не содержит player/open/probe authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct XspfLocationAdmission {
    draft: Option<PlaylistSingleImportDraft>,
    issues: Box<[XspfLocationFallbackIssue]>,
    sensitive_durable_locator_count: usize,
}

impl XspfLocationAdmission {
    /// Возвращает первый admitted draft, если он существовал.
    pub(crate) const fn draft(&self) -> Option<&PlaylistSingleImportDraft> {
        self.draft.as_ref()
    }

    /// Передаёт admitted draft import transaction owner-у.
    pub(crate) fn into_draft(self) -> Option<PlaylistSingleImportDraft> {
        self.draft
    }

    /// Возвращает rejected prefix до первого admissible location.
    pub(crate) fn issues(&self) -> &[XspfLocationFallbackIssue] {
        &self.issues
    }

    /// Возвращает aggregated sensitive count для preview.
    pub(crate) const fn sensitive_durable_locator_count(&self) -> usize {
        self.sensitive_durable_locator_count
    }
}

/// Выбирает ровно первый admissible XSPF location в document order.
pub(crate) fn admit_first_xspf_location(
    candidates: &[XspfLocationCandidate],
    cached_metadata: CachedPlaylistMetadata,
    provenance: PlaylistImportProvenance,
) -> XspfLocationAdmission {
    let mut issues = Vec::new();
    for candidate in candidates {
        let parsed = match Url::parse(candidate.expose_uri_for_admission()) {
            Ok(parsed) => parsed,
            Err(_) => {
                issues.push(XspfLocationFallbackIssue::InvalidAbsoluteUri);
                continue;
            }
        };
        let admitted = if parsed.scheme() == "file" {
            match parsed.to_file_path() {
                Ok(path) => Some((
                    DurableReopenLocator::local(LocalLocator::Native(path)),
                    false,
                )),
                Err(()) => {
                    issues.push(XspfLocationFallbackIssue::UnsupportedFileUri);
                    None
                }
            }
        } else {
            match classify_startup_url(parsed.as_str()) {
                StartupUrlClassification::Supported(locator) => {
                    let sensitive = locator.requires_sensitive_persistence_acknowledgement();
                    match locator.to_playlist_locator() {
                        Ok(locator) => Some((DurableReopenLocator::url(locator), sensitive)),
                        Err(_) => {
                            issues.push(XspfLocationFallbackIssue::InvalidDurableLocator);
                            None
                        }
                    }
                }
                StartupUrlClassification::NotUrl | StartupUrlClassification::Unsupported { .. } => {
                    issues.push(XspfLocationFallbackIssue::UnsupportedServiceLocator);
                    None
                }
            }
        };
        let Some((durable_locator, sensitive)) = admitted else {
            continue;
        };
        let draft = match PlaylistSingleImportDraft::new(
            durable_locator,
            cached_metadata,
            None,
            Vec::new(),
            provenance,
            PlaylistImportAvailability::Available,
        ) {
            Ok(draft) => draft,
            Err(_) => {
                issues.push(XspfLocationFallbackIssue::InvalidDurableLocator);
                break;
            }
        };
        return XspfLocationAdmission {
            draft: Some(draft),
            issues: issues.into_boxed_slice(),
            sensitive_durable_locator_count: usize::from(sensitive),
        };
    }
    XspfLocationAdmission {
        draft: None,
        issues: issues.into_boxed_slice(),
        sensitive_durable_locator_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use playlist_core::{PlaylistImportSourceKind, PlaylistMediaKind};
    use playlist_io::{
        XspfDocumentSource, XspfParseRequest, XspfParserLimits, parse_xspf_document,
    };

    #[test]
    fn registry_selects_first_admissible_location_and_keeps_rejected_prefix() {
        let document = br#"<?xml version="1.0" encoding="UTF-8"?>
            <playlist xmlns="http://xspf.org/ns/0/" version="1">
              <trackList>
                <track>
                  <location>rtsp://unsupported.example/media</location>
                  <location>file:///tmp/first.mkv</location>
                  <location>https://example.test/later.mkv</location>
                </track>
              </trackList>
            </playlist>"#;
        let source = XspfDocumentSource::network("https://example.test/list.xspf").expect("source");
        let playlist = parse_xspf_document(XspfParseRequest::new(
            document,
            source,
            XspfParserLimits::default(),
        ))
        .expect("xspf");
        let root =
            DurableReopenLocator::local(LocalLocator::Native(PathBuf::from("/tmp/list.xspf")));
        let admission = admit_first_xspf_location(
            playlist.tracks()[0].location_candidates(),
            CachedPlaylistMetadata::new("Track", PlaylistMediaKind::Unknown),
            PlaylistImportProvenance::new(root, PlaylistImportSourceKind::Xspf, None),
        );

        assert_eq!(
            admission.issues(),
            &[XspfLocationFallbackIssue::UnsupportedServiceLocator]
        );
        assert_eq!(admission.sensitive_durable_locator_count(), 0);
        assert_eq!(
            admission
                .draft()
                .and_then(|draft| draft.reopen_locator().expose_local_for_reopen())
                .and_then(LocalLocator::expose_native_path_for_open),
            Some(std::path::Path::new("/tmp/first.mkv"))
        );
    }
}
