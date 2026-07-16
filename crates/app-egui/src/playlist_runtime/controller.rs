//! Process-lifetime playlist controller: queue ownership, presentation state и typed outcomes.

mod automatic_lifecycle;
mod discovery;
mod discovery_navigation;
mod install;
mod manual_navigation;
mod metadata;
mod removal;
mod sorting;
mod startup_restore;
mod transport;

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use playlist_core::{
    AddItemsError, AddItemsOutcome, PlaylistItemDraft, PlaylistItemId, PlaylistQueue, RepeatMode,
    ShuffleToggleError,
};

use super::identity::{
    ActiveMediaIdentity, PlaylistItemErrorCategory, PlaylistItemErrorPhase,
    PlaylistItemRuntimeError, StopAfterCurrentLatch,
};
use super::view::{
    PlaylistDirtyRevision, PlaylistDirtySignal, PlaylistStructuralRevision, PlaylistViewSnapshot,
    PlaylistViewState, PlaylistWorkerAvailability, rebuild_snapshot,
};

#[allow(unused_imports)]
pub(crate) use automatic_lifecycle::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, AutomaticStopCause,
    AutomaticTargetFailureOutcome, EndedSnapshotKind, PlaylistErrorBehavior,
};
pub(crate) use discovery::{DiscoveryContinuation, DiscoveryContinuationRevision};
pub(crate) use discovery_navigation::{AutomaticDiscoveryReadiness, DiscoveryNavigationInterest};
#[allow(unused_imports)]
pub(crate) use install::{
    AuthorizationDispatchStart, BarrierRaceIntent, ControllerInstallPhase,
    ControllerMediaOpenCommand, ControllerMediaOpenCommandError, ControllerMediaOpenDisposition,
    ControllerTerminalDrain, ControllerTerminalResolution, DeferredControllerIntent,
    DesiredQueueModes, InstallReadyOutcome, LifecycleIntentOutcome,
    PlaylistControllerInvariantViolation, PlaylistInstallAdmissionError, PlaylistInstallMutation,
    PlaylistInstallRequest,
};
#[allow(unused_imports)]
pub(crate) use manual_navigation::{
    ManualNavigationCancelOutcome, ManualNavigationFailureOutcome, ManualNavigationInvalidation,
    ManualNavigationOriginState, ManualNavigationRetryOutcome, ManualNavigationTerminalAction,
    PreConcreteProbeRejectionOutcome,
};
#[allow(unused_imports)]
pub(crate) use metadata::ControllerMetadataPatchError;
pub(crate) use removal::{
    ControllerActiveMediaRebindOutcome, ControllerDestructiveRemoval,
    ControllerDestructiveRemovalOutcome, ControllerRemovalKind, ControllerRemovalUndoOutcome,
    DetachedActiveTombstone,
};
#[allow(unused_imports)]
pub(crate) use sorting::ControllerCanonicalSortError;
pub(crate) use startup_restore::{StartupRestoreFailureOutcome, StartupRestoreTarget};
#[allow(unused_imports)]
pub(crate) use transport::{
    AppTransportDisposition, ControllerManualNavigationAvailability,
    ControllerManualNavigationOutcome, ControllerPlayItemOutcome, ControllerStableIntentDispatch,
    DeferredTransportExecutionContext, DeferredTransportExecutionOutcome,
    DiscoveryManualWaitAvailability, ManualNavigationWaitId, PlannedPlaylistInstall,
    PreviousRestartThreshold, SiblingDiscoveryScopeId, StablePlaybackIntent,
    StopAfterCurrentOutcome, TransportGuardOutcome,
};

/// Typed append результат сохраняет distinction между mutation и no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerAppendOutcome {
    Added {
        item_ids: Vec<PlaylistItemId>,
        dirty: PlaylistDirtySignal,
        manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
    },
    NoItemsProvided,
}

/// Controller-owned preflight errors не смешиваются с domain allocation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerAppendError {
    FatalInvariant,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    Domain(AddItemsError),
}

/// D67 controller outcome сохраняет cap accounting рядом с exact committed IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControllerCappedAppendOutcome {
    pub(crate) item_ids: Vec<PlaylistItemId>,
    pub(crate) capacity_rejected: usize,
    pub(crate) dirty: Option<PlaylistDirtySignal>,
    pub(crate) manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
}

/// Ошибка correlation не изменяет badge и не dirty-ит persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeErrorCorrelationOutcome {
    Recorded,
    ItemNotCommitted,
    StaleRequest,
    StaleMediaInstance,
}

/// Startup initialization публикует не больше одной matching dirty revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StartupInitializationMutation {
    None,
    Modes,
    Structural,
}

/// Startup controller build не скрывает fallible queue mode initialization.
#[derive(Debug)]
pub(crate) enum StartupControllerBuildError {
    Shuffle(ShuffleToggleError),
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
}

/// Process-lifetime owner canonical queue и app-owned runtime identities.
pub(crate) struct PlaylistController {
    pub(super) queue: PlaylistQueue,
    pub(super) selected_item_id: Option<PlaylistItemId>,
    pub(super) active_media: Option<ActiveMediaIdentity>,
    pub(super) pending_target: Option<super::identity::PendingTarget>,
    pub(super) runtime_errors: HashMap<PlaylistItemId, PlaylistItemRuntimeError>,
    pub(super) repeat_mode: RepeatMode,
    pub(super) structural_revision: PlaylistStructuralRevision,
    pub(super) dirty_revision: PlaylistDirtyRevision,
    pub(super) latest_dirty_signal: Option<PlaylistDirtySignal>,
    pub(super) next_lineage_identity: u64,
    install_state: Option<install::InstallState>,
    pub(super) protected_modes_generation: u64,
    pub(super) stop_after_current: Option<StopAfterCurrentLatch>,
    pub(super) stable_playback_intent: transport::StablePlaybackIntent,
    pub(super) stable_intent_revision: u64,
    pub(super) transport_disposition: transport::AppTransportDisposition,
    pending_manual_traversal: Option<transport::PendingManualTraversal>,
    manual_navigation_cursor: manual_navigation::ManualNavigationCursor,
    automatic_lifecycle: automatic_lifecycle::AutomaticLifecycle,
    pub(super) detached_active_tombstone: Option<DetachedActiveTombstone>,
    error_behavior: automatic_lifecycle::PlaylistErrorBehavior,
    pub(super) next_manual_wait_identity: u64,
    discovery_continuation_revision: DiscoveryContinuationRevision,
    pub(super) worker_availability: PlaylistWorkerAvailability,
    pub(super) fatal_invariant: Option<PlaylistControllerInvariantViolation>,
    #[cfg(test)]
    reject_metadata_dirty_preflight_for_test: bool,
    view_snapshot: Arc<PlaylistViewSnapshot>,
}

impl PlaylistController {
    /// Создаёт чистую новую lineage без persistence/load wiring Session 14.
    pub(crate) fn new() -> Self {
        let queue = PlaylistQueue::new();
        let view_snapshot = Arc::new(PlaylistViewSnapshot::initial(&queue));
        Self {
            queue,
            selected_item_id: None,
            active_media: None,
            pending_target: None,
            runtime_errors: HashMap::new(),
            repeat_mode: RepeatMode::StopAtEnd,
            structural_revision: PlaylistStructuralRevision::INITIAL,
            dirty_revision: PlaylistDirtyRevision::CLEAN,
            latest_dirty_signal: None,
            next_lineage_identity: 1,
            install_state: None,
            protected_modes_generation: 0,
            stop_after_current: None,
            stable_playback_intent: transport::StablePlaybackIntent::Paused,
            stable_intent_revision: 1,
            transport_disposition: transport::AppTransportDisposition::Active,
            pending_manual_traversal: None,
            manual_navigation_cursor: manual_navigation::ManualNavigationCursor::default(),
            automatic_lifecycle: automatic_lifecycle::AutomaticLifecycle::default(),
            detached_active_tombstone: None,
            error_behavior: automatic_lifecycle::PlaylistErrorBehavior::Stop,
            next_manual_wait_identity: 1,
            discovery_continuation_revision: DiscoveryContinuationRevision::INITIAL,
            worker_availability: PlaylistWorkerAvailability::Available,
            fatal_invariant: None,
            #[cfg(test)]
            reject_metadata_dirty_preflight_for_test: false,
            view_snapshot,
        }
    }

    /// Создаёт controller только после allocator gate из уже выбранной startup queue.
    ///
    /// Persisted modes и coalesced pre-gate overlay применяются до единственной
    /// публикации view, поэтому промежуточное состояние renderer-у не видно.
    pub(super) fn from_startup_queue(
        queue: PlaylistQueue,
        persisted_repeat_mode: RepeatMode,
        desired_repeat_mode: Option<RepeatMode>,
        desired_shuffle_enabled: Option<bool>,
        mutation: StartupInitializationMutation,
    ) -> Result<Self, StartupControllerBuildError> {
        let mut controller = Self::new();
        controller.queue = queue;
        controller.repeat_mode = desired_repeat_mode.unwrap_or(persisted_repeat_mode);

        match desired_shuffle_enabled {
            Some(true) if !controller.queue.shuffle_enabled() => {
                controller
                    .queue
                    .enable_shuffle()
                    .map_err(StartupControllerBuildError::Shuffle)?;
            }
            Some(false) if controller.queue.shuffle_enabled() => {
                controller
                    .queue
                    .disable_shuffle()
                    .map_err(StartupControllerBuildError::Shuffle)?;
            }
            Some(_) | None => {}
        }

        if !matches!(mutation, StartupInitializationMutation::None) {
            let next_dirty = controller
                .dirty_revision
                .checked_next()
                .ok_or(StartupControllerBuildError::DirtyRevisionExhausted)?;
            controller.commit_dirty(next_dirty);
        }
        if matches!(mutation, StartupInitializationMutation::Structural) {
            controller.structural_revision = controller
                .structural_revision
                .checked_next()
                .ok_or(StartupControllerBuildError::StructuralRevisionExhausted)?;
        }

        controller.publish_view(true);
        Ok(controller)
    }

    pub(crate) fn view_snapshot(&self) -> Arc<PlaylistViewSnapshot> {
        self.view_snapshot.clone()
    }

    pub(crate) fn queue(&self) -> &PlaylistQueue {
        &self.queue
    }

    /// Возвращает committed repeat policy для app-owned neutral discovery priority mapping.
    pub(crate) const fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    pub(crate) const fn active_media(&self) -> Option<ActiveMediaIdentity> {
        self.active_media
    }

    pub(crate) const fn selected_item_id(&self) -> Option<PlaylistItemId> {
        self.selected_item_id
    }

    pub(crate) const fn dirty_revision(&self) -> PlaylistDirtyRevision {
        self.dirty_revision
    }

    pub(crate) const fn latest_dirty_signal(&self) -> Option<PlaylistDirtySignal> {
        self.latest_dirty_signal
    }

    pub(crate) const fn fatal_invariant(&self) -> Option<PlaylistControllerInvariantViolation> {
        self.fatal_invariant
    }

    /// Selection является presentation state и не запускает playback/dirty mutation.
    pub(crate) fn select_row(&mut self, item_id: Option<PlaylistItemId>) -> bool {
        let validated = item_id.filter(|candidate| self.queue.item(*candidate).is_some());
        if self.selected_item_id == validated {
            return false;
        }
        self.selected_item_id = validated;
        self.publish_view(false);
        true
    }

    /// Append не меняет active/pending/current и никогда не запускает playback.
    pub(crate) fn append(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<ControllerAppendOutcome, ControllerAppendError> {
        if self.fatal_invariant.is_some() {
            return Err(ControllerAppendError::FatalInvariant);
        }
        if drafts.is_empty() {
            return Ok(ControllerAppendOutcome::NoItemsProvided);
        }
        let next_dirty = self
            .dirty_revision
            .checked_next()
            .ok_or(ControllerAppendError::DirtyRevisionExhausted)?;
        let next_structural = self
            .structural_revision
            .checked_next()
            .ok_or(ControllerAppendError::StructuralRevisionExhausted)?;

        match self.queue.append_batch(drafts) {
            Ok(AddItemsOutcome::Added(item_ids)) => {
                let item_ids = item_ids.into_vec();
                let manual_navigation_invalidation =
                    self.invalidate_manual_navigation_after_structural_mutation();
                self.structural_revision = next_structural;
                let dirty = self.commit_dirty(next_dirty);
                self.publish_view(true);
                Ok(ControllerAppendOutcome::Added {
                    item_ids,
                    dirty,
                    manual_navigation_invalidation,
                })
            }
            Ok(AddItemsOutcome::NoItemsProvided) => Ok(ControllerAppendOutcome::NoItemsProvided),
            Err(error) => Err(ControllerAppendError::Domain(error)),
        }
    }

    /// D67 append-ит только доступный deterministic prefix одной mutation.
    pub(crate) fn append_capped_tail(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<ControllerCappedAppendOutcome, ControllerAppendError> {
        if self.fatal_invariant.is_some() {
            return Err(ControllerAppendError::FatalInvariant);
        }
        let accepted = drafts
            .len()
            .min(playlist_core::MAX_PLAYLIST_ITEMS.saturating_sub(self.queue.len()));
        if accepted == 0 {
            return Ok(ControllerCappedAppendOutcome {
                item_ids: Vec::new(),
                capacity_rejected: drafts.len(),
                dirty: None,
                manual_navigation_invalidation: None,
            });
        }
        let next_dirty = self
            .dirty_revision
            .checked_next()
            .ok_or(ControllerAppendError::DirtyRevisionExhausted)?;
        let next_structural = self
            .structural_revision
            .checked_next()
            .ok_or(ControllerAppendError::StructuralRevisionExhausted)?;
        let (item_ids, capacity_rejected) = self
            .queue
            .append_capped_tail(drafts)
            .map_err(ControllerAppendError::Domain)?
            .into_parts();
        let manual_navigation_invalidation =
            self.invalidate_manual_navigation_after_structural_mutation();
        self.structural_revision = next_structural;
        let dirty = self.commit_dirty(next_dirty);
        self.publish_view(true);
        Ok(ControllerCappedAppendOutcome {
            item_ids,
            capacity_rejected,
            dirty: Some(dirty),
            manual_navigation_invalidation,
        })
    }

    /// Retry start не очищает старый badge: D49 ждёт exact same-item Installed.
    pub(crate) fn record_request_error(
        &mut self,
        item_id: PlaylistItemId,
        request_id: crate::media_open::MediaOpenRequestId,
        phase: PlaylistItemErrorPhase,
        category: PlaylistItemErrorCategory,
        safe_summary: Arc<str>,
    ) -> RuntimeErrorCorrelationOutcome {
        if self.queue.item(item_id).is_none() {
            return RuntimeErrorCorrelationOutcome::ItemNotCommitted;
        }
        let request_matches = self.pending_target.is_some_and(|pending| {
            pending.request_id() == request_id && pending.item_id() == Some(item_id)
        });
        if !request_matches {
            return RuntimeErrorCorrelationOutcome::StaleRequest;
        }
        self.upsert_runtime_error(
            item_id,
            phase,
            category,
            safe_summary,
            Some(request_id),
            None,
        );
        RuntimeErrorCorrelationOutcome::Recorded
    }

    /// Runtime playback error принимается только от exact active instance/item.
    pub(crate) fn record_playback_error(
        &mut self,
        item_id: PlaylistItemId,
        media_instance_id: player_core::MediaInstanceId,
        safe_summary: Arc<str>,
    ) -> RuntimeErrorCorrelationOutcome {
        let active_matches = self.active_media.is_some_and(|active| {
            active.item_id() == Some(item_id) && active.media_instance_id() == media_instance_id
        });
        if !active_matches {
            return RuntimeErrorCorrelationOutcome::StaleMediaInstance;
        }
        self.upsert_runtime_error(
            item_id,
            PlaylistItemErrorPhase::Playback,
            PlaylistItemErrorCategory::Runtime,
            safe_summary,
            None,
            Some(media_instance_id),
        );
        RuntimeErrorCorrelationOutcome::Recorded
    }

    /// D70 unavailable row остаётся committed и не создаёт dirty signal.
    pub(crate) fn mark_committed_source_unavailable(
        &mut self,
        item_id: PlaylistItemId,
        safe_summary: Arc<str>,
    ) -> RuntimeErrorCorrelationOutcome {
        if self.queue.item(item_id).is_none() {
            return RuntimeErrorCorrelationOutcome::ItemNotCommitted;
        }
        self.upsert_runtime_error(
            item_id,
            PlaylistItemErrorPhase::SourceUnavailable,
            PlaylistItemErrorCategory::Unavailable,
            safe_summary,
            None,
            None,
        );
        RuntimeErrorCorrelationOutcome::Recorded
    }

    pub(crate) fn set_worker_availability(&mut self, availability: PlaylistWorkerAvailability) {
        if self.worker_availability != availability {
            self.worker_availability = availability;
            self.publish_view(false);
        }
    }

    /// Хранилище latch-а не исполняет `Ended` policy в Session 11A.
    pub(crate) fn set_stop_after_current(&mut self, enabled: bool) -> bool {
        let next_latch = if enabled {
            self.active_media.map(StopAfterCurrentLatch::new)
        } else {
            None
        };
        if self.stop_after_current == next_latch {
            return false;
        }
        self.stop_after_current = next_latch;
        true
    }

    pub(crate) const fn stop_after_current(&self) -> Option<StopAfterCurrentLatch> {
        self.stop_after_current
    }

    fn upsert_runtime_error(
        &mut self,
        item_id: PlaylistItemId,
        phase: PlaylistItemErrorPhase,
        category: PlaylistItemErrorCategory,
        safe_summary: Arc<str>,
        request_id: Option<crate::media_open::MediaOpenRequestId>,
        media_instance_id: Option<player_core::MediaInstanceId>,
    ) {
        self.runtime_errors
            .entry(item_id)
            .and_modify(|runtime_error| {
                runtime_error.replace_with_latest(
                    phase,
                    category,
                    safe_summary.clone(),
                    request_id,
                    media_instance_id,
                );
            })
            .or_insert_with(|| {
                PlaylistItemRuntimeError::first(
                    phase,
                    category,
                    safe_summary,
                    request_id,
                    media_instance_id,
                )
            });
        self.publish_view(false);
    }

    pub(super) fn commit_dirty(&mut self, revision: PlaylistDirtyRevision) -> PlaylistDirtySignal {
        self.dirty_revision = revision;
        let signal = PlaylistDirtySignal::new(revision);
        self.latest_dirty_signal = Some(signal);
        signal
    }

    pub(super) fn allocate_lineage(&mut self) -> Option<super::identity::ActiveMediaLineageId> {
        let identity = NonZeroU64::new(self.next_lineage_identity)?;
        self.next_lineage_identity = self.next_lineage_identity.checked_add(1)?;
        Some(super::identity::ActiveMediaLineageId::from_non_zero(
            identity,
        ))
    }

    fn invalidate_manual_navigation_after_structural_mutation(
        &mut self,
    ) -> Option<ManualNavigationInvalidation> {
        let request_id = self
            .manual_navigation_install_phase()
            .and_then(|(phase, request_id)| {
                (phase == ControllerInstallPhase::AwaitingReady).then_some(request_id)
            });
        if let Some(request_id) = request_id {
            // Structural mutation уже committed; старый coordinator result теперь только stale.
            if self
                .retire_awaiting_manual_navigation_request(request_id)
                .is_err()
            {
                self.set_fatal(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
                return None;
            }
        }
        let invalidation = self.manual_navigation_cursor.discard(
            &self.queue,
            player_core::MediaInstallCancellationCause::StructuralInvalidation,
            request_id,
        );
        if let Some(invalidation) = invalidation {
            self.consume_manual_terminal_action(
                invalidation.terminal_action,
                automatic_lifecycle::AutomaticStopCause::StructuralInvalidation,
            );
        }
        invalidation
    }

    pub(super) fn install_linearizing(&self) -> bool {
        self.install_state
            .as_ref()
            .is_some_and(install::InstallState::holds_reservation)
    }

    pub(super) fn publish_view(&mut self, structural_rows_changed: bool) {
        let state = PlaylistViewState {
            queue: &self.queue,
            structural_revision: self.structural_revision,
            errors: &self.runtime_errors,
            selected_item_id: self.selected_item_id,
            active_media: self.active_media,
            pending_target: self.pending_target,
            repeat_mode: self.repeat_mode,
            structural_actions_enabled: !self.install_linearizing()
                && self.fatal_invariant.is_none(),
            worker_availability: self.worker_availability,
            awaiting_user_after_navigation_failure: self
                .manual_navigation_cursor
                .is_awaiting_user_after_failure(),
            active_tombstone: self.detached_active_tombstone.is_some(),
        };
        self.view_snapshot = Arc::new(rebuild_snapshot(
            &self.view_snapshot,
            state,
            structural_rows_changed,
        ));
    }

    pub(super) fn set_fatal(&mut self, violation: PlaylistControllerInvariantViolation) {
        self.fatal_invariant.get_or_insert(violation);
        self.publish_view(false);
    }
}

impl Default for PlaylistController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
