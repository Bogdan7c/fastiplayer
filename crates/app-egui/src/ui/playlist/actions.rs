//! Typed actions read-only egui snapshot-а.

use std::fmt;

use playlist_core::{MoveItemIntent, PlaylistItemId, SortCanonicalQueue};

use crate::playlist_runtime::{PlaylistGoCurrentTarget, PlaylistProgressCancelScope};

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
    Select(PlaylistItemId),
    Play(PlaylistItemId),
    Remove(PlaylistItemId),
    RemoveOthers(PlaylistItemId),
    Move {
        item_id: PlaylistItemId,
        intent: MoveItemIntent,
    },
    AddFiles,
    OpenUrlEditor,
    UpdateUrlDraft(PlaylistUrlDraftText),
    SubmitUrl,
    CancelUrlEditor,
    Clear,
    Sort(SortCanonicalQueue),
    CancelProgress(PlaylistProgressCancelScope),
    CancelNavigation,
    RetrySave,
    GoCurrent(PlaylistGoCurrentTarget),
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
