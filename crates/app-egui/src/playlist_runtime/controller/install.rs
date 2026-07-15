//! D08/D39 reservation/authorization state machine playlist controller-а.

mod intents;
mod manual;
mod token;

pub(crate) use intents::{
    BarrierRaceIntent, ControllerTerminalDrain, ControllerTerminalResolution,
    DeferredControllerIntent, DeferredTransportIntent, DesiredQueueModes, LifecycleIntentOutcome,
};
use token::{GuardedInstallAbort, GuardedInstallToken};

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackIntentRevision};
use playlist_core::{
    AutomaticTraversalPlan, PrepareReservedMutationError, QueueRevisionSnapshot,
    ReservedQueueMutation, ShuffleToggleError,
};

use super::PlaylistController;
use crate::media_open::{AuthorizationDispatchResolution, MediaOpenClientKey, MediaOpenRequestId};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, PendingTarget, PendingTargetOrigin};
use crate::playlist_runtime::view::{PlaylistDirtySignal, PlaylistWorkerAvailability};

/// Controller выбирает command semantics; coordinator остаётся policy-neutral executor-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMediaOpenDisposition {
    Start,
    Coalesce,
    Supersede {
        expected_request_id: MediaOpenRequestId,
    },
}

/// Opaque controller command не содержит queue target/priority для coordinator-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMediaOpenCommand {
    Start {
        client_key: MediaOpenClientKey,
    },
    Coalesce {
        client_key: MediaOpenClientKey,
    },
    Supersede {
        expected_request_id: MediaOpenRequestId,
        client_key: MediaOpenClientKey,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMediaOpenCommandError {
    WorkerUnavailable,
    InstallCommitLinearizing,
    FatalInvariant,
}

/// Admission результата coordinator-а не превращает ожидаемый busy/supersede race в fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistInstallAdmissionError {
    Busy,
    StaleSupersede,
    InstallCommitLinearizing,
    FatalInvariant,
}

/// Все queue-specific данные остаются в controller после neutral coordinator admission.
pub(crate) struct PlaylistInstallRequest {
    pub request_id: MediaOpenRequestId,
    pub player_request_id: MediaInstallRequestId,
    pub target_item_id: Option<playlist_core::PlaylistItemId>,
    pub origin: PendingTargetOrigin,
    pub intent_revision: PlaybackIntentRevision,
    pub expected_queue_revision: QueueRevisionSnapshot,
    pub mutation: PlaylistInstallMutation,
}

/// Controller сохраняет domain-owned manual preview вплоть до exact Installed.
pub(crate) enum PlaylistInstallMutation {
    /// Обычный explicit select/replacement reservation.
    Reserved(ReservedQueueMutation),
    /// One-step manual navigation, включая private shuffle history/upcoming preview.
    ManualNavigation,
    /// Opaque fixed-snapshot automatic traversal plan.
    AutomaticTraversal(Box<AutomaticTraversalPlan>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerInstallPhase {
    AwaitingReady,
    ReservedAwaitingAuthorization,
    AuthorizationDispatchPending,
    AuthorizationInFlight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistControllerInvariantViolation {
    StaleReadyToCommit,
    UnexpectedInstallPhase,
    MissingAuthorizationResolution,
    MissingInstalledTerminal,
    TerminalRequestMismatch,
    PlayerRequestMismatch,
    DirtyRevisionExhaustedAfterPlayerCommit,
    LineageIdentityExhausted,
    DeferredModeApplicationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallReadyOutcome {
    RequestAuthorization {
        request_id: MediaOpenRequestId,
    },
    ReservationRejected {
        request_id: MediaOpenRequestId,
        error: PrepareReservedMutationError,
    },
    StaleManualNavigationResult {
        request_id: MediaOpenRequestId,
    },
    Fatal(PlaylistControllerInvariantViolation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuthorizationDispatchStart {
    request_id: MediaOpenRequestId,
}

impl AuthorizationDispatchStart {
    pub(crate) const fn request_id(self) -> MediaOpenRequestId {
        self.request_id
    }
}

pub(super) enum InstallState {
    AwaitingReady(AwaitingReady),
    ReservedAwaitingAuthorization(GuardedInstall),
    AuthorizationDispatchPending {
        guarded: GuardedInstall,
        race_intent: Option<BarrierRaceIntent>,
    },
    AuthorizationInFlight {
        guarded: GuardedInstall,
        post_commit_intent: Option<DeferredControllerIntent>,
    },
}

impl InstallState {
    pub(super) const fn holds_reservation(&self) -> bool {
        !matches!(self, Self::AwaitingReady(_))
    }

    const fn phase(&self) -> ControllerInstallPhase {
        match self {
            Self::AwaitingReady(_) => ControllerInstallPhase::AwaitingReady,
            Self::ReservedAwaitingAuthorization(_) => {
                ControllerInstallPhase::ReservedAwaitingAuthorization
            }
            Self::AuthorizationDispatchPending { .. } => {
                ControllerInstallPhase::AuthorizationDispatchPending
            }
            Self::AuthorizationInFlight { .. } => ControllerInstallPhase::AuthorizationInFlight,
        }
    }

    pub(super) const fn request_id(&self) -> MediaOpenRequestId {
        match self {
            Self::AwaitingReady(state) => state.request.request_id,
            Self::ReservedAwaitingAuthorization(state)
            | Self::AuthorizationDispatchPending { guarded: state, .. }
            | Self::AuthorizationInFlight { guarded: state, .. } => state.request_id,
        }
    }

    pub(super) const fn player_request_id(&self) -> MediaInstallRequestId {
        match self {
            Self::AwaitingReady(state) => state.request.player_request_id,
            Self::ReservedAwaitingAuthorization(state)
            | Self::AuthorizationDispatchPending { guarded: state, .. }
            | Self::AuthorizationInFlight { guarded: state, .. } => state.player_request_id,
        }
    }
}

pub(super) struct AwaitingReady {
    request: PlaylistInstallRequest,
}

pub(super) struct GuardedInstall {
    request_id: MediaOpenRequestId,
    player_request_id: MediaInstallRequestId,
    target_item_id: Option<playlist_core::PlaylistItemId>,
    token: GuardedInstallToken,
    desired_modes: Option<DesiredQueueModes>,
    queue_revision_before_commit: QueueRevisionSnapshot,
}

impl PlaylistController {
    /// Возвращает policy command до передачи source payload coordinator-у.
    pub(crate) fn media_open_command(
        &self,
        client_key: MediaOpenClientKey,
        disposition: ControllerMediaOpenDisposition,
    ) -> Result<ControllerMediaOpenCommand, ControllerMediaOpenCommandError> {
        if self.fatal_invariant.is_some() {
            return Err(ControllerMediaOpenCommandError::FatalInvariant);
        }
        if self.worker_availability == PlaylistWorkerAvailability::Unavailable {
            return Err(ControllerMediaOpenCommandError::WorkerUnavailable);
        }
        if self.install_linearizing() {
            return Err(ControllerMediaOpenCommandError::InstallCommitLinearizing);
        }
        Ok(match disposition {
            ControllerMediaOpenDisposition::Start => {
                ControllerMediaOpenCommand::Start { client_key }
            }
            ControllerMediaOpenDisposition::Coalesce => {
                ControllerMediaOpenCommand::Coalesce { client_key }
            }
            ControllerMediaOpenDisposition::Supersede {
                expected_request_id,
            } => ControllerMediaOpenCommand::Supersede {
                expected_request_id,
                client_key,
            },
        })
    }

    /// Регистрирует exact coordinator/player request после admission/staging acceptance.
    pub(crate) fn accept_install_request(
        &mut self,
        request: PlaylistInstallRequest,
    ) -> Result<(), PlaylistInstallAdmissionError> {
        if self.fatal_invariant.is_some() {
            return Err(PlaylistInstallAdmissionError::FatalInvariant);
        }
        if self.install_state.is_some() {
            return Err(if self.install_linearizing() {
                PlaylistInstallAdmissionError::InstallCommitLinearizing
            } else {
                PlaylistInstallAdmissionError::Busy
            });
        }
        if matches!(request.mutation, PlaylistInstallMutation::ManualNavigation)
            && (!self
                .manual_navigation_cursor
                .matches_target(request.target_item_id)
                || !self
                    .manual_navigation_cursor
                    .bind_request(request.request_id))
        {
            return Err(PlaylistInstallAdmissionError::StaleSupersede);
        }
        self.replace_awaiting_ready(request);
        Ok(())
    }

    /// Exact pre-Ready supersede заменяет один pending intent без command FIFO.
    pub(crate) fn supersede_install_request_before_ready(
        &mut self,
        expected_request_id: MediaOpenRequestId,
        replacement: PlaylistInstallRequest,
    ) -> Result<(), PlaylistInstallAdmissionError> {
        if self.fatal_invariant.is_some() {
            return Err(PlaylistInstallAdmissionError::FatalInvariant);
        }
        let Some(state) = self.install_state.take() else {
            return Err(PlaylistInstallAdmissionError::StaleSupersede);
        };
        let InstallState::AwaitingReady(awaiting) = state else {
            self.install_state = Some(state);
            return Err(PlaylistInstallAdmissionError::InstallCommitLinearizing);
        };
        if awaiting.request.request_id != expected_request_id {
            self.install_state = Some(InstallState::AwaitingReady(awaiting));
            return Err(PlaylistInstallAdmissionError::StaleSupersede);
        }
        if matches!(
            replacement.mutation,
            PlaylistInstallMutation::ManualNavigation
        ) && (!self
            .manual_navigation_cursor
            .matches_target(replacement.target_item_id)
            || !self
                .manual_navigation_cursor
                .bind_request(replacement.request_id))
        {
            self.install_state = Some(InstallState::AwaitingReady(awaiting));
            return Err(PlaylistInstallAdmissionError::StaleSupersede);
        }
        self.replace_awaiting_ready(replacement);
        Ok(())
    }

    /// Coordinator coalesce обязан вернуть тот же exact request, иначе state рассинхронизирован.
    pub(crate) fn confirm_coalesced_install_request(
        &self,
        request_id: MediaOpenRequestId,
    ) -> Result<(), PlaylistInstallAdmissionError> {
        match &self.install_state {
            Some(InstallState::AwaitingReady(awaiting))
                if awaiting.request.request_id == request_id =>
            {
                Ok(())
            }
            Some(state) if state.holds_reservation() => {
                Err(PlaylistInstallAdmissionError::InstallCommitLinearizing)
            }
            _ => Err(PlaylistInstallAdmissionError::StaleSupersede),
        }
    }

    fn replace_awaiting_ready(&mut self, request: PlaylistInstallRequest) {
        self.pending_target = Some(PendingTarget::new(
            request.request_id,
            request.target_item_id,
            request.origin,
            request.intent_revision,
        ));
        self.install_state = Some(InstallState::AwaitingReady(AwaitingReady { request }));
        self.publish_view(false);
    }

    pub(super) fn take_awaiting_automatic_failure(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Option<(playlist_core::PlaylistItemId, AutomaticTraversalPlan)> {
        let state = self.install_state.take()?;
        let InstallState::AwaitingReady(awaiting) = state else {
            self.install_state = Some(state);
            return None;
        };
        if awaiting.request.request_id != request_id {
            self.install_state = Some(InstallState::AwaitingReady(awaiting));
            return None;
        }
        if !matches!(
            awaiting.request.mutation,
            PlaylistInstallMutation::AutomaticTraversal(_)
        ) {
            self.install_state = Some(InstallState::AwaitingReady(awaiting));
            return None;
        }
        let PlaylistInstallRequest {
            target_item_id,
            mutation,
            ..
        } = awaiting.request;
        let PlaylistInstallMutation::AutomaticTraversal(plan) = mutation else {
            return None;
        };
        let item_id = target_item_id?;
        if plan.target_item_id() != item_id {
            return None;
        }
        self.pending_target = None;
        self.publish_view(false);
        Some((item_id, *plan))
    }

    pub(crate) fn install_phase(&self) -> Option<ControllerInstallPhase> {
        self.install_state.as_ref().map(InstallState::phase)
    }

    /// Structural removal retires only a pre-Ready request; guarded phases remain immutable.
    pub(super) fn retire_awaiting_install_for_removal(
        &mut self,
    ) -> Result<Option<MediaOpenRequestId>, PlaylistControllerInvariantViolation> {
        let Some(state) = self.install_state.take() else {
            return Ok(None);
        };
        let InstallState::AwaitingReady(awaiting) = state else {
            self.install_state = Some(state);
            return self.fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        let request_id = awaiting.request.request_id;
        self.pending_target = None;
        Ok(Some(request_id))
    }

    /// Matching Ready выполняет все domain fallible checks до authorization dispatch.
    pub(crate) fn on_ready_to_commit(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> InstallReadyOutcome {
        if let Some(violation) = self.fatal_invariant {
            return InstallReadyOutcome::Fatal(violation);
        }
        if self.manual_navigation_cursor.is_retired_request(request_id) {
            return InstallReadyOutcome::StaleManualNavigationResult { request_id };
        }
        let Some(state) = self.install_state.take() else {
            return self.fatal_ready(PlaylistControllerInvariantViolation::StaleReadyToCommit);
        };
        let InstallState::AwaitingReady(awaiting) = state else {
            self.install_state = Some(state);
            return self.fatal_ready(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        if awaiting.request.request_id != request_id {
            self.install_state = Some(InstallState::AwaitingReady(awaiting));
            return self.fatal_ready(PlaylistControllerInvariantViolation::StaleReadyToCommit);
        }
        let PlaylistInstallRequest {
            request_id,
            player_request_id,
            target_item_id,
            expected_queue_revision,
            mutation,
            ..
        } = awaiting.request;
        let token_result = match mutation {
            PlaylistInstallMutation::Reserved(mutation) => self
                .queue
                .prepare_reserved_mutation(expected_queue_revision, mutation)
                .map(GuardedInstallToken::Queue),
            PlaylistInstallMutation::ManualNavigation => {
                if !self.manual_navigation_cursor.matches_target(target_item_id) {
                    self.pending_target = None;
                    self.publish_view(false);
                    return InstallReadyOutcome::StaleManualNavigationResult { request_id };
                }
                let Some(preview) = self.manual_navigation_cursor.take_for_prepare() else {
                    self.pending_target = None;
                    self.publish_view(false);
                    return InstallReadyOutcome::StaleManualNavigationResult { request_id };
                };
                match self.queue.prepare_manual_navigation(preview) {
                    Ok(token) => Ok(GuardedInstallToken::ManualNavigation(token)),
                    Err(failure) => {
                        let reason = failure.reason();
                        self.manual_navigation_cursor
                            .restore_after_abort(failure.into_preview());
                        Err(reason)
                    }
                }
            }
            PlaylistInstallMutation::AutomaticTraversal(plan) => {
                match self.queue.prepare_automatic_traversal(*plan) {
                    Ok(token) => Ok(GuardedInstallToken::AutomaticTraversal(token)),
                    Err(failure) => {
                        let reason = failure.reason();
                        if let Some(item_id) = target_item_id {
                            self.retain_released_automatic_plan(
                                request_id,
                                item_id,
                                failure.into_plan(),
                            );
                        }
                        Err(reason)
                    }
                }
            }
        };
        let token = match token_result {
            Ok(token) => token,
            Err(error) => {
                self.pending_target = None;
                self.publish_view(false);
                return InstallReadyOutcome::ReservationRejected { request_id, error };
            }
        };
        self.install_state = Some(InstallState::ReservedAwaitingAuthorization(
            GuardedInstall {
                request_id,
                player_request_id,
                target_item_id,
                token,
                desired_modes: None,
                queue_revision_before_commit: self.queue.revision_snapshot(),
            },
        ));
        self.publish_view(false);
        InstallReadyOutcome::RequestAuthorization { request_id }
    }

    /// Coordinator command acceptance переводит controller в отдельную dispatch-pending фазу.
    pub(crate) fn begin_authorization_dispatch(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<AuthorizationDispatchStart, PlaylistControllerInvariantViolation> {
        if let Some(violation) = self.fatal_invariant {
            return Err(violation);
        }
        let Some(state) = self.install_state.take() else {
            return self.fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        let InstallState::ReservedAwaitingAuthorization(guarded) = state else {
            self.install_state = Some(state);
            return self.fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        if guarded.request_id != request_id {
            self.install_state = Some(InstallState::ReservedAwaitingAuthorization(guarded));
            return self
                .fatal_result(PlaylistControllerInvariantViolation::TerminalRequestMismatch);
        }
        self.install_state = Some(InstallState::AuthorizationDispatchPending {
            guarded,
            race_intent: None,
        });
        self.publish_view(false);
        Ok(AuthorizationDispatchStart { request_id })
    }

    /// Только authoritative lossless resolution решает судьбу opaque token-а.
    pub(crate) fn resolve_authorization_dispatch(
        &mut self,
        request_id: MediaOpenRequestId,
        resolution: AuthorizationDispatchResolution,
    ) -> Result<Option<ControllerTerminalDrain>, PlaylistControllerInvariantViolation> {
        if let Some(violation) = self.fatal_invariant {
            return Err(violation);
        }
        let Some(state) = self.install_state.take() else {
            return self.fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        let InstallState::AuthorizationDispatchPending {
            guarded,
            race_intent,
        } = state
        else {
            self.install_state = Some(state);
            return self.fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        if guarded.request_id != request_id {
            self.install_state = Some(InstallState::AuthorizationDispatchPending {
                guarded,
                race_intent,
            });
            return self
                .fatal_result(PlaylistControllerInvariantViolation::TerminalRequestMismatch);
        }
        match resolution {
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner => {
                self.install_state = Some(InstallState::AuthorizationInFlight {
                    guarded,
                    post_commit_intent: race_intent.map(BarrierRaceIntent::intent),
                });
                self.publish_view(false);
                Ok(None)
            }
            resolution @ (AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                ..
            }
            | AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                ..
            }) => {
                let GuardedInstall {
                    request_id,
                    target_item_id,
                    token,
                    desired_modes,
                    queue_revision_before_commit: _,
                    ..
                } = guarded;
                match token.abort(&mut self.queue) {
                    GuardedInstallAbort::Queue => {}
                    GuardedInstallAbort::ManualNavigation(preview) => match resolution {
                        AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                            ..
                        } => self
                            .manual_navigation_cursor
                            .mark_failed_after_abort(preview),
                        AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                            ..
                        } => self.manual_navigation_cursor.restore_after_abort(preview),
                        AuthorizationDispatchResolution::EnqueuedAtPlayerOwner => {
                            unreachable!("pre-barrier branch excludes enqueue winner")
                        }
                    },
                    GuardedInstallAbort::AutomaticTraversal(plan) => {
                        if matches!(
                            resolution,
                            AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue { .. }
                        ) && let Some(item_id) = target_item_id
                        {
                            self.retain_released_automatic_plan(request_id, item_id, plan);
                        }
                    }
                }
                if let Some(deferred) = race_intent.map(BarrierRaceIntent::intent)
                    && !matches!(deferred, DeferredControllerIntent::Transport(_))
                {
                    let _discarded = self.manual_navigation_cursor.discard(
                        &self.queue,
                        deferred.cancellation_cause(),
                        Some(request_id),
                    );
                }
                self.pending_target = None;
                let dirty = self.apply_desired_modes(desired_modes)?;
                let deferred_intent = race_intent.map(BarrierRaceIntent::intent);
                self.publish_view(false);
                Ok(Some(ControllerTerminalDrain {
                    request_id,
                    active_media: self.active_media,
                    dirty,
                    deferred_intent,
                    resolution: match resolution {
                        AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue { cause } => {
                            ControllerTerminalResolution::CancelWonBeforePlayerEnqueue { cause }
                        }
                        AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                            rejection,
                        } => ControllerTerminalResolution::DownstreamRejectedBeforeEnqueue {
                            rejection,
                        },
                        AuthorizationDispatchResolution::EnqueuedAtPlayerOwner => {
                            unreachable!("pre-barrier branch excludes enqueue winner")
                        }
                    },
                }))
            }
        }
    }

    /// Delayed resolution никогда не превращается в timeout-abort.
    pub(crate) fn report_missing_authorization_resolution(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> PlaylistControllerInvariantViolation {
        let violation = if self.install_state.as_ref().is_some_and(|state| {
            state.request_id() == request_id
                && state.phase() == ControllerInstallPhase::AuthorizationDispatchPending
        }) {
            PlaylistControllerInvariantViolation::MissingAuthorizationResolution
        } else {
            PlaylistControllerInvariantViolation::UnexpectedInstallPhase
        };
        self.set_fatal(violation);
        violation
    }

    /// Exact Installed one-shot коммитит token, затем modes, затем возвращает deferred intent.
    pub(crate) fn on_installed(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        binding_generation: PlaylistBindingGeneration,
    ) -> Result<ControllerTerminalDrain, PlaylistControllerInvariantViolation> {
        if let Some(violation) = self.fatal_invariant {
            return Err(violation);
        }
        let Some(state) = self.install_state.take() else {
            return self
                .fatal_result(PlaylistControllerInvariantViolation::MissingInstalledTerminal);
        };
        let InstallState::AuthorizationInFlight {
            guarded,
            post_commit_intent,
        } = state
        else {
            self.install_state = Some(state);
            return self.fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        if guarded.request_id != request_id {
            self.install_state = Some(InstallState::AuthorizationInFlight {
                guarded,
                post_commit_intent,
            });
            return self
                .fatal_result(PlaylistControllerInvariantViolation::TerminalRequestMismatch);
        }
        if guarded.player_request_id != player_request_id {
            self.install_state = Some(InstallState::AuthorizationInFlight {
                guarded,
                post_commit_intent,
            });
            return self.fatal_result(PlaylistControllerInvariantViolation::PlayerRequestMismatch);
        }
        let Some(next_dirty) = self.dirty_revision.checked_next() else {
            self.install_state = Some(InstallState::AuthorizationInFlight {
                guarded,
                post_commit_intent,
            });
            return self.fatal_result(
                PlaylistControllerInvariantViolation::DirtyRevisionExhaustedAfterPlayerCommit,
            );
        };
        let Some(lineage_id) = self.allocate_lineage() else {
            self.install_state = Some(InstallState::AuthorizationInFlight {
                guarded,
                post_commit_intent,
            });
            return self
                .fatal_result(PlaylistControllerInvariantViolation::LineageIdentityExhausted);
        };
        let GuardedInstall {
            target_item_id,
            token,
            desired_modes,
            queue_revision_before_commit,
            ..
        } = guarded;
        let commit = token.commit(&mut self.queue);
        let structural_changed = commit.structural_changed;
        if commit.manual_navigation {
            self.manual_navigation_cursor.commit_finished();
        }
        let queue_changed = self.queue.revision_snapshot() != queue_revision_before_commit;
        let committed_item_id = Some(commit.traversal_current.item_id());
        if target_item_id.is_some() && target_item_id != committed_item_id {
            self.set_fatal(PlaylistControllerInvariantViolation::TerminalRequestMismatch);
            return Err(PlaylistControllerInvariantViolation::TerminalRequestMismatch);
        }
        let active_media = ActiveMediaIdentity::installed(
            committed_item_id,
            lineage_id,
            media_instance_id,
            binding_generation,
        );
        self.active_media = Some(active_media);
        self.release_detached_tombstone_for_new_lineage(active_media);
        self.automatic_install_committed(active_media);
        self.stop_after_current = None;
        self.pending_target = None;
        if let Some(item_id) = committed_item_id {
            self.runtime_errors.remove(&item_id);
        }
        if structural_changed {
            let Some(next_structural_revision) = self.structural_revision.checked_next() else {
                self.set_fatal(PlaylistControllerInvariantViolation::DeferredModeApplicationFailed);
                return Err(PlaylistControllerInvariantViolation::DeferredModeApplicationFailed);
            };
            self.structural_revision = next_structural_revision;
        }
        let mut dirty = queue_changed.then(|| self.commit_dirty(next_dirty));
        if let Some(mode_dirty) = self.apply_desired_modes(desired_modes)? {
            dirty = Some(mode_dirty);
        }
        self.publish_view(structural_changed);
        Ok(ControllerTerminalDrain {
            request_id,
            active_media: Some(active_media),
            dirty,
            deferred_intent: post_commit_intent,
            resolution: ControllerTerminalResolution::Installed,
        })
    }

    /// Любой non-Installed terminal после enqueue barrier-а является fatal invariant.
    pub(crate) fn report_terminal_without_installed(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> PlaylistControllerInvariantViolation {
        let violation = if self.install_state.as_ref().is_some_and(|state| {
            state.request_id() == request_id
                && state.phase() == ControllerInstallPhase::AuthorizationInFlight
        }) {
            PlaylistControllerInvariantViolation::MissingInstalledTerminal
        } else {
            PlaylistControllerInvariantViolation::UnexpectedInstallPhase
        };
        self.set_fatal(violation);
        violation
    }

    /// До dispatch token abort-ится; после dispatch intent ждёт authoritative winner.
    pub(crate) fn request_lifecycle_intent(
        &mut self,
        intent: DeferredControllerIntent,
    ) -> Result<LifecycleIntentOutcome, PlaylistControllerInvariantViolation> {
        self.request_deferred_intent(intent)
    }

    /// D52 update остаётся разрешённым: controller только подтверждает exact request correlation.
    pub(crate) fn accepts_playback_intent_update(&self, request_id: MediaOpenRequestId) -> bool {
        self.install_state
            .as_ref()
            .is_some_and(|state| state.request_id() == request_id)
    }

    /// Во время guard сохраняется один desired value; вне guard применяется сразу.
    pub(crate) fn request_queue_modes(
        &mut self,
        desired: DesiredQueueModes,
    ) -> Result<Option<PlaylistDirtySignal>, PlaylistControllerInvariantViolation> {
        if let Some(violation) = self.fatal_invariant {
            return Err(violation);
        }
        if let Some(state) = &mut self.install_state {
            match state {
                InstallState::ReservedAwaitingAuthorization(guarded)
                | InstallState::AuthorizationDispatchPending { guarded, .. }
                | InstallState::AuthorizationInFlight { guarded, .. } => {
                    guarded.desired_modes = Some(desired);
                    return Ok(None);
                }
                InstallState::AwaitingReady(_) => {}
            }
        }
        let dirty = self.apply_desired_modes(Some(desired))?;
        self.publish_view(false);
        Ok(dirty)
    }

    fn apply_desired_modes(
        &mut self,
        desired: Option<DesiredQueueModes>,
    ) -> Result<Option<PlaylistDirtySignal>, PlaylistControllerInvariantViolation> {
        let Some(desired) = desired else {
            return Ok(None);
        };
        let persistent_change = self.repeat_mode != desired.repeat_mode
            || self.queue.shuffle_enabled() != desired.shuffle_enabled;
        self.protected_modes_generation = desired.protected_runtime_generation;
        if !persistent_change {
            return Ok(None);
        }
        let next_dirty = self.dirty_revision.checked_next().ok_or_else(|| {
            self.set_fatal(
                PlaylistControllerInvariantViolation::DirtyRevisionExhaustedAfterPlayerCommit,
            );
            PlaylistControllerInvariantViolation::DirtyRevisionExhaustedAfterPlayerCommit
        })?;
        let shuffle_result = if desired.shuffle_enabled {
            self.queue.enable_shuffle()
        } else {
            self.queue.disable_shuffle()
        };
        if let Err(error) = shuffle_result {
            let violation = match error {
                ShuffleToggleError::InstallCommitLinearizing
                | ShuffleToggleError::TraversalRevisionExhausted => {
                    PlaylistControllerInvariantViolation::DeferredModeApplicationFailed
                }
            };
            self.set_fatal(violation);
            return Err(violation);
        }
        self.repeat_mode = desired.repeat_mode;
        Ok(Some(self.commit_dirty(next_dirty)))
    }

    fn fatal_ready(
        &mut self,
        violation: PlaylistControllerInvariantViolation,
    ) -> InstallReadyOutcome {
        self.set_fatal(violation);
        InstallReadyOutcome::Fatal(violation)
    }

    fn fatal_result<T>(
        &mut self,
        violation: PlaylistControllerInvariantViolation,
    ) -> Result<T, PlaylistControllerInvariantViolation> {
        self.set_fatal(violation);
        Err(violation)
    }
}
