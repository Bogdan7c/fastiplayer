//! Destructive queue mutations, detached active tombstone и controller-side Undo restore.

mod clear;
mod selection;
pub(crate) use clear::ControllerClearMediaResetCommit;

use std::collections::HashSet;
use std::sync::Arc;

use player_core::{ExactMediaTransportAction, ExactMediaTransportRequest};
use playlist_core::{
    AutomaticEndedIntent, AutomaticTraversalPlan, AutomaticTraversalStart, BulkRemoveError,
    BulkRemoveOutcome, PlaylistEntry, PlaylistEntryId, PlaylistItemId, PlaylistRemovalSnapshot,
    RemovalCurrentOutcome, RemovalSnapshotRestoreError, RemoveItemOutcome, RepeatMode,
};

use super::{
    ManualNavigationInvalidation, PlaylistController, PlaylistDirtySignal,
    PlaylistStructuralRevision,
};
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};
use crate::playlist_runtime::selection::PlaylistSelectionSnapshot;

#[cfg(test)]
mod tests;

/// Все v1 destructive actions имеют одну business semantics Undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerRemovalKind {
    Remove,
    Clear,
    RemoveOthers,
    RemoveSelected,
    RemoveUnselected,
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
    pub(crate) selection_before: PlaylistSelectionSnapshot,
    pub(crate) selection_after: PlaylistSelectionSnapshot,
    pub(crate) current_outcome: RemovalCurrentOutcome,
    pub(crate) dirty: PlaylistDirtySignal,
    /// Только Clear формирует exact полный reset текущего player media.
    pub(crate) media_reset_request: Option<ExactMediaTransportRequest>,
    pub(crate) active_lineage_at_removal: Option<ActiveMediaLineageId>,
    pub(crate) active_tombstone_lineage: Option<ActiveMediaLineageId>,
    pub(crate) active_tombstone_item_id: Option<PlaylistItemId>,
    pub(crate) manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
    pub(crate) pending_request_to_cancel: Option<MediaOpenRequestId>,
}

/// Typed action outcome не смешивает no-op, lock и revision failures.
#[derive(Debug)]
pub(crate) enum ControllerDestructiveRemovalOutcome {
    /// Heap indirection сохраняет error outcomes компактными; allocation бывает только на commit.
    Removed(Box<ControllerDestructiveRemoval>),
    NotFound {
        item_id: PlaylistItemId,
    },
    DuplicateItemId {
        item_id: PlaylistItemId,
    },
    DuplicateEntryId {
        entry_id: PlaylistEntryId,
    },
    EntryNotFound {
        entry_id: PlaylistEntryId,
    },
    InvalidRetainedItem {
        item_id: PlaylistItemId,
    },
    PartialCompoundSelection {
        compound_entry_id: playlist_core::PlaylistEntryId,
    },
    CompoundPartTarget {
        part_item_id: PlaylistItemId,
        compound_entry_id: playlist_core::PlaylistEntryId,
    },
    StaleStructuralRevision,
    StaleSelection,
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
    NotFound {
        item_id: PlaylistItemId,
    },
    CompoundPartTarget {
        part_item_id: PlaylistItemId,
        compound_entry_id: playlist_core::PlaylistEntryId,
    },
    NoChange,
    InstallCommitLinearizing,
    DomainRevisionExhausted,
}

/// Controller явно выбирает post-removal selection policy до domain commit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionAfterRemoval {
    /// Сохранить все surviving selected IDs, anchor и cursor.
    PreserveSurvivors,
    /// Снять selection и interaction cursor.
    Clear,
    /// Оставить единственный retained stable ID.
    SelectSingle(PlaylistEntryId),
    /// Перенести selection/focus на ближайшую surviving строку.
    SelectNearest,
}

/// Политика явно разделяет обычный detach строки и полный Clear lifecycle reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovedActiveMediaPolicy {
    /// Обычное удаление сохраняет playback и при необходимости создаёт tombstone.
    DetachRemovedPlaylistItem,
    /// Clear забывает любую active identity и требует exact reset её player instance.
    ResetCurrentMedia,
}

impl RemovalMutationError {
    fn into_outcome(self) -> ControllerDestructiveRemovalOutcome {
        match self {
            Self::NotFound { item_id } => ControllerDestructiveRemovalOutcome::NotFound { item_id },
            Self::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => ControllerDestructiveRemovalOutcome::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            },
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
        selected_entry_id: Option<PlaylistEntryId>,
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
        if self.queue.item(item_id).is_none() {
            return ControllerDestructiveRemovalOutcome::NotFound { item_id };
        }
        let removed_entry_id = PlaylistEntryId::Single(item_id);
        let selection_policy = if self.selection.snapshot().is_selected(removed_entry_id) {
            SelectionAfterRemoval::SelectNearest
        } else {
            SelectionAfterRemoval::PreserveSurvivors
        };
        self.commit_destructive_removal(
            ControllerRemovalKind::Remove,
            Arc::from([removed_entry_id]),
            selection_policy,
            RemovedActiveMediaPolicy::DetachRemovedPlaylistItem,
            move |controller| match controller
                .queue
                .remove(playlist_core::PlaylistEntryId::Single(item_id))
            {
                RemoveItemOutcome::Removed {
                    current_outcome, ..
                } => Ok(current_outcome),
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
                RemoveItemOutcome::CompoundPartTarget {
                    part_item_id,
                    compound_entry_id,
                } => Err(RemovalMutationError::CompoundPartTarget {
                    part_item_id,
                    compound_entry_id,
                }),
            },
        )
    }

    /// Remove Others сохраняет retained stable ID и делает один bulk commit.
    pub(crate) fn remove_other_items(
        &mut self,
        retained_item_id: PlaylistItemId,
    ) -> ControllerDestructiveRemovalOutcome {
        let retained_entry_id = playlist_core::PlaylistEntryId::Single(retained_item_id);
        if self.queue.top_level_entry(retained_entry_id).is_none() {
            return ControllerDestructiveRemovalOutcome::InvalidRetainedItem {
                item_id: retained_item_id,
            };
        }
        let removed_entry_ids = self
            .queue
            .iter_top_level_entry_ids()
            .filter(|entry_id| *entry_id != retained_entry_id)
            .collect::<Vec<_>>();
        self.commit_destructive_removal(
            ControllerRemovalKind::RemoveOthers,
            removed_entry_ids.into(),
            SelectionAfterRemoval::SelectSingle(retained_entry_id),
            RemovedActiveMediaPolicy::DetachRemovedPlaylistItem,
            |controller| match controller.queue.remove_others(retained_entry_id) {
                Ok(BulkRemoveOutcome::Removed {
                    current_outcome, ..
                }) => Ok(current_outcome),
                Ok(BulkRemoveOutcome::NoItemsRequested | BulkRemoveOutcome::NoMatchingItems) => {
                    Err(RemovalMutationError::NoChange)
                }
                Err(BulkRemoveError::InstallCommitLinearizing) => {
                    Err(RemovalMutationError::InstallCommitLinearizing)
                }
                Err(BulkRemoveError::CompoundPartTarget { .. }) => {
                    Err(RemovalMutationError::NoChange)
                }
                Err(
                    BulkRemoveError::StructuralRevisionExhausted
                    | BulkRemoveError::TraversalRevisionExhausted,
                ) => Err(RemovalMutationError::DomainRevisionExhausted),
            },
        )
    }

    /// Удаляет exact current selection одним domain `remove_batch` commit-ом.
    pub(crate) fn remove_selected_items(
        &mut self,
        entry_ids: Arc<[PlaylistEntryId]>,
        expected_structural_revision: PlaylistStructuralRevision,
    ) -> ControllerDestructiveRemovalOutcome {
        let domain_entry_ids =
            match self.preflight_exact_removal_entries(&entry_ids, expected_structural_revision) {
                Ok(entry_ids) => entry_ids,
                Err(outcome) => return outcome,
            };
        let selection = self.selection.snapshot();
        if selection.selected_count() != domain_entry_ids.len()
            || domain_entry_ids
                .iter()
                .any(|entry_id| !selection.is_selected(*entry_id))
        {
            return ControllerDestructiveRemovalOutcome::StaleSelection;
        }
        self.commit_destructive_removal(
            ControllerRemovalKind::RemoveSelected,
            entry_ids,
            SelectionAfterRemoval::SelectNearest,
            RemovedActiveMediaPolicy::DetachRemovedPlaylistItem,
            move |controller| match controller.queue.remove_batch(&domain_entry_ids) {
                Ok(BulkRemoveOutcome::Removed {
                    current_outcome, ..
                }) => Ok(current_outcome),
                Ok(BulkRemoveOutcome::NoItemsRequested | BulkRemoveOutcome::NoMatchingItems) => {
                    Err(RemovalMutationError::NoChange)
                }
                Err(BulkRemoveError::InstallCommitLinearizing) => {
                    Err(RemovalMutationError::InstallCommitLinearizing)
                }
                Err(BulkRemoveError::CompoundPartTarget { .. }) => {
                    Err(RemovalMutationError::NoChange)
                }
                Err(
                    BulkRemoveError::StructuralRevisionExhausted
                    | BulkRemoveError::TraversalRevisionExhausted,
                ) => Err(RemovalMutationError::DomainRevisionExhausted),
            },
        )
    }

    /// Удаляет exact complement current selection одним domain commit-ом.
    pub(crate) fn remove_unselected_items(
        &mut self,
        entry_ids: Arc<[PlaylistEntryId]>,
        expected_structural_revision: PlaylistStructuralRevision,
    ) -> ControllerDestructiveRemovalOutcome {
        let domain_entry_ids =
            match self.preflight_exact_removal_entries(&entry_ids, expected_structural_revision) {
                Ok(entry_ids) => entry_ids,
                Err(outcome) => return outcome,
            };
        let selection = self.selection.snapshot();
        if selection.selected_count() + domain_entry_ids.len() != self.queue.top_level_entry_count()
            || domain_entry_ids
                .iter()
                .any(|entry_id| selection.is_selected(*entry_id))
        {
            return ControllerDestructiveRemovalOutcome::StaleSelection;
        }
        self.commit_destructive_removal(
            ControllerRemovalKind::RemoveUnselected,
            entry_ids,
            SelectionAfterRemoval::PreserveSurvivors,
            RemovedActiveMediaPolicy::DetachRemovedPlaylistItem,
            move |controller| match controller.queue.remove_batch(&domain_entry_ids) {
                Ok(BulkRemoveOutcome::Removed {
                    current_outcome, ..
                }) => Ok(current_outcome),
                Ok(BulkRemoveOutcome::NoItemsRequested | BulkRemoveOutcome::NoMatchingItems) => {
                    Err(RemovalMutationError::NoChange)
                }
                Err(BulkRemoveError::InstallCommitLinearizing) => {
                    Err(RemovalMutationError::InstallCommitLinearizing)
                }
                Err(BulkRemoveError::CompoundPartTarget { .. }) => {
                    Err(RemovalMutationError::NoChange)
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
        removed_entry_ids: Arc<[PlaylistEntryId]>,
        selection_policy: SelectionAfterRemoval,
        active_media_policy: RemovedActiveMediaPolicy,
        mutate: impl FnOnce(
            &mut PlaylistController,
        ) -> Result<RemovalCurrentOutcome, RemovalMutationError>,
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

        let selection_before = self.selection.snapshot();
        let current_before = self.queue.traversal_current();
        let snapshot = self.queue.capture_removal_snapshot();
        let active_before = self.active_media;
        let removed_entry_ids: HashSet<_> = removed_entry_ids.iter().copied().collect();
        let mut removed_item_ids = HashSet::new();
        for entry in self
            .queue
            .iter_top_level_entries()
            .filter(|entry| removed_entry_ids.contains(&entry.entry_id()))
        {
            match entry {
                PlaylistEntry::Single(item) => {
                    removed_item_ids.insert(item.item_id());
                }
                PlaylistEntry::Compound(group) => {
                    removed_item_ids.extend(group.parts().map(|part| part.item().item_id()));
                }
            }
        }
        let fallback_index = self.nearest_survivor_index(&removed_entry_ids);
        let removed_active_item_id = active_before
            .and_then(ActiveMediaIdentity::item_id)
            .filter(|active_item_id| removed_item_ids.contains(active_item_id));
        let media_reset_request = match active_media_policy {
            RemovedActiveMediaPolicy::DetachRemovedPlaylistItem => None,
            RemovedActiveMediaPolicy::ResetCurrentMedia => {
                active_before.map(|active| ExactMediaTransportRequest {
                    media_instance_id: active.media_instance_id(),
                    action: ExactMediaTransportAction::ResetMedia,
                })
            }
        };
        let continuation = match active_media_policy {
            RemovedActiveMediaPolicy::DetachRemovedPlaylistItem => removed_active_item_id
                .filter(|removed_item_id| {
                    current_before.is_some_and(|current| current.item_id() == *removed_item_id)
                })
                .and_then(|_| self.capture_removal_continuation()),
            RemovedActiveMediaPolicy::ResetCurrentMedia => None,
        };

        let current_outcome = match mutate(self) {
            Ok(committed) => committed,
            Err(error) => return error.into_outcome(),
        };

        self.apply_selection_after_removal(
            selection_before.clone(),
            selection_policy,
            fallback_index,
        );
        self.runtime_errors
            .retain(|item_id, _| self.queue.item(*item_id).is_some());
        match active_media_policy {
            RemovedActiveMediaPolicy::DetachRemovedPlaylistItem => {
                if let (Some(active), Some(removed_item_id)) =
                    (active_before, removed_active_item_id)
                {
                    self.active_media = Some(active.detached());
                    self.detached_active_tombstone = Some(DetachedActiveTombstone {
                        removed_item_id,
                        active_lineage_id: active.lineage_id(),
                        continuation,
                    });
                }
            }
            RemovedActiveMediaPolicy::ResetCurrentMedia => {
                self.active_media = None;
                self.detached_active_tombstone = None;
                self.replacement_detached_disposition = None;
            }
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
        let selection_after = self.selection.snapshot();

        ControllerDestructiveRemovalOutcome::Removed(Box::new(ControllerDestructiveRemoval {
            kind,
            snapshot,
            selection_before,
            selection_after,
            current_outcome,
            dirty,
            media_reset_request,
            // Clear Undo восстанавливает только queue/selection и не reattach-ит playback.
            active_lineage_at_removal: match active_media_policy {
                RemovedActiveMediaPolicy::DetachRemovedPlaylistItem => {
                    active_before.map(ActiveMediaIdentity::lineage_id)
                }
                RemovedActiveMediaPolicy::ResetCurrentMedia => None,
            },
            // Undo reattach относится только к tombstone, созданному этой mutation.
            active_tombstone_lineage: match active_media_policy {
                RemovedActiveMediaPolicy::DetachRemovedPlaylistItem => removed_active_item_id
                    .and_then(|_| active_before.map(ActiveMediaIdentity::lineage_id)),
                RemovedActiveMediaPolicy::ResetCurrentMedia => None,
            },
            active_tombstone_item_id: match active_media_policy {
                RemovedActiveMediaPolicy::DetachRemovedPlaylistItem => removed_active_item_id,
                RemovedActiveMediaPolicy::ResetCurrentMedia => None,
            },
            manual_navigation_invalidation,
            pending_request_to_cancel,
        }))
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

    /// Находит insertion position ближайшей surviving строки относительно selection cursor.
    fn nearest_survivor_index(
        &self,
        removed_entry_ids: &HashSet<PlaylistEntryId>,
    ) -> Option<usize> {
        let focus_origin = self
            .selection
            .interaction_cursor()
            .filter(|entry_id| removed_entry_ids.contains(entry_id))
            .or_else(|| {
                self.queue
                    .iter_top_level_entry_ids()
                    .find(|entry_id| removed_entry_ids.contains(entry_id))
            })?;
        let origin_index = self
            .queue
            .iter_top_level_entry_ids()
            .position(|entry_id| entry_id == focus_origin)?;
        Some(
            self.queue
                .iter_top_level_entry_ids()
                .take(origin_index)
                .filter(|entry_id| !removed_entry_ids.contains(entry_id))
                .count(),
        )
    }

    /// Применяет заранее выбранную selection policy только после успешного domain commit-а.
    fn apply_selection_after_removal(
        &mut self,
        selection_before: PlaylistSelectionSnapshot,
        selection_policy: SelectionAfterRemoval,
        fallback_index: Option<usize>,
    ) {
        match selection_policy {
            SelectionAfterRemoval::PreserveSurvivors => {
                self.selection.restore(selection_before, &self.queue);
            }
            SelectionAfterRemoval::Clear => {
                self.selection
                    .replace_after_removal(HashSet::new(), None, None);
            }
            SelectionAfterRemoval::SelectSingle(entry_id) => {
                let selected_entry_ids = self
                    .queue
                    .top_level_entry(entry_id)
                    .map(|_| HashSet::from([entry_id]))
                    .unwrap_or_default();
                let cursor = (!selected_entry_ids.is_empty()).then_some(entry_id);
                self.selection
                    .replace_after_removal(selected_entry_ids, cursor, cursor);
            }
            SelectionAfterRemoval::SelectNearest => {
                let fallback_entry_id = fallback_index.and_then(|index| {
                    self.queue
                        .iter_top_level_entry_ids()
                        .nth(index)
                        .or_else(|| self.queue.iter_top_level_entry_ids().next_back())
                });
                let selected_entry_ids = fallback_entry_id
                    .map(|entry_id| HashSet::from([entry_id]))
                    .unwrap_or_default();
                self.selection.replace_after_removal(
                    selected_entry_ids,
                    fallback_entry_id,
                    fallback_entry_id,
                );
            }
        }
    }

    /// Undo обычного removal reattach-ит lineage, а Undo Clear восстанавливает только queue.
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

        self.selection
            .restore(removal.selection_before, &self.queue);
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
            selected_entry_id: self.selected_entry_id(),
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
