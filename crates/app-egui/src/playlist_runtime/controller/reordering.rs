//! Canonical reorder boundary без знания о UI drag geometry.

use playlist_core::{MoveItemIntent, MoveItemOutcome, PlaylistItemId};

use super::{ManualNavigationInvalidation, PlaylistController};
use crate::playlist_runtime::view::PlaylistDirtySignal;

/// Typed controller outcome не смешивает no-op, stale anchor и revision failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMoveItemOutcome {
    Moved {
        dirty: PlaylistDirtySignal,
        manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
    },
    AlreadyInPlace,
    ItemNotFound {
        item_id: PlaylistItemId,
    },
    AnchorNotFound {
        anchor_item_id: PlaylistItemId,
    },
    InstallCommitLinearizing,
    FatalInvariant,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    DomainRevisionExhausted,
}

impl PlaylistController {
    /// Одна drop-команда публикует не больше одной canonical/dirty mutation.
    pub(crate) fn move_item(
        &mut self,
        item_id: PlaylistItemId,
        intent: MoveItemIntent,
    ) -> ControllerMoveItemOutcome {
        if self.fatal_invariant.is_some() {
            return ControllerMoveItemOutcome::FatalInvariant;
        }
        let Some(next_dirty) = self.dirty_revision.checked_next() else {
            return ControllerMoveItemOutcome::DirtyRevisionExhausted;
        };
        let Some(next_structural) = self.structural_revision.checked_next() else {
            return ControllerMoveItemOutcome::StructuralRevisionExhausted;
        };

        match self.queue.move_item(item_id, intent) {
            MoveItemOutcome::Moved { .. } => {
                let manual_navigation_invalidation =
                    self.invalidate_manual_navigation_after_structural_mutation();
                self.structural_revision = next_structural;
                let dirty = self.commit_dirty(next_dirty);
                self.publish_view(true);
                ControllerMoveItemOutcome::Moved {
                    dirty,
                    manual_navigation_invalidation,
                }
            }
            MoveItemOutcome::AlreadyInPlace { .. } => ControllerMoveItemOutcome::AlreadyInPlace,
            MoveItemOutcome::ItemNotFound { item_id } => {
                ControllerMoveItemOutcome::ItemNotFound { item_id }
            }
            MoveItemOutcome::AnchorNotFound { anchor_item_id } => {
                ControllerMoveItemOutcome::AnchorNotFound { anchor_item_id }
            }
            MoveItemOutcome::InstallCommitLinearizing => {
                ControllerMoveItemOutcome::InstallCommitLinearizing
            }
            MoveItemOutcome::StructuralRevisionExhausted => {
                ControllerMoveItemOutcome::DomainRevisionExhausted
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind,
    };

    use super::*;

    fn draft(index: usize) -> PlaylistItemDraft {
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(format!("{index}.mp3"))),
            None,
            CachedPlaylistMetadata::new(format!("{index}.mp3"), PlaylistMediaKind::Audio),
        )
    }

    #[test]
    fn move_is_one_mutation_and_preserves_identity_owned_state() {
        let mut controller = PlaylistController::new();
        let ids = match controller.append((0..4).map(draft).collect()).unwrap() {
            super::super::ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
            super::super::ControllerAppendOutcome::NoItemsProvided => {
                panic!("fixture is non-empty")
            }
        };
        controller.select_row(Some(ids[2]));
        controller.queue.enable_shuffle().unwrap();
        let shuffle_before = controller.queue.shuffle_traversal_snapshot();
        let dirty_before = controller.dirty_revision();

        assert!(matches!(
            controller.move_item(ids[0], MoveItemIntent::ToBack),
            ControllerMoveItemOutcome::Moved { .. }
        ));
        assert_eq!(controller.selected_item_id(), Some(ids[2]));
        assert_eq!(controller.queue().items()[3].item_id(), ids[0]);
        assert_eq!(
            controller.queue.shuffle_traversal_snapshot(),
            shuffle_before
        );
        assert_ne!(controller.dirty_revision(), dirty_before);
    }

    #[test]
    fn adjacent_and_self_targets_are_noop_without_dirty_revision() {
        let mut controller = PlaylistController::new();
        let ids = match controller.append((0..3).map(draft).collect()).unwrap() {
            super::super::ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
            super::super::ControllerAppendOutcome::NoItemsProvided => {
                panic!("fixture is non-empty")
            }
        };
        let dirty_before = controller.dirty_revision();

        assert_eq!(
            controller.move_item(ids[0], MoveItemIntent::Before(ids[1])),
            ControllerMoveItemOutcome::AlreadyInPlace
        );
        assert_eq!(
            controller.move_item(ids[1], MoveItemIntent::Before(ids[1])),
            ControllerMoveItemOutcome::AlreadyInPlace
        );
        assert_eq!(controller.dirty_revision(), dirty_before);
    }
}
