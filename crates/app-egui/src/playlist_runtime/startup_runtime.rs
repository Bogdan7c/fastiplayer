//! App-owned startup outcome -> controller/load-gate policy.

use std::sync::Arc;

use playlist_core::{PlaylistQueue, PlaylistQueueRestore, RepeatMode};
use playlist_state::{
    InspectionOutcome, LoadedPlaylistState, QuarantineFileName, QuarantineOutcome, SaveBlockReason,
};

use super::{
    PlaylistController, PlaylistLineagePersistence, PlaylistLoadGateState, PlaylistRuntime,
    PlaylistStartupApplyError, PlaylistStartupDrainOutcome, PlaylistStartupPhase,
    PlaylistStartupStateStore, PlaylistStartupView, PlaylistStartupWarning,
    StartupDraftAdmissionError, StartupOwnerError, controller, startup,
};

#[allow(
    dead_code,
    reason = "Session 14 bootstrap/save-worker integration consumes these startup entrypoints"
)]
impl PlaylistRuntime {
    /// Structural user actions call this intent boundary; mode-only actions do not.
    pub(crate) fn supersede_startup_media_apply(&mut self) {
        self.startup_media_apply_superseded = true;
    }

    /// Read-only winner query keeps startup controller independent from queue fields.
    pub(crate) const fn startup_media_apply_was_superseded(&self) -> bool {
        self.startup_media_apply_superseded
    }

    /// Typed gate query для startup orchestration; caller не читает controller internals.
    pub(crate) const fn allocator_load_gate_is_open(&self) -> bool {
        matches!(self.load_gate, PlaylistLoadGateState::Open(_))
    }

    /// Возвращает только persisted current; `None` остаётся idle и не выбирает первый row.
    pub(crate) fn startup_restored_current(&self) -> Option<super::StartupRestoreTarget> {
        self.controller.as_ref()?.startup_restored_current()
    }

    /// D70/D22 failure остаётся runtime badge и может продолжить bounded paused chain.
    pub(crate) fn report_startup_restore_failure(
        &mut self,
        failed: super::StartupRestoreTarget,
        safe_summary: Arc<str>,
    ) -> Option<super::StartupRestoreTarget> {
        let controller = self.controller.as_mut()?;
        match controller.report_startup_restore_failure(failed, safe_summary) {
            super::StartupRestoreFailureOutcome::Stopped { cause } => {
                tracing::warn!(?cause, "Startup restore остановлен typed error policy");
                None
            }
            super::StartupRestoreFailureOutcome::OpenItem { target } => Some(target),
        }
    }

    /// Связывает opaque restored traversal plan с exact staged request.
    pub(crate) fn accept_startup_restore_install(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
        player_request_id: player_core::MediaInstallRequestId,
        target: super::StartupRestoreTarget,
    ) -> Result<(), super::PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(super::PlaylistMediaOpenGateError::LoadDecisionPending)?;
        controller
            .accept_startup_restore_install(request_id, player_request_id, target)
            .map_err(super::PlaylistMediaOpenGateError::InstallAdmission)
    }

    /// Только proven pre-barrier failure может сохранить/продолжить fallback.
    pub(crate) fn report_startup_restore_install_failure(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
        safe_summary: Arc<str>,
    ) -> Option<super::StartupRestoreTarget> {
        let outcome = self
            .controller
            .as_mut()?
            .report_startup_restore_install_failure(request_id, safe_summary)?;
        match outcome {
            super::StartupRestoreFailureOutcome::Stopped { cause } => {
                tracing::warn!(?cause, "Startup restore install failure остановил chain");
                None
            }
            super::StartupRestoreFailureOutcome::OpenItem { target } => Some(target),
        }
    }

    /// Запускает read-only inspection вне event-loop thread.
    pub(crate) fn begin_playlist_state_inspection(
        &mut self,
        store: Arc<dyn PlaylistStartupStateStore>,
    ) -> Result<(), StartupOwnerError> {
        self.startup.begin_inspection(store)
    }

    /// Забирает максимум один lossless completion и применяет typed startup policy.
    pub(crate) fn drain_playlist_state_startup(
        &mut self,
        quarantine_file_name: QuarantineFileName,
    ) -> Result<PlaylistStartupDrainOutcome, PlaylistStartupApplyError> {
        let Some(completion) = self.startup.drain_completion() else {
            return Ok(PlaylistStartupDrainOutcome::NoCompletion);
        };
        match completion {
            startup::StartupJobCompletion::Inspection {
                generation,
                outcome,
            } => {
                if generation != self.startup.decision_generation() || self.startup.is_shutdown() {
                    return Ok(PlaylistStartupDrainOutcome::StaleCompletionIgnored);
                }
                self.apply_inspection_outcome(outcome, quarantine_file_name)
            }
            startup::StartupJobCompletion::Quarantine {
                generation,
                corrupt_cause,
                outcome,
            } => {
                if generation != self.startup.decision_generation() || self.startup.is_shutdown() {
                    return Ok(PlaylistStartupDrainOutcome::StaleCompletionIgnored);
                }
                self.apply_quarantine_outcome(corrupt_cause, outcome)?;
                Ok(PlaylistStartupDrainOutcome::Ready)
            }
        }
    }

    /// Structural pre-gate Clear supersede-ит только restored items/traversal.
    #[allow(dead_code, reason = "Session 14A/UI wires the explicit Clear caller")]
    pub(crate) fn record_startup_clear(&mut self) -> Result<(), StartupDraftAdmissionError> {
        self.startup
            .draft_mut()
            .map_err(StartupDraftAdmissionError::Owner)?
            .record_clear()
            .map_err(StartupDraftAdmissionError::Draft)?;
        self.supersede_startup_media_apply();
        Ok(())
    }

    /// Open/Play/replacement ждут gate без provisional Item ID/player staging.
    #[allow(dead_code, reason = "Session 17 wires startup Open/Play precedence")]
    pub(crate) fn record_startup_media_replacement(
        &mut self,
    ) -> Result<(), StartupDraftAdmissionError> {
        if self.startup_action_retention_is_active() {
            self.retain_startup_media_replacement()
                .map_err(StartupDraftAdmissionError::Draft)?;
            self.supersede_startup_media_apply();
            return Ok(());
        }
        self.startup
            .draft_mut()
            .map_err(StartupDraftAdmissionError::Owner)?
            .record_media_replacement()
            .map_err(StartupDraftAdmissionError::Draft)?;
        self.supersede_startup_media_apply();
        Ok(())
    }

    /// Prepared Add остаётся ID-less и bounded до trusted allocator decision.
    pub(crate) fn record_startup_prepared_add(
        &mut self,
        drafts: Vec<playlist_core::PlaylistItemDraft>,
    ) -> Result<(), StartupDraftAdmissionError> {
        if self.startup_action_retention_is_active() {
            self.retain_startup_prepared_add(drafts)
                .map_err(StartupDraftAdmissionError::Draft)?;
            self.supersede_startup_media_apply();
            return Ok(());
        }
        self.startup
            .draft_mut()
            .map_err(StartupDraftAdmissionError::Owner)?
            .record_prepared_add(drafts)
            .map_err(StartupDraftAdmissionError::Draft)?;
        self.supersede_startup_media_apply();
        Ok(())
    }

    /// Mode-only intent coalesce-ится без supersede restore generation.
    pub(crate) fn record_startup_repeat_mode(
        &mut self,
        repeat_mode: RepeatMode,
    ) -> Result<bool, StartupOwnerError> {
        match self.startup.view().phase {
            PlaylistStartupPhase::Shutdown => Err(StartupOwnerError::InvalidPhase),
            PlaylistStartupPhase::Ready => {
                let dirty_before = self
                    .controller
                    .as_ref()
                    .ok_or(StartupOwnerError::InvalidPhase)?
                    .dirty_revision();
                let visible_before = self.controller.as_ref().map(|controller| {
                    (
                        controller.repeat_mode(),
                        controller.queue().shuffle_enabled(),
                    )
                });
                self.controller
                    .as_mut()
                    .ok_or(StartupOwnerError::InvalidPhase)?
                    .request_startup_mode_overlay(Some(repeat_mode), None)
                    .map_err(|_| StartupOwnerError::InvalidPhase)?;
                let visible_after = self.controller.as_ref().map(|controller| {
                    (
                        controller.repeat_mode(),
                        controller.queue().shuffle_enabled(),
                    )
                });
                self.publish_controller_mutation_if_dirty(dirty_before);
                Ok(visible_before != visible_after)
            }
            PlaylistStartupPhase::PendingLoadDecision
            | PlaylistStartupPhase::Inspecting
            | PlaylistStartupPhase::ApplyingQuarantine => {
                self.startup.draft_mut()?.set_repeat_mode(repeat_mode);
                Ok(false)
            }
        }
    }

    /// Mode-only intent coalesce-ится без supersede restore generation.
    pub(crate) fn record_startup_shuffle_enabled(
        &mut self,
        shuffle_enabled: bool,
    ) -> Result<bool, StartupOwnerError> {
        match self.startup.view().phase {
            PlaylistStartupPhase::Shutdown => Err(StartupOwnerError::InvalidPhase),
            PlaylistStartupPhase::Ready => {
                let dirty_before = self
                    .controller
                    .as_ref()
                    .ok_or(StartupOwnerError::InvalidPhase)?
                    .dirty_revision();
                let visible_before = self.controller.as_ref().map(|controller| {
                    (
                        controller.repeat_mode(),
                        controller.queue().shuffle_enabled(),
                    )
                });
                self.controller
                    .as_mut()
                    .ok_or(StartupOwnerError::InvalidPhase)?
                    .request_startup_mode_overlay(None, Some(shuffle_enabled))
                    .map_err(|_| StartupOwnerError::InvalidPhase)?;
                let visible_after = self.controller.as_ref().map(|controller| {
                    (
                        controller.repeat_mode(),
                        controller.queue().shuffle_enabled(),
                    )
                });
                self.publish_controller_mutation_if_dirty(dirty_before);
                Ok(visible_before != visible_after)
            }
            PlaylistStartupPhase::PendingLoadDecision
            | PlaylistStartupPhase::Inspecting
            | PlaylistStartupPhase::ApplyingQuarantine => {
                self.startup
                    .draft_mut()?
                    .set_shuffle_enabled(shuffle_enabled);
                Ok(false)
            }
        }
    }

    /// Read-only loading/warning/save-block model не отдаёт mutable policy state.
    pub(crate) const fn playlist_startup_view(&self) -> PlaylistStartupView {
        self.startup.view()
    }

    fn apply_inspection_outcome(
        &mut self,
        outcome: InspectionOutcome,
        quarantine_file_name: QuarantineFileName,
    ) -> Result<PlaylistStartupDrainOutcome, PlaylistStartupApplyError> {
        match outcome {
            InspectionOutcome::Missing => {
                self.complete_new_lineage(PlaylistLineagePersistence::Persistent, None)?;
                Ok(PlaylistStartupDrainOutcome::Ready)
            }
            InspectionOutcome::Loaded(loaded) => {
                self.complete_loaded_lineage(loaded)?;
                Ok(PlaylistStartupDrainOutcome::Ready)
            }
            InspectionOutcome::CorruptNeedsQuarantine {
                inspected_identity,
                cause,
            } => {
                self.startup
                    .start_quarantine(cause, inspected_identity, quarantine_file_name)
                    .map_err(PlaylistStartupApplyError::Owner)?;
                Ok(PlaylistStartupDrainOutcome::ApplyingQuarantine)
            }
            InspectionOutcome::NewerSchemaSaveBlocked { schema_version } => {
                self.complete_new_lineage(
                    PlaylistLineagePersistence::NonPersistent {
                        queue_generation: self.startup.queue_generation(),
                        save_block: SaveBlockReason::NewerSchema,
                    },
                    Some(PlaylistStartupWarning::NewerSchema { schema_version }),
                )?;
                Ok(PlaylistStartupDrainOutcome::Ready)
            }
            InspectionOutcome::UnrecognizedVersionSaveBlocked { cause } => {
                let save_block = if matches!(
                    cause,
                    playlist_state::ProtectedStateCause::DuplicateSchemaVersion
                ) {
                    SaveBlockReason::DuplicateVersion
                } else {
                    SaveBlockReason::UnrecognizedVersion
                };
                self.complete_new_lineage(
                    PlaylistLineagePersistence::NonPersistent {
                        queue_generation: self.startup.queue_generation(),
                        save_block,
                    },
                    Some(PlaylistStartupWarning::UnrecognizedVersion { cause }),
                )?;
                Ok(PlaylistStartupDrainOutcome::Ready)
            }
        }
    }

    fn apply_quarantine_outcome(
        &mut self,
        corrupt_cause: playlist_state::CorruptStateCause,
        outcome: QuarantineOutcome,
    ) -> Result<(), PlaylistStartupApplyError> {
        match outcome {
            QuarantineOutcome::Applied { .. } => self.complete_new_lineage(
                PlaylistLineagePersistence::Persistent,
                Some(PlaylistStartupWarning::CorruptStateQuarantined {
                    cause: corrupt_cause,
                }),
            ),
            QuarantineOutcome::SourceChanged => self.complete_new_lineage(
                PlaylistLineagePersistence::NonPersistent {
                    queue_generation: self.startup.queue_generation(),
                    save_block: SaveBlockReason::QuarantineSourceChanged,
                },
                Some(PlaylistStartupWarning::CorruptStateSourceChanged {
                    cause: corrupt_cause,
                }),
            ),
            QuarantineOutcome::FailedSaveBlocked { cause } => self.complete_new_lineage(
                PlaylistLineagePersistence::NonPersistent {
                    queue_generation: self.startup.queue_generation(),
                    save_block: SaveBlockReason::QuarantineFailed,
                },
                Some(PlaylistStartupWarning::CorruptStateQuarantineFailed {
                    corrupt_cause,
                    failure_cause: cause,
                }),
            ),
        }
    }

    fn complete_loaded_lineage(
        &mut self,
        loaded: LoadedPlaylistState,
    ) -> Result<(), PlaylistStartupApplyError> {
        let draft = self.startup.take_draft();
        let (queue_plan, desired_modes) = draft.into_parts();
        let (loaded_queue, persisted_repeat_mode) = loaded.into_parts();
        let next_item_id = loaded_queue.next_item_id_snapshot();
        let persisted_shuffle_enabled = loaded_queue.shuffle_enabled();
        let applies_restored_items = queue_plan.applies_restored_items();
        let queue = if applies_restored_items {
            loaded_queue
        } else {
            PlaylistQueue::restore(PlaylistQueueRestore::new(Vec::new(), next_item_id, None))
                .map_err(|_| PlaylistStartupApplyError::AllocatorInvariant)?
        };
        let desired_shuffle_enabled = desired_modes
            .shuffle_enabled()
            .or((!applies_restored_items).then_some(persisted_shuffle_enabled));
        let mode_changed = desired_modes
            .repeat_mode()
            .is_some_and(|mode| mode != persisted_repeat_mode)
            || desired_modes
                .shuffle_enabled()
                .is_some_and(|enabled| enabled != persisted_shuffle_enabled);
        self.install_startup_controller(
            queue,
            persisted_repeat_mode,
            desired_modes.repeat_mode(),
            desired_shuffle_enabled,
            queue_plan,
            true,
            mode_changed,
            PlaylistLineagePersistence::Persistent,
            None,
        )
    }

    fn complete_new_lineage(
        &mut self,
        persistence: PlaylistLineagePersistence,
        warning: Option<PlaylistStartupWarning>,
    ) -> Result<(), PlaylistStartupApplyError> {
        let draft = self.startup.take_draft();
        let (queue_plan, desired_modes) = draft.into_parts();
        let mut policy_controller = PlaylistController::new();
        self.settings
            .initialize_new_queue_policy(&mut policy_controller);
        let default_repeat_mode = policy_controller.repeat_mode;
        let mode_changed = desired_modes
            .repeat_mode()
            .is_some_and(|mode| mode != default_repeat_mode)
            || desired_modes
                .shuffle_enabled()
                .is_some_and(|enabled| enabled);
        self.install_startup_controller(
            PlaylistQueue::new(),
            default_repeat_mode,
            desired_modes.repeat_mode(),
            desired_modes.shuffle_enabled(),
            queue_plan,
            false,
            mode_changed,
            persistence,
            warning,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn install_startup_controller(
        &mut self,
        queue: PlaylistQueue,
        persisted_repeat_mode: RepeatMode,
        desired_repeat_mode: Option<RepeatMode>,
        desired_shuffle_enabled: Option<bool>,
        queue_plan: startup::StartupQueuePlan,
        loaded_state_existed: bool,
        mode_changed: bool,
        persistence: PlaylistLineagePersistence,
        warning: Option<PlaylistStartupWarning>,
    ) -> Result<(), PlaylistStartupApplyError> {
        let mutation = match &queue_plan {
            startup::StartupQueuePlan::PreparedItems(_) => {
                controller::StartupInitializationMutation::None
            }
            startup::StartupQueuePlan::Empty if loaded_state_existed => {
                controller::StartupInitializationMutation::Structural
            }
            startup::StartupQueuePlan::RestoreCandidate
            | startup::StartupQueuePlan::Empty
            | startup::StartupQueuePlan::AwaitingMediaReplacement
                if mode_changed =>
            {
                controller::StartupInitializationMutation::Modes
            }
            startup::StartupQueuePlan::RestoreCandidate
            | startup::StartupQueuePlan::Empty
            | startup::StartupQueuePlan::AwaitingMediaReplacement => {
                controller::StartupInitializationMutation::None
            }
        };
        let mut controller = PlaylistController::from_startup_queue(
            queue,
            persisted_repeat_mode,
            desired_repeat_mode,
            desired_shuffle_enabled,
            mutation,
        )
        .map_err(PlaylistStartupApplyError::Controller)?;
        self.settings
            .initialize_restored_queue_policy(&mut controller);
        if let startup::StartupQueuePlan::PreparedItems(drafts) = queue_plan {
            controller
                .append(drafts)
                .map_err(PlaylistStartupApplyError::Append)?;
        }

        self.controller.install(controller);
        self.load_gate = PlaylistLoadGateState::Open(persistence);
        self.startup.mark_ready(persistence, warning);
        self.activate_playlist_persistence();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn resolve_missing_state_for_test(&mut self) {
        self.complete_new_lineage(PlaylistLineagePersistence::Persistent, None)
            .expect("missing-state fixture must open allocator gate");
    }
}
