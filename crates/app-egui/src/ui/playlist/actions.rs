//! Typed actions read-only egui snapshot-а.

use std::fmt;
use std::sync::Arc;

use playlist_core::{MoveItemIntent, PlaylistEntryId, PlaylistItemId, SortCanonicalQueue};

use crate::playlist_runtime::{
    PlaylistExportRequest, PlaylistGoCurrentTarget, PlaylistImportIntent, PlaylistImportPreviewId,
    PlaylistStructuralRevision, UpdateSelection,
};

/// Exact selected IDs для одного bulk removal commit-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoveSelected {
    entry_ids: Arc<[PlaylistEntryId]>,
    structural_revision: PlaylistStructuralRevision,
}

impl RemoveSelected {
    /// Captures revision-stable selected IDs после explicit UI event.
    pub(crate) fn new(
        entry_ids: Arc<[PlaylistEntryId]>,
        structural_revision: PlaylistStructuralRevision,
    ) -> Self {
        Self {
            entry_ids,
            structural_revision,
        }
    }

    /// Передаёт exact action payload runtime owner-у.
    pub(crate) fn into_parts(self) -> (Arc<[PlaylistEntryId]>, PlaylistStructuralRevision) {
        (self.entry_ids, self.structural_revision)
    }
}

/// Exact unselected IDs для атомарного `remove_batch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoveUnselected {
    entry_ids: Arc<[PlaylistEntryId]>,
    structural_revision: PlaylistStructuralRevision,
}

impl RemoveUnselected {
    /// Captures revision-stable complement selection после explicit UI event.
    pub(crate) fn new(
        entry_ids: Arc<[PlaylistEntryId]>,
        structural_revision: PlaylistStructuralRevision,
    ) -> Self {
        Self {
            entry_ids,
            structural_revision,
        }
    }

    /// Передаёт exact action payload runtime owner-у.
    pub(crate) fn into_parts(self) -> (Arc<[PlaylistEntryId]>, PlaylistStructuralRevision) {
        (self.entry_ids, self.structural_revision)
    }
}

/// Exact group drag payload с typed insertion intent и capture revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveItems {
    entry_ids: Arc<[PlaylistEntryId]>,
    intent: MoveItemIntent,
    structural_revision: PlaylistStructuralRevision,
}

impl MoveItems {
    /// Captures stable IDs once at drag start and one resolved intent at drop.
    pub(crate) fn new(
        entry_ids: Arc<[PlaylistEntryId]>,
        intent: MoveItemIntent,
        structural_revision: PlaylistStructuralRevision,
    ) -> Self {
        Self {
            entry_ids,
            intent,
            structural_revision,
        }
    }

    /// Передаёт exact action payload runtime owner-у без positional bool.
    pub(crate) fn into_parts(
        self,
    ) -> (
        Arc<[PlaylistEntryId]>,
        MoveItemIntent,
        PlaylistStructuralRevision,
    ) {
        (self.entry_ids, self.intent, self.structural_revision)
    }
}

/// Raw URL хранится в отдельном типе с redacted `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PlaylistUrlDraftText(String);

impl PlaylistUrlDraftText {
    pub(crate) fn new(text: String) -> Self {
        Self(text)
    }

    pub(crate) fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for PlaylistUrlDraftText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaylistUrlDraftText(<redacted>)")
    }
}

/// UI описывает намерение; runtime/controller применяет его после egui render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistAction {
    UpdateSelection(UpdateSelection),
    Play(PlaylistItemId),
    RemoveSelected(RemoveSelected),
    RemoveUnselected(RemoveUnselected),
    MoveItems(MoveItems),
    AddFiles,
    /// Запускает single-root import dialog с явным append/replace intent.
    StartImport(PlaylistImportIntent),
    /// Запускает export с explicit scope/format до save dialog.
    StartExport(PlaylistExportRequest),
    /// Продолжает exact staged preview; stale generation не оживляется.
    ContinueImport(PlaylistImportPreviewId),
    /// Отменяет exact staged preview без queue/player mutation.
    CancelImport(PlaylistImportPreviewId),
    OpenUrlEditor,
    UpdateUrlDraft(PlaylistUrlDraftText),
    SubmitUrl,
    CancelUrlEditor,
    Clear,
    Sort(SortCanonicalQueue),
    CancelNavigation,
    RetrySave,
    GoCurrent(PlaylistGoCurrentTarget),
    /// Отменяет authoritative последнюю destructive removal через runtime owner.
    UndoRemoval,
    UrlFocusRestored,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_action_debug_is_redacted() {
        let action = PlaylistAction::UpdateUrlDraft(PlaylistUrlDraftText::new(
            "https://user:secret@example.test/?token=raw".to_string(),
        ));
        let debug = format!("{action:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("token"));
        assert!(debug.contains("redacted"));
    }
}
