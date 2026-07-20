//! Единственный process-lifetime D40/D46 removal Undo slot.

use std::sync::Arc;
use std::time::{Duration, Instant};

use player_core::{MediaInstallRequestId, MediaInstanceId};
use playlist_core::{PlaylistItemId, RemovalCurrentOutcome};

use super::controller::{
    ControllerDestructiveRemoval, ControllerDestructiveRemovalOutcome, ControllerRemovalKind,
    ControllerRemovalUndoOutcome, ControllerTerminalDrain, ManualNavigationInvalidation,
    PlaylistControllerInvariantViolation,
};
use super::identity::ActiveMediaIdentity;
use super::identity::ActiveMediaLineageId;
use super::view::{PlaylistDirtyRevision, PlaylistDirtySignal, PlaylistStructuralRevision};
use super::{PlaylistBindingGeneration, PlaylistRuntime};
use crate::media_open::{CancellationDispatchOutcome, MediaOpenCommandError, MediaOpenRequestId};

#[cfg(test)]
mod tests;

/// Пользовательское окно Undo фиксировано domain-решением D40.
pub(crate) const REMOVAL_UNDO_WINDOW: Duration = Duration::from_secs(8);

/// Runtime-only transaction; snapshot не попадает в store/serde DTO.
pub(super) struct RemovalUndoState {
    removal: ControllerDestructiveRemoval,
    expected_dirty_revision: PlaylistDirtyRevision,
    deadline: Instant,
}

impl RemovalUndoState {
    /// Lineage correlation нужна lifecycle owner-у без раскрытия removal snapshot internals.
    pub(super) fn active_lineage_at_removal(&self) -> Option<ActiveMediaLineageId> {
        self.removal.active_lineage_at_removal
    }
}

/// Read-only модель будущей prototype UI без UI wiring в Session 12A.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemovalUndoStatus {
    pub(crate) kind: ControllerRemovalKind,
    pub(crate) seconds_remaining: u64,
    pub(crate) next_wake_deadline: Instant,
}

/// Runtime boundary скрывает snapshot, но сохраняет typed action distinctions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeRemovalOutcome {
    Removed {
        kind: ControllerRemovalKind,
        selected_item_id: Option<PlaylistItemId>,
        current_outcome: RemovalCurrentOutcome,
        dirty: PlaylistDirtySignal,
        manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
        pending_cancellation: Option<Result<CancellationDispatchOutcome, MediaOpenCommandError>>,
    },
    NotFound {
        item_id: PlaylistItemId,
    },
    DuplicateItemId {
        item_id: PlaylistItemId,
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
    DeferredUntilStartupInstallResolution,
    InstallCommitLinearizing,
    FatalInvariant,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    DomainRevisionExhausted,
    DeadlineOverflow,
    LoadDecisionPending,
}

/// Typed Undo result сохраняет distinction expiry/invalidation/controller failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemovalUndoOutcome {
    Restored {
        selected_item_id: Option<PlaylistItemId>,
        reattached_active: bool,
    },
    Unavailable,
    Expired,
    Invalidated,
    Controller(ControllerRemovalUndoOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalUndoAvailability {
    Available,
    Expired,
    Invalidated,
}

impl RemovalUndoState {
    fn new(removal: ControllerDestructiveRemoval, now: Instant) -> Option<Self> {
        let expected_dirty_revision = removal.dirty.revision();
        let deadline = now.checked_add(REMOVAL_UNDO_WINDOW)?;
        Some(Self {
            removal,
            expected_dirty_revision,
            deadline,
        })
    }

    fn availability(
        &self,
        now: Instant,
        current_dirty_revision: PlaylistDirtyRevision,
        active_media: Option<ActiveMediaIdentity>,
    ) -> RemovalUndoAvailability {
        if now >= self.deadline {
            return RemovalUndoAvailability::Expired;
        }
        if current_dirty_revision != self.expected_dirty_revision {
            return RemovalUndoAvailability::Invalidated;
        }
        if active_media.map(ActiveMediaIdentity::lineage_id)
            != self.removal.active_lineage_at_removal
        {
            return RemovalUndoAvailability::Invalidated;
        }
        RemovalUndoAvailability::Available
    }

    fn status(&self, now: Instant) -> RemovalUndoStatus {
        let remaining = self.deadline.saturating_duration_since(now);
        let seconds_remaining = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0));
        let duration_after_next_label = Duration::from_secs(seconds_remaining.saturating_sub(1));
        let next_wake_deadline = self
            .deadline
            .checked_sub(duration_after_next_label)
            .unwrap_or(now)
            .min(self.deadline);
        RemovalUndoStatus {
            kind: self.removal.kind,
            seconds_remaining,
            next_wake_deadline,
        }
    }
}

impl PlaylistRuntime {
    /// Remove/Delete заменяет slot только после успешного domain commit-а.
    pub(crate) fn remove_playlist_item(
        &mut self,
        item_id: PlaylistItemId,
        now: Instant,
    ) -> RuntimeRemovalOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeRemovalOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.remove_item(item_id);
        let runtime_outcome = self.store_removal_outcome(outcome, now);
        self.publish_controller_mutation_if_dirty(dirty_before);
        runtime_outcome
    }

    /// Clear использует тот же last-action slot; empty no-op сохраняет прежний Undo.
    pub(crate) fn clear_playlist(&mut self, now: Instant) -> RuntimeRemovalOutcome {
        self.supersede_startup_media_apply();
        self.cancel_queue_replacement_confirmation_for_structural_replacement();
        self.supersede_manual_add_queue_generation();
        if self.controller.as_ref().is_none() {
            return match self.record_startup_clear() {
                Ok(()) => RuntimeRemovalOutcome::DeferredUntilStartupInstallResolution,
                Err(_) => RuntimeRemovalOutcome::StructuralRevisionExhausted,
            };
        }
        let retention_active = self.startup_action_retention_is_active();
        if retention_active && self.retain_startup_clear(now).is_err() {
            return RuntimeRemovalOutcome::StructuralRevisionExhausted;
        }
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeRemovalOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.clear_queue();
        let mut runtime_outcome = self.store_removal_outcome(outcome, now);
        self.publish_controller_mutation_if_dirty(dirty_before);
        if retention_active {
            if matches!(
                runtime_outcome,
                RuntimeRemovalOutcome::InstallCommitLinearizing
            ) {
                runtime_outcome = RuntimeRemovalOutcome::DeferredUntilStartupInstallResolution;
            } else {
                self.mark_retained_startup_queue_action_committed();
            }
        }
        runtime_outcome
    }

    /// Remove Others сохраняет retained row и один shared snapshot.
    pub(crate) fn remove_other_playlist_items(
        &mut self,
        retained_item_id: PlaylistItemId,
        now: Instant,
    ) -> RuntimeRemovalOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeRemovalOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.remove_other_items(retained_item_id);
        let runtime_outcome = self.store_removal_outcome(outcome, now);
        self.publish_controller_mutation_if_dirty(dirty_before);
        runtime_outcome
    }

    /// Удаляет exact captured selection одним commit-ом и создаёт один Undo slot.
    pub(crate) fn remove_selected_playlist_items(
        &mut self,
        item_ids: Arc<[PlaylistItemId]>,
        structural_revision: PlaylistStructuralRevision,
        now: Instant,
    ) -> RuntimeRemovalOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeRemovalOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.remove_selected_items(item_ids, structural_revision);
        let runtime_outcome = self.store_removal_outcome(outcome, now);
        self.publish_controller_mutation_if_dirty(dirty_before);
        runtime_outcome
    }

    /// Удаляет exact captured complement selection одним commit-ом и одним Undo.
    pub(crate) fn remove_unselected_playlist_items(
        &mut self,
        item_ids: Arc<[PlaylistItemId]>,
        structural_revision: PlaylistStructuralRevision,
        now: Instant,
    ) -> RuntimeRemovalOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeRemovalOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.remove_unselected_items(item_ids, structural_revision);
        let runtime_outcome = self.store_removal_outcome(outcome, now);
        self.publish_controller_mutation_if_dirty(dirty_before);
        runtime_outcome
    }

    fn store_removal_outcome(
        &mut self,
        outcome: ControllerDestructiveRemovalOutcome,
        now: Instant,
    ) -> RuntimeRemovalOutcome {
        match outcome {
            ControllerDestructiveRemovalOutcome::Removed(removal) => {
                let clears_media = removal.kind == ControllerRemovalKind::Clear;
                let media_reset_request = removal.media_reset_request;
                let pending_cancellation = removal.pending_request_to_cancel.map(|request_id| {
                    self.cancel_media_open(
                        request_id,
                        player_core::MediaInstallCancellationCause::StructuralInvalidation,
                    )
                });
                if clears_media {
                    self.media_reset.schedule(media_reset_request);
                    self.clear_resume_checkpoint_after_playlist_clear(now);
                }
                let summary = RuntimeRemovalOutcome::Removed {
                    kind: removal.kind,
                    selected_item_id: removal.selection_after.selected_cursor(),
                    current_outcome: removal.current_outcome,
                    dirty: removal.dirty,
                    manual_navigation_invalidation: removal.manual_navigation_invalidation,
                    pending_cancellation,
                };
                let Some(undo) = RemovalUndoState::new(*removal, now) else {
                    self.removal_undo = None;
                    return RuntimeRemovalOutcome::DeadlineOverflow;
                };
                // Замена Option освобождает snapshot предыдущего removal ровно один раз.
                self.removal_undo = Some(undo);
                summary
            }
            ControllerDestructiveRemovalOutcome::NotFound { item_id } => {
                RuntimeRemovalOutcome::NotFound { item_id }
            }
            ControllerDestructiveRemovalOutcome::DuplicateItemId { item_id } => {
                RuntimeRemovalOutcome::DuplicateItemId { item_id }
            }
            ControllerDestructiveRemovalOutcome::InvalidRetainedItem { item_id } => {
                RuntimeRemovalOutcome::InvalidRetainedItem { item_id }
            }
            ControllerDestructiveRemovalOutcome::PartialCompoundSelection { compound_entry_id } => {
                RuntimeRemovalOutcome::PartialCompoundSelection { compound_entry_id }
            }
            ControllerDestructiveRemovalOutcome::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            } => RuntimeRemovalOutcome::CompoundPartTarget {
                part_item_id,
                compound_entry_id,
            },
            ControllerDestructiveRemovalOutcome::StaleStructuralRevision => {
                RuntimeRemovalOutcome::StaleStructuralRevision
            }
            ControllerDestructiveRemovalOutcome::StaleSelection => {
                RuntimeRemovalOutcome::StaleSelection
            }
            ControllerDestructiveRemovalOutcome::NoChange => RuntimeRemovalOutcome::NoChange,
            ControllerDestructiveRemovalOutcome::InstallCommitLinearizing => {
                RuntimeRemovalOutcome::InstallCommitLinearizing
            }
            ControllerDestructiveRemovalOutcome::FatalInvariant => {
                RuntimeRemovalOutcome::FatalInvariant
            }
            ControllerDestructiveRemovalOutcome::DirtyRevisionExhausted => {
                RuntimeRemovalOutcome::DirtyRevisionExhausted
            }
            ControllerDestructiveRemovalOutcome::StructuralRevisionExhausted => {
                RuntimeRemovalOutcome::StructuralRevisionExhausted
            }
            ControllerDestructiveRemovalOutcome::DomainRevisionExhausted => {
                RuntimeRemovalOutcome::DomainRevisionExhausted
            }
        }
    }

    /// Возвращает countdown model и exactly once освобождает expired/invalidated slot.
    pub(crate) fn removal_undo_status(&mut self, now: Instant) -> Option<RemovalUndoStatus> {
        let controller = self.controller.as_ref()?;
        let availability = self.removal_undo.as_ref().map(|undo| {
            undo.availability(now, controller.dirty_revision(), controller.active_media())
        });
        match availability {
            Some(RemovalUndoAvailability::Available) => {
                self.removal_undo.as_ref().map(|undo| undo.status(now))
            }
            Some(RemovalUndoAvailability::Expired | RemovalUndoAvailability::Invalidated) => {
                self.removal_undo = None;
                None
            }
            None => None,
        }
    }

    /// Undo до deadline восстанавливает exact queue/traversal/selection как новую revision.
    pub(crate) fn undo_last_removal(&mut self, now: Instant) -> RemovalUndoOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RemovalUndoOutcome::Unavailable;
        };
        let dirty_before = controller.dirty_revision();
        let Some(undo) = self.removal_undo.as_ref() else {
            return RemovalUndoOutcome::Unavailable;
        };
        match undo.availability(now, controller.dirty_revision(), controller.active_media()) {
            RemovalUndoAvailability::Expired => {
                self.removal_undo = None;
                return RemovalUndoOutcome::Expired;
            }
            RemovalUndoAvailability::Invalidated => {
                self.removal_undo = None;
                return RemovalUndoOutcome::Invalidated;
            }
            RemovalUndoAvailability::Available => {}
        }

        let removal = undo.removal.clone();
        match controller.restore_destructive_removal(removal) {
            ControllerRemovalUndoOutcome::Restored {
                selected_item_id,
                reattached_active,
                ..
            } => {
                self.removal_undo = None;
                let outcome = RemovalUndoOutcome::Restored {
                    selected_item_id,
                    reattached_active,
                };
                self.publish_controller_mutation_if_dirty(dirty_before);
                outcome
            }
            ControllerRemovalUndoOutcome::ActiveLineageChanged => {
                self.removal_undo = None;
                RemovalUndoOutcome::Invalidated
            }
            failure => RemovalUndoOutcome::Controller(failure),
        }
    }

    /// Реальная persistent mutation инвалидирует Undo до изменения domain state.
    pub(crate) fn invalidate_removal_undo_for_persistent_mutation(&mut self) {
        self.removal_undo = None;
    }

    /// Exact Installed проходит через process owner, чтобы новая lineage освободила Undo сразу.
    pub(crate) fn on_playlist_installed(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        binding_generation: PlaylistBindingGeneration,
    ) -> Result<ControllerTerminalDrain, PlaylistControllerInvariantViolation> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(PlaylistControllerInvariantViolation::LoadDecisionPending)?;
        let dirty_before = controller.dirty_revision();
        let drain = controller.on_installed(
            request_id,
            player_request_id,
            media_instance_id,
            binding_generation,
        )?;
        if let Some(installed_active) = drain.active_media
            && self.removal_undo.as_ref().is_some_and(|undo| {
                undo.removal.active_lineage_at_removal != Some(installed_active.lineage_id())
            })
        {
            self.removal_undo = None;
        }
        self.publish_controller_mutation_if_dirty(dirty_before);
        Ok(drain)
    }
}
