//! Pure mapping bounded `playlist-io` expansion-а в S08 ID-less draft.

use std::num::NonZeroU32;
use std::path::PathBuf;

use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistCompoundImportDraft,
    PlaylistImportEntryDraft, PlaylistImportProvenance, PlaylistImportSourceKind,
    PlaylistMediaKind, PlaylistSingleImportDraft,
};
use playlist_io::{
    ExpandedLocalPlaylistDocument, ExpandedLocalPlaylistEntry, LocalPlaylistDocumentFormat,
    LocalPlaylistExpansion, UnexpandedLocalPlaylistInclude, XspfGroup, XspfTrack,
};

use super::super::import_transaction::{
    PlaylistImportDraft, PlaylistImportIssue, PlaylistImportIssueKind, PlaylistImportRejectedCount,
    PlaylistImportSourceTruncation, XspfLocationFallbackIssue, admit_first_xspf_location,
};

/// Собирает whole-result draft и lossless bounded preview accounting.
pub(super) fn materialize_expansion(expansion: &LocalPlaylistExpansion) -> PlaylistImportDraft {
    let mut materializer = ImportDraftMaterializer::default();
    let entries = expansion.root_document().map_or_else(Vec::new, |document| {
        materializer.materialize_document(document)
    });
    materializer.issues.extend(
        expansion
            .issues()
            .iter()
            .map(|_| PlaylistImportIssue::new(PlaylistImportIssueKind::SourceRejectedEntry)),
    );
    if expansion.summary().omitted_diagnostics() > 0 {
        materializer.issues.push(PlaylistImportIssue::new(
            PlaylistImportIssueKind::DiagnosticPrefixTruncated,
        ));
    }
    let source_truncation = expansion
        .summary()
        .was_truncated()
        .then(|| PlaylistImportSourceTruncation::new(PlaylistImportRejectedCount::AtLeast(1)));
    PlaylistImportDraft::new(
        entries,
        materializer.issues,
        source_truncation,
        materializer.sensitive_durable_locator_count,
    )
}

/// Stateful DFS adapter хранит только preview accounting одного expansion result-а.
#[derive(Default)]
struct ImportDraftMaterializer {
    issues: Vec<PlaylistImportIssue>,
    sensitive_durable_locator_count: usize,
}

impl ImportDraftMaterializer {
    /// Каждый included document сохраняет собственные provenance и group boundaries.
    fn materialize_document(
        &mut self,
        document: &ExpandedLocalPlaylistDocument,
    ) -> Vec<PlaylistImportEntryDraft> {
        let direct_slots = document
            .entries()
            .iter()
            .enumerate()
            .map(|(source_index, entry)| {
                self.materialize_source_entry(document, source_index, entry)
            })
            .collect::<Vec<_>>();
        if document.format() != LocalPlaylistDocumentFormat::Xspf
            || document.xspf_groups().is_empty()
        {
            return direct_slots.into_iter().flatten().collect();
        }
        self.apply_xspf_groups(document, direct_slots)
    }

    /// Один source entry может раскрыться в несколько top-level nested entries.
    fn materialize_source_entry(
        &mut self,
        document: &ExpandedLocalPlaylistDocument,
        source_index: usize,
        entry: &ExpandedLocalPlaylistEntry,
    ) -> Vec<PlaylistImportEntryDraft> {
        match entry {
            ExpandedLocalPlaylistEntry::M3uItem(draft) => {
                self.note_sensitive_locator(draft.reopen_locator());
                vec![PlaylistImportEntryDraft::Single((**draft).clone())]
            }
            ExpandedLocalPlaylistEntry::XspfTrack(track) => self
                .materialize_xspf_track(document, source_index, track)
                .into_iter()
                .map(PlaylistImportEntryDraft::Single)
                .collect(),
            ExpandedLocalPlaylistEntry::IncludedDocument(included) => {
                self.materialize_document(included)
            }
            ExpandedLocalPlaylistEntry::UnexpandedInclude(include) => match include {
                UnexpandedLocalPlaylistInclude::M3uItem(draft) => {
                    self.issues.push(PlaylistImportIssue::new(
                        PlaylistImportIssueKind::SourceRejectedEntry,
                    ));
                    self.note_sensitive_locator(draft.reopen_locator());
                    vec![PlaylistImportEntryDraft::Single((**draft).clone())]
                }
                UnexpandedLocalPlaylistInclude::XspfTrack(track) => {
                    self.issues.push(PlaylistImportIssue::new(
                        PlaylistImportIssueKind::SourceRejectedEntry,
                    ));
                    self.materialize_xspf_track(document, source_index, track)
                        .into_iter()
                        .map(PlaylistImportEntryDraft::Single)
                        .collect()
                }
            },
        }
    }

    /// XSPF ordered alternatives проходят app-owned first-admissible registry.
    fn materialize_xspf_track(
        &mut self,
        document: &ExpandedLocalPlaylistDocument,
        source_index: usize,
        track: &XspfTrack,
    ) -> Option<PlaylistSingleImportDraft> {
        let provenance = document_provenance(document, source_index);
        let admission = admit_first_xspf_location(
            track.location_candidates(),
            xspf_track_metadata(track),
            provenance,
        );
        self.note_xspf_admission_issues(admission.issues());
        self.sensitive_durable_locator_count = self
            .sensitive_durable_locator_count
            .saturating_add(admission.sensitive_durable_locator_count());
        let draft = admission.into_draft();
        if draft.is_none() {
            self.issues.push(PlaylistImportIssue::new(
                PlaylistImportIssueKind::UnsupportedLocator,
            ));
        }
        draft
    }

    /// Non-overlapping XSPF ranges становятся first-class Compound только без nesting.
    fn apply_xspf_groups(
        &mut self,
        document: &ExpandedLocalPlaylistDocument,
        mut direct_slots: Vec<Vec<PlaylistImportEntryDraft>>,
    ) -> Vec<PlaylistImportEntryDraft> {
        let mut output = Vec::new();
        let mut source_index = 0usize;
        for group in document.xspf_groups() {
            let group_start = group.first_track().get() as usize - 1;
            while source_index < group_start {
                output.append(&mut direct_slots[source_index]);
                source_index += 1;
            }
            let group_end = group_start.saturating_add(group.track_count().get() as usize);
            let grouped =
                self.materialize_xspf_group(document, group, &direct_slots[group_start..group_end]);
            match grouped {
                Some(grouped) => output.push(PlaylistImportEntryDraft::Compound(grouped)),
                None => {
                    for slot in &mut direct_slots[group_start..group_end] {
                        output.append(slot);
                    }
                }
            }
            source_index = group_end;
        }
        while source_index < direct_slots.len() {
            output.append(&mut direct_slots[source_index]);
            source_index += 1;
        }
        output
    }

    /// Compound range не может скрыто flatten-ить nested compound/include topology.
    fn materialize_xspf_group(
        &mut self,
        document: &ExpandedLocalPlaylistDocument,
        group: &XspfGroup,
        slots: &[Vec<PlaylistImportEntryDraft>],
    ) -> Option<PlaylistCompoundImportDraft> {
        let mut parts = Vec::with_capacity(slots.len());
        for slot in slots {
            let [PlaylistImportEntryDraft::Single(single)] = slot.as_slice() else {
                self.issues.push(PlaylistImportIssue::new(
                    PlaylistImportIssueKind::SourceRejectedEntry,
                ));
                return None;
            };
            parts.push(single.clone());
        }
        let source_index = group.first_track().get() as usize - 1;
        let provenance = document_provenance(document, source_index);
        let root_admission = admit_first_xspf_location(
            std::slice::from_ref(group.root_location()),
            CachedPlaylistMetadata::new("Группа XSPF", PlaylistMediaKind::Unknown),
            provenance.clone(),
        );
        self.note_xspf_admission_issues(root_admission.issues());
        self.sensitive_durable_locator_count = self
            .sensitive_durable_locator_count
            .saturating_add(root_admission.sensitive_durable_locator_count());
        let root = root_admission.into_draft()?;
        match PlaylistCompoundImportDraft::new(
            root.reopen_locator().clone(),
            root.cached_metadata().clone(),
            provenance,
            parts,
        ) {
            Ok(compound) => Some(compound),
            Err(error) => {
                tracing::warn!(?error, "XSPF group не прошёл neutral compound boundary");
                self.issues.push(PlaylistImportIssue::new(
                    PlaylistImportIssueKind::SourceRejectedEntry,
                ));
                None
            }
        }
    }

    /// Registry detail остаётся typed, а preview показывает bounded category.
    fn note_xspf_admission_issues(&mut self, issues: &[XspfLocationFallbackIssue]) {
        self.issues.extend(
            issues
                .iter()
                .map(|_| PlaylistImportIssue::new(PlaylistImportIssueKind::UnsupportedLocator)),
        );
    }

    /// M3U draft уже содержит durable locator; app повторно не меняет admission.
    fn note_sensitive_locator(&mut self, locator: &DurableReopenLocator) {
        let Some(secret_url) = locator.expose_url_for_reopen() else {
            return;
        };
        let raw_url = secret_url.expose_secret_for_persistence();
        let sensitive = match crate::url_service_adapter::classify_startup_url(raw_url) {
            crate::url_service_adapter::StartupUrlClassification::Supported(locator) => {
                locator.requires_sensitive_persistence_acknowledgement()
            }
            crate::url_service_adapter::StartupUrlClassification::NotUrl
            | crate::url_service_adapter::StartupUrlClassification::Unsupported { .. } => true,
        };
        self.sensitive_durable_locator_count = self
            .sensitive_durable_locator_count
            .saturating_add(usize::from(sensitive));
    }
}

/// XSPF metadata остаётся hint-ом и не получает playback/open authority.
fn xspf_track_metadata(track: &XspfTrack) -> CachedPlaylistMetadata {
    let fallback = track.title().unwrap_or("Элемент XSPF").to_owned();
    let metadata = CachedPlaylistMetadata::new(fallback, PlaylistMediaKind::Unknown)
        .with_duration(track.duration_hint())
        .with_title(track.title().map(ToOwned::to_owned))
        .with_album(track.album().map(ToOwned::to_owned))
        .with_sequence(None, track.track_number(), None, None);
    match track.creator() {
        Some(creator) => metadata
            .with_artists(vec![creator.to_owned()])
            .expect("один XSPF creator не превышает bounded artists limit"),
        None => metadata,
    }
}

/// Provenance root хранит exact native document identity и one-based source ordinal.
fn document_provenance(
    document: &ExpandedLocalPlaylistDocument,
    source_index: usize,
) -> PlaylistImportProvenance {
    let source_kind = match document.format() {
        LocalPlaylistDocumentFormat::M3u => PlaylistImportSourceKind::M3u,
        LocalPlaylistDocumentFormat::M3u8 => PlaylistImportSourceKind::M3u8,
        LocalPlaylistDocumentFormat::Xspf => PlaylistImportSourceKind::Xspf,
    };
    let ordinal = source_index
        .checked_add(1)
        .and_then(|value| u32::try_from(value).ok())
        .and_then(NonZeroU32::new);
    PlaylistImportProvenance::new(
        DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(document.source_path()))),
        source_kind,
        ordinal,
    )
}
