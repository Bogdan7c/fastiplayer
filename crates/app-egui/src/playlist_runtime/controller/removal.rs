//! Destructive queue mutations, detached active tombstone и controller-side Undo restore.

use playlist_core::{
    AutomaticEndedIntent, AutomaticTraversalPlan, AutomaticTraversalStart, BulkRemoveError,
    BulkRemoveOutcome, ClearQueueOutcome, PlaylistItemId, PlaylistRemovalSnapshot,
    RemovalCurrentOutcome, RemovalSnapshotRestoreError, RemoveItemOutcome, RepeatMode,
};

use super::{ManualNavigationInvalidation, PlaylistController, PlaylistDirtySignal};
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};

#[cfg(test)]
mod tests;

/// Все v1 destructive actions имеют одну business semantics Undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerRemovalKind {
    Remove,
    Clear,
    RemoveOthers,
}

/// Runtime-only detached active identity и domain-owned continuation plan.
pub(crate) struct DetachedActiveTombstone {
    removed_item_id: PlaylistItemId,
    active_lineage_id: ActiveMediaLineageId,
    continuation: Option<Box<AutomaticTraversalPlan>>,
}

impl std::fmt::Debug for DetachedActiveTombstone {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DetachedActiveTombstone")
            .field("removed_item_id", &self.removed_item_id)
            .field("active_lineage_id", &self.active_lineage_id)
            .field("has_continuation", &self.continuation.is_some())
            .finish()
    }
}

impl DetachedActiveTombstone {
    pub(super) const fn removed_item_id(&self) -> PlaylistItemId {
        self.removed_item_id
    }

    pub(super) const fn active_lineage_id(&self) -> ActiveMediaLineageId {
        self.active_lineage_id
    }

    pub(super) fn take_continuation(&mut self) -> Option<Box<AutomaticTraversalPlan>> {
        self.continuation.take()
    }
}

/// Успешный removal отдаёт runtime owner-у immutable pre-mutation snapshot.
#[derive(Debug, Clone)]
pub(crate) struct ControllerDestructiveRemoval {
    pub(crate) kind: ControllerRemovalKind,
    pub(crate) snapshot: PlaylistRemovalSnapshot,
    pub(crate) selected_item_id_before: Option<PlaylistItemId>,
    pub(crate) selected_item_id_after: Option<PlaylistItemId>,
    pub(crate) current_outcome: RemovalCurrentOutcome,
    pub(crate) dirty: PlaylistDirtySignal,
    pub(crate) active_lineage_at_removal: Option<ActiveMediaLineageId>,
    pub(crate) active_tombstone_lineage: Option<ActiveMediaLineageId>,
    pub(crate) active_tombstone_item_id: Option<PlaylistItemId>,
    pub(crate) manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
    pub(crate) pending_request_to_cancel: Option<MediaOpenRequestId>,
}

/// Typed action outcome не смешивает no-op, lock и revision failures.
#[derive(Debug)]
pub(crate) enum ControllerDestructiveRemovalOutcome {
    Removed(ControllerDestructiveRemoval),
    NotFound { item_id: PlaylistItemId },
    InvalidRetainedItem { item_id: PlaylistItemId },
    NoChange,
    InstallCommitLinearizing,
    FatalInvariant,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    DomainRevisionExhausted,
}

/// Внутренний mutation error остаётся маленьким и не несёт success snapshot variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalMutationError {
    NotFound { item_id: PlaylistItemId },
    NoChange,
    InstallCommitLinearizing,
    DomainRevisionExhausted,
}

impl RemovalMutationError {
    fn into_outcome(self) -> ControllerDestructiveRemovalOutcome {
        match self {
            Self::NotFound { item_id } => ControllerDestructiveRemovalOutcome::NotFound { item_id },
            Self::NoChange => ControllerDestructiveRemovalOutcome::NoChange,
            Self::InstallCommitLinearizing => {
                ControllerDestructiveRemovalOutcome::InstallCommitLinearizing
            }
            Self::DomainRevisionExhausted => {
                ControllerDestructiveRemovalOutcome::DomainRevisionExhausted
            }
        }
    }
}

/// Undo либо восстанавливает snapshot целиком, либо typed-отвергается до mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerRemovalUndoOutcome {
    Restored {
        selected_item_id: Option<PlaylistItemId>,
        reattached_active: bool,
        dirty: PlaylistDirtySignal,
    },
    ActiveLineageChanged,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    Domain(RemovalSnapshotRestoreError),
}

/// D72 rebind принимает только exact прежнюю active identity и отвергает stale completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerActiveMediaRebindOutcome {
    /// Новый player instance привязан к прежней app-owned lineage.
    Rebound { active_media: ActiveMediaIdentity },
    /// Текущая identity уже отсутствует либо была заменена более новым lifecycle result.
    Stale {
        current_active_media: Option<ActiveMediaIdentity>,
    },
}

impl PlaylistController {
    /// Remove/Delete применяет D47 и при необходимости создаёт active tombstone.
    pub(crate) fn remove_item(
        &mut self,
        item_id: PlaylistItemId,
    ) -> ControllerDestructiveRemovalOutcome {
        let Some(removed_index) = self
            .queue
            .items()
            .iter()
            .position(|item| item.item_id() == item_id)
        else {
            return ControllerDestructiveRemovalOutcome::NotFound { item_id };
        };
        self.commit_destructive_removal(
            ControllerRemovalKind::Remove,
            Some(item_id),
            move |controller| {
                let outcome = controller.queue.remove(item_id);
                match outcome {
                    RemoveItemOutcome::Removed {
                        current_outcome, ..
                    } => Ok((current_outcome, Some(removed_index))),
                    RemoveItemOutcome::InstallCommitLinearizing => {
                        Err(RemovalMutationError::InstallCommitLinearizing)
                    }
                    RemoveItemOutcome::StructuralRevisionExhausted
                    | RemoveItemOutcome::TraversalRevisionExhausted => {
                        Err(RemovalMutationError::DomainRevisionExhausted)
                    }
                    RemoveItemOutcome::NotFound { .. } => {
                        Err(RemovalMutationError::NotFound { item_id })
                    }
                }
            },
        )
    }

    /// Clear немедленно оставляет active media detached и не посылает player Stop.
    pub(crate) fn clear_queue(&mut self) -> ControllerDestructiveRemovalOutcome {
        self.commit_destructive_removal(ControllerRemovalKind::Clear, None, |controller| {
            match controller.queue.clear() {
                ClearQueueOutcome::Cleared {
                    current_outcome, ..
                } => Ok((current_outcome, None)),
                ClearQueueOutcome::AlreadyEmpty => Err(RemovalMutationError::NoChange),
                ClearQueueOutcome::InstallCommitLinearizing => {
                    Err(RemovalMutationError::InstallCommitLinearizing)
                }
                ClearQueueOutcome::StructuralRevisionExhausted
                | ClearQueueOutcome::TraversalRevisionExhausted => {
                    Err(RemovalMutationError::DomainRevisionExhausted)
                }
            }
        })
    }

    /// Remove Others сохраняет retained stable ID и делает один bulk commit.
    pub(crate) fn remove_other_items(
        &mut self,
        retained_item_id: PlaylistItemId,
    ) -> ControllerDestructiveRemovalOutcome {
        if self.queue.item(retained_item_id).is_none() {
            return ControllerDestructiveRemovalOutcome::InvalidRetainedItem {
                item_id: retained_item_id,
            };
        }
        self.commit_destructive_removal(
            ControllerRemovalKind::RemoveOthers,
            Some(retained_item_id),
            |controller| match controller.queue.remove_others(retained_item_id) {
                Ok(BulkRemoveOutcome::Removed {
                    current_outcome, ..
                }) => Ok((current_outcome, None)),
                Ok(BulkRemoveOutcome::NoItemsRequested | BulkRemoveOutcome::NoMatchingItems) => {
                    Err(RemovalMutationError::NoChange)
                }
                Err(BulkRemoveError::InstallCommitLinearizing) => {
                    Err(RemovalMutationError::InstallCommitLinearizing)
                }
                Err(
                    BulkRemoveError::StructuralRevisionExhausted
                    | BulkRemoveError::TraversalRevisionExhausted,
                ) => Err(RemovalMutationError::DomainRevisionExhausted),
            },
        )
    }

    /// Общая transaction сначала захватывает snapshot, затем публикует ровно одну mutation.
    fn commit_destructive_removal(
        &mut self,
        kind: ControllerRemovalKind,
        retained_or_removed_item_id: Option<PlaylistItemId>,
        mutate: impl FnOnce(
            &mut PlaylistController,
        )
            -> Result<(RemovalCurrentOutcome, Option<usize>), RemovalMutationError>,
    ) -> ControllerDestructiveRemovalOutcome {
        if self.fatal_invariant.is_some() {
            return ControllerDestructiveRemovalOutcome::FatalInvariant;
        }
        if self
            .install_phase()
            .is_some_and(|phase| phase != super::ControllerInstallPhase::AwaitingReady)
        {
            return ControllerDestructiveRemovalOutcome::InstallCommitLinearizing;
        }
        let Some(next_dirty) = self.dirty_revision.checked_next() else {
            return ControllerDestructiveRemovalOutcome::DirtyRevisionExhausted;
        };
        let Some(next_structural) = self.structural_revision.checked_next() else {
            return ControllerDestructiveRemovalOutcome::StructuralRevisionExhausted;
        };

        let selected_before = self.selected_item_id;
        let current_before = self.queue.traversal_current();
        let snapshot = self.queue.capture_removal_snapshot();
        let active_before = self.active_media;
        let removed_active_item_id = active_before.and_then(ActiveMediaIdentity::item_id).filter(
            |active_item_id| match kind {
                ControllerRemovalKind::Remove => {
                    Some(*active_item_id) == retained_or_removed_item_id
                }
                ControllerRemovalKind::Clear => true,
                ControllerRemovalKind::RemoveOthers => {
                    Some(*active_item_id) != retained_or_removed_item_id
                }
            },
        );
        let continuation = if kind == ControllerRemovalKind::Clear {
            None
        } else {
            removed_active_item_id
                .filter(|removed_item_id| {
                    current_before.is_some_and(|current| current.item_id() == *removed_item_id)
                })
                .and_then(|_| self.capture_removal_continuation())
        };

        let (current_outcome, removed_index) = match mutate(self) {
            Ok(committed) => committed,
            Err(error) => return error.into_outcome(),
        };

        self.apply_selection_after_removal(kind, retained_or_removed_item_id, removed_index);
        self.runtime_errors
            .retain(|item_id, _| self.queue.item(*item_id).is_some());
        if let (Some(active), Some(removed_item_id)) = (active_before, removed_active_item_id) {
            self.active_media = Some(active.detached());
            self.detached_active_tombstone = Some(DetachedActiveTombstone {
                removed_item_id,
                active_lineage_id: active.lineage_id(),
                continuation,
            });
        }
        let pending_request_to_cancel = match self.retire_awaiting_install_for_removal() {
            Ok(request_id) => request_id,
            Err(_) => return ControllerDestructiveRemovalOutcome::FatalInvariant,
        };
        let manual_navigation_invalidation =
            self.invalidate_manual_navigation_after_structural_mutation();
        self.structural_revision = next_structural;
        let dirty = self.commit_dirty(next_dirty);
        self.publish_view(true);

        ControllerDestructiveRemovalOutcome::Removed(ControllerDestructiveRemoval {
            kind,
            snapshot,
            selected_item_id_before: selected_before,
            selected_item_id_after: self.selected_item_id,
            current_outcome,
            dirty,
            active_lineage_at_removal: active_before.map(ActiveMediaIdentity::lineage_id),
            // Undo reattach относится только к tombstone, созданному этой mutation.
            active_tombstone_lineage: removed_active_item_id
                .and_then(|_| active_before.map(ActiveMediaIdentity::lineage_id)),
            active_tombstone_item_id: removed_active_item_id,
            manual_navigation_invalidation,
            pending_request_to_cancel,
        })
    }

    fn capture_removal_continuation(&self) -> Option<Box<AutomaticTraversalPlan>> {
        let repeat_mode = match self.repeat_mode {
            // RepeatOne не имеет права replay-ить удалённую active identity.
            RepeatMode::RepeatOne => RepeatMode::StopAtEnd,
            repeat_mode => repeat_mode,
        };
        match self
            .queue
            .begin_automatic_traversal(AutomaticEndedIntent::new(repeat_mode))
        {
            AutomaticTraversalStart::OpenItem { plan, .. } => Some(plan),
            AutomaticTraversalStart::ReplayCurrent { .. } | AutomaticTraversalStart::Stop(_) => {
                None
            }
        }
    }

    fn apply_selection_after_removal(
        &mut self,
        kind: ControllerRemovalKind,
        retained_or_removed_item_id: Option<PlaylistItemId>,
        removed_index: Option<usize>,
    ) {
        match kind {
            ControllerRemovalKind::Clear => self.selected_item_id = None,
            ControllerRemovalKind::RemoveOthers => {
                self.selected_item_id = retained_or_removed_item_id;
            }
            ControllerRemovalKind::Remove => {
                if self.selected_item_id == retained_or_removed_item_id {
                    self.selected_item_id = removed_index.and_then(|index| {
                        self.queue
                            .items()
                            .get(index)
                            .or_else(|| self.queue.items().last())
                            .map(|item| item.item_id())
                    });
                }
            }
        }
    }

    /// Matching-lineage Undo reattach-ит текущий player instance без reopen.
    pub(crate) fn restore_destructive_removal(
        &mut self,
        removal: ControllerDestructiveRemoval,
    ) -> ControllerRemovalUndoOutcome {
        if let Some(expected_lineage) = removal.active_tombstone_lineage
            && self.active_media.map(ActiveMediaIdentity::lineage_id) != Some(expected_lineage)
        {
            return ControllerRemovalUndoOutcome::ActiveLineageChanged;
        }
        let Some(next_dirty) = self.dirty_revision.checked_next() else {
            return ControllerRemovalUndoOutcome::DirtyRevisionExhausted;
        };
        let Some(next_structural) = self.structural_revision.checked_next() else {
            return ControllerRemovalUndoOutcome::StructuralRevisionExhausted;
        };
        if let Err(error) = self.queue.restore_removal_snapshot(removal.snapshot) {
            return ControllerRemovalUndoOutcome::Domain(error);
        }

        self.selected_item_id = removal
            .selected_item_id_before
            .filter(|item_id| self.queue.item(*item_id).is_some());
        let reattached_active = match (
            removal.active_tombstone_lineage,
            removal.active_tombstone_item_id,
            self.active_media,
        ) {
            (Some(lineage), Some(item_id), Some(active)) if active.lineage_id() == lineage => {
                self.active_media = Some(active.reattached(item_id));
                self.detached_active_tombstone = None;
                true
            }
            _ => false,
        };
        self.structural_revision = next_structural;
        let dirty = self.commit_dirty(next_dirty);
        self.publish_view(true);
        ControllerRemovalUndoOutcome::Restored {
            selected_item_id: self.selected_item_id,
            reattached_active,
            dirty,
        }
    }

    /// Successful Installed другой lineage освобождает tombstone ровно один раз.
    pub(super) fn release_detached_tombstone_for_new_lineage(
        &mut self,
        installed_active: ActiveMediaIdentity,
    ) {
        if self
            .detached_active_tombstone
            .as_ref()
            .is_some_and(|tombstone| tombstone.active_lineage_id() != installed_active.lineage_id())
        {
            self.detached_active_tombstone = None;
        }
    }

    /// D72 boundary обновляет exact instance/binding той же lineage без reattach.
    pub(crate) fn rebind_active_media_same_lineage(
        &mut self,
        expected_active_media: ActiveMediaIdentity,
        media_instance_id: player_core::MediaInstanceId,
        binding_generation: crate::playlist_runtime::PlaylistBindingGeneration,
    ) -> ControllerActiveMediaRebindOutcome {
        if self.active_media != Some(expected_active_media) {
            return ControllerActiveMediaRebindOutcome::Stale {
                current_active_media: self.active_media,
            };
        }
        let rebound_active = expected_active_media.rebound(media_instance_id, binding_generation);
        self.active_media = Some(rebound_active);
        self.publish_view(false);
        ControllerActiveMediaRebindOutcome::Rebound {
            active_media: rebound_active,
        }
    }

    /// Process shutdown release не меняет queue/dirty state.
    pub(crate) fn release_detached_tombstone_for_shutdown(&mut self) {
        self.detached_active_tombstone = None;
    }
}
