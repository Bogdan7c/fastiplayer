//! Canonical reorder boundary без знания о UI drag geometry.

use std::collections::HashSet;
use std::sync::Arc;

use playlist_core::{
    MoveItemIntent, MoveItemOutcome, MoveItemsOutcome, PlaylistEntryId, PlaylistItemId,
};

use super::{ManualNavigationInvalidation, PlaylistController};
use crate::playlist_runtime::PlaylistStructuralRevision;
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
    CompoundPartTarget {
        part_item_id: PlaylistItemId,
        compound_entry_id: PlaylistEntryId,
    },
    AnchorNotFound {
        anchor_entry_id: PlaylistEntryId,
    },
    CompoundPartAnchor {
        part_item_id: PlaylistItemId,
        compound_entry_id: PlaylistEntryId,
    },
    InstallCommitLinearizing,
    FatalInvariant,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    DomainRevisionExhausted,
}

/// Group move сохраняет exact request failures и один structural/dirty commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMoveItemsOutcome {
    Moved {
        item_count: usize,
        dirty: PlaylistDirtySignal,
        manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
    },
    NoItemsRequested,
    AlreadyInPlace {
        item_count: usize,
    },
    DuplicateEntryId {
        entry_id: PlaylistEntryId,
    },
    EntryNotFound {
        entry_id: PlaylistEntryId,
    },
    AnchorNotFound {
        anchor_entry_id: PlaylistEntryId,
    },
    CompoundPartAnchor {
        part_item_id: PlaylistItemId,
        compound_entry_id: PlaylistEntryId,
    },
    AnchorSelected {
        anchor_entry_id: PlaylistEntryId,
    },
    StaleStructuralRevision,
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

        match self
            .queue
            .move_item(PlaylistEntryId::Single(item_id), intent)
        {
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
            MoveItemOutcome::EntryNotFound {
                entry_id: PlaylistEntryId::Single(item_id),
            } => ControllerMoveItemOutcome::ItemNotFound { item_id },
            MoveItemOutcome::EntryNotFound { .. } => ControllerMoveItemOutcome::FatalInvariant,
            MoveItemOutcome::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => ControllerMoveItemOutcome::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            },
            MoveItemOutcome::AnchorNotFound { anchor_entry_id } => {
                ControllerMoveItemOutcome::AnchorNotFound { anchor_entry_id }
            }
            MoveItemOutcome::CompoundPartAnchor {
                part_item_id,
                compound_entry_id,
            } => ControllerMoveItemOutcome::CompoundPartAnchor {
                part_item_id,
                compound_entry_id,
            },
            MoveItemOutcome::InstallCommitLinearizing => {
                ControllerMoveItemOutcome::InstallCommitLinearizing
            }
            MoveItemOutcome::StructuralRevisionExhausted => {
                ControllerMoveItemOutcome::DomainRevisionExhausted
            }
        }
    }

    /// Exact group drop публикует максимум одну canonical/dirty mutation.
    pub(crate) fn move_items(
        &mut self,
        entry_ids: Arc<[PlaylistEntryId]>,
        intent: MoveItemIntent,
        expected_structural_revision: PlaylistStructuralRevision,
    ) -> ControllerMoveItemsOutcome {
        if self.fatal_invariant.is_some() {
            return ControllerMoveItemsOutcome::FatalInvariant;
        }
        if expected_structural_revision != self.structural_revision {
            return ControllerMoveItemsOutcome::StaleStructuralRevision;
        }
        let Some(next_dirty) = self.dirty_revision.checked_next() else {
            return ControllerMoveItemsOutcome::DirtyRevisionExhausted;
        };
        let Some(next_structural) = self.structural_revision.checked_next() else {
            return ControllerMoveItemsOutcome::StructuralRevisionExhausted;
        };

        let mut unique_entry_ids = HashSet::with_capacity(entry_ids.len());
        for entry_id in entry_ids.iter().copied() {
            if !unique_entry_ids.insert(entry_id) {
                return ControllerMoveItemsOutcome::DuplicateEntryId { entry_id };
            }
            if self.queue.top_level_entry(entry_id).is_none() {
                return ControllerMoveItemsOutcome::EntryNotFound { entry_id };
            }
        }

        match self.queue.move_items(&entry_ids, intent) {
            MoveItemsOutcome::Moved { item_count } => {
                let manual_navigation_invalidation =
                    self.invalidate_manual_navigation_after_structural_mutation();
                self.structural_revision = next_structural;
                let dirty = self.commit_dirty(next_dirty);
                self.publish_view(true);
                ControllerMoveItemsOutcome::Moved {
                    item_count,
                    dirty,
                    manual_navigation_invalidation,
                }
            }
            MoveItemsOutcome::NoItemsRequested => ControllerMoveItemsOutcome::NoItemsRequested,
            MoveItemsOutcome::AlreadyInPlace { item_count } => {
                ControllerMoveItemsOutcome::AlreadyInPlace { item_count }
            }
            MoveItemsOutcome::DuplicateEntryId { entry_id } => {
                ControllerMoveItemsOutcome::DuplicateEntryId { entry_id }
            }
            MoveItemsOutcome::EntryNotFound { entry_id } => {
                ControllerMoveItemsOutcome::EntryNotFound { entry_id }
            }
            MoveItemsOutcome::CompoundPartTarget { .. } => {
                ControllerMoveItemsOutcome::FatalInvariant
            }
            MoveItemsOutcome::AnchorNotFound { anchor_entry_id } => {
                ControllerMoveItemsOutcome::AnchorNotFound { anchor_entry_id }
            }
            MoveItemsOutcome::CompoundPartAnchor {
                part_item_id,
                compound_entry_id,
            } => ControllerMoveItemsOutcome::CompoundPartAnchor {
                part_item_id,
                compound_entry_id,
            },
            MoveItemsOutcome::AnchorSelected { anchor_entry_id } => {
                ControllerMoveItemsOutcome::AnchorSelected { anchor_entry_id }
            }
            MoveItemsOutcome::InstallCommitLinearizing => {
                ControllerMoveItemsOutcome::InstallCommitLinearizing
            }
            MoveItemsOutcome::StructuralRevisionExhausted => {
                ControllerMoveItemsOutcome::DomainRevisionExhausted
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use playlist_core::{
        AddPlaylistEntriesOutcome, CachedPlaylistMetadata, LocalLocator,
        PlaylistCompoundGroupDraft, PlaylistEntryDraft, PlaylistItemDraft, PlaylistLocator,
        PlaylistMediaKind,
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
        assert_eq!(controller.queue().iter_playable_ids().nth(3), Some(ids[0]));
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
            controller.move_item(
                ids[0],
                MoveItemIntent::Before(PlaylistEntryId::Single(ids[1])),
            ),
            ControllerMoveItemOutcome::AlreadyInPlace
        );
        assert_eq!(
            controller.move_item(
                ids[1],
                MoveItemIntent::Before(PlaylistEntryId::Single(ids[1])),
            ),
            ControllerMoveItemOutcome::AlreadyInPlace
        );
        assert_eq!(controller.dirty_revision(), dirty_before);
    }

    #[test]
    fn group_move_is_one_commit_and_preserves_arc_selection() {
        let mut controller = PlaylistController::new();
        let ids = match controller
            .append((0..5).map(draft).collect())
            .expect("append")
        {
            crate::playlist_runtime::controller::ControllerAppendOutcome::Added {
                item_ids,
                ..
            } => item_ids,
            crate::playlist_runtime::controller::ControllerAppendOutcome::NoItemsProvided => {
                panic!("expected items")
            }
        };
        let entry_ids = ids
            .iter()
            .copied()
            .map(PlaylistEntryId::Single)
            .collect::<Vec<_>>();
        let revision = controller.structural_revision;
        assert_eq!(
            controller.update_selection(crate::playlist_runtime::UpdateSelection::Replace {
                entry_id: entry_ids[1],
                structural_revision: revision,
            }),
            crate::playlist_runtime::UpdateSelectionOutcome::Updated
        );
        assert_eq!(
            controller.update_selection(crate::playlist_runtime::UpdateSelection::Toggle {
                entry_id: entry_ids[3],
                structural_revision: revision,
            }),
            crate::playlist_runtime::UpdateSelectionOutcome::Updated
        );
        let selection_before = controller.view_snapshot().selection().clone();
        let dirty_before = controller.dirty_revision();

        assert!(matches!(
            controller.move_items(
                Arc::from([entry_ids[3], entry_ids[1]]),
                MoveItemIntent::ToFront,
                revision,
            ),
            ControllerMoveItemsOutcome::Moved { item_count: 2, .. }
        ));
        assert_eq!(
            controller.queue().iter_playable_ids().collect::<Vec<_>>(),
            vec![ids[1], ids[3], ids[0], ids[2], ids[4]]
        );
        let selection = controller.view_snapshot();
        assert!(selection.selection().is_selected(entry_ids[1]));
        assert!(selection.selection().is_selected(entry_ids[3]));
        assert_eq!(
            selection.selection().interaction_cursor(),
            Some(entry_ids[3])
        );
        assert!(Arc::ptr_eq(
            selection_before.selected_entry_ids(),
            selection.selection().selected_entry_ids()
        ));
        assert_eq!(
            controller.dirty_revision(),
            dirty_before.checked_next().expect("one dirty revision")
        );

        let order_before = controller.queue().iter_playable_ids().collect::<Vec<_>>();
        assert_eq!(
            controller.move_items(
                Arc::from([entry_ids[1], entry_ids[3]]),
                MoveItemIntent::ToFront,
                controller.structural_revision,
            ),
            ControllerMoveItemsOutcome::AlreadyInPlace { item_count: 2 }
        );
        assert!(
            controller
                .queue()
                .iter_playable_ids()
                .eq(order_before.iter().copied())
        );
        assert_eq!(
            controller.move_items(
                Arc::from([entry_ids[1], entry_ids[3]]),
                MoveItemIntent::ToBack,
                revision,
            ),
            ControllerMoveItemsOutcome::StaleStructuralRevision
        );
    }

    #[test]
    fn subordinate_part_identity_is_rejected_and_explicit_compound_moves_once() {
        let mut controller = PlaylistController::new();
        let compound = PlaylistCompoundGroupDraft::new(
            PlaylistLocator::Local(LocalLocator::Native(PathBuf::from("album"))),
            CachedPlaylistMetadata::new("album", PlaylistMediaKind::Audio),
            vec![draft(1), draft(2)],
        )
        .expect("compound requires parts");
        let AddPlaylistEntriesOutcome::Added(allocated) = controller
            .queue
            .append_entries(vec![
                PlaylistEntryDraft::Single(draft(0)),
                PlaylistEntryDraft::Compound(compound),
            ])
            .expect("mixed append")
        else {
            panic!("fixture append is non-empty");
        };
        let entry_ids = allocated.iter_entry_ids().collect::<Vec<_>>();
        let item_ids = allocated.iter_playable_item_ids().collect::<Vec<_>>();
        let revision = controller.structural_revision;
        let order_before = controller
            .queue
            .iter_top_level_entry_ids()
            .collect::<Vec<_>>();

        assert_eq!(
            controller.move_items(
                Arc::from([PlaylistEntryId::Single(item_ids[1])]),
                MoveItemIntent::ToFront,
                revision,
            ),
            ControllerMoveItemsOutcome::EntryNotFound {
                entry_id: PlaylistEntryId::Single(item_ids[1]),
            }
        );
        assert_eq!(
            controller
                .queue
                .iter_top_level_entry_ids()
                .collect::<Vec<_>>(),
            order_before
        );

        assert!(matches!(
            controller.move_items(Arc::from([entry_ids[1]]), MoveItemIntent::ToFront, revision,),
            ControllerMoveItemsOutcome::Moved { item_count: 1, .. }
        ));
        assert_eq!(
            controller
                .queue
                .iter_top_level_entry_ids()
                .collect::<Vec<_>>(),
            [entry_ids[1], entry_ids[0]]
        );
    }
}
