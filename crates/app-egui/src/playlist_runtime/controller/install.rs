//! D08/D39 reservation/authorization state machine playlist controller-а.

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackIntentRevision};
use playlist_core::{
    PrepareReservedMutationError, PreparedQueueMutationToken, QueueRevisionSnapshot, RepeatMode,
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
    pub mutation: ReservedQueueMutation,
}

/// Persistent queue modes и runtime protected generation coalesce-ятся в один value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DesiredQueueModes {
    pub repeat_mode: RepeatMode,
    pub shuffle_enabled: bool,
    pub protected_runtime_generation: u64,
}

/// Один deferred intent вместо command FIFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredControllerIntent {
    Stop,
    Suspend,
    Shutdown,
}

impl DeferredControllerIntent {
    fn priority(self) -> u8 {
        match self {
            Self::Stop => 0,
            Self::Suspend => 1,
            Self::Shutdown => 2,
        }
    }
}

/// Dispatch-pending slot подчёркивает, что barrier winner ещё неизвестен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BarrierRaceIntent(DeferredControllerIntent);

impl BarrierRaceIntent {
    pub(crate) const fn intent(self) -> DeferredControllerIntent {
        self.0
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleIntentOutcome {
    Immediate {
        intent: DeferredControllerIntent,
    },
    CancelPendingRequest {
        request_id: MediaOpenRequestId,
        intent: DeferredControllerIntent,
    },
    AwaitAuthorizationResolution {
        request_id: MediaOpenRequestId,
    },
    AwaitInstalled {
        request_id: MediaOpenRequestId,
    },
    NoPendingInstall,
    Fatal(PlaylistControllerInvariantViolation),
}

/// Результат terminal drain уже соблюдает commit/abort -> modes -> intent ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControllerTerminalDrain {
    pub request_id: MediaOpenRequestId,
    pub active_media: Option<ActiveMediaIdentity>,
    pub dirty: Option<PlaylistDirtySignal>,
    pub deferred_intent: Option<DeferredControllerIntent>,
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

    const fn request_id(&self) -> MediaOpenRequestId {
        match self {
            Self::AwaitingReady(state) => state.request.request_id,
            Self::ReservedAwaitingAuthorization(state)
            | Self::AuthorizationDispatchPending { guarded: state, .. }
            | Self::AuthorizationInFlight { guarded: state, .. } => state.request_id,
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
    token: PreparedQueueMutationToken,
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

    pub(crate) fn install_phase(&self) -> Option<ControllerInstallPhase> {
        self.install_state.as_ref().map(InstallState::phase)
    }

    /// Matching Ready выполняет все domain fallible checks до authorization dispatch.
    pub(crate) fn on_ready_to_commit(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> InstallReadyOutcome {
        if let Some(violation) = self.fatal_invariant {
            return InstallReadyOutcome::Fatal(violation);
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
        let token = match self
            .queue
            .prepare_reserved_mutation(expected_queue_revision, mutation)
        {
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
                queue_revision_before_commit: expected_queue_revision,
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
            AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue { .. }
            | AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue { .. } => {
                let GuardedInstall {
                    request_id,
                    token,
                    desired_modes,
                    queue_revision_before_commit: _,
                    ..
                } = guarded;
                self.queue.abort_reserved(token);
                self.pending_target = None;
                let dirty = self.apply_desired_modes(desired_modes)?;
                let deferred_intent = race_intent.map(BarrierRaceIntent::intent);
                self.publish_view(false);
                Ok(Some(ControllerTerminalDrain {
                    request_id,
                    active_media: self.active_media,
                    dirty,
                    deferred_intent,
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
        let commit = self.queue.commit_reserved(token);
        let structural_changed = !commit.allocated_item_ids().as_slice().is_empty();
        let queue_changed = self.queue.revision_snapshot() != queue_revision_before_commit;
        let committed_item_id = Some(commit.traversal_current().item_id());
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
        if let Some(violation) = self.fatal_invariant {
            return Err(violation);
        }
        let Some(state) = self.install_state.take() else {
            return Ok(LifecycleIntentOutcome::NoPendingInstall);
        };
        match state {
            InstallState::AwaitingReady(awaiting) => {
                let request_id = awaiting.request.request_id;
                self.pending_target = None;
                self.publish_view(false);
                Ok(LifecycleIntentOutcome::CancelPendingRequest { request_id, intent })
            }
            InstallState::ReservedAwaitingAuthorization(guarded) => {
                let GuardedInstall {
                    token,
                    desired_modes,
                    queue_revision_before_commit: _,
                    ..
                } = guarded;
                self.queue.abort_reserved(token);
                self.pending_target = None;
                self.apply_desired_modes(desired_modes)?;
                self.publish_view(false);
                Ok(LifecycleIntentOutcome::Immediate { intent })
            }
            InstallState::AuthorizationDispatchPending {
                guarded,
                race_intent,
            } => {
                let request_id = guarded.request_id;
                let race_intent = Some(BarrierRaceIntent(select_intent(
                    race_intent.map(BarrierRaceIntent::intent),
                    intent,
                )));
                self.install_state = Some(InstallState::AuthorizationDispatchPending {
                    guarded,
                    race_intent,
                });
                self.publish_view(false);
                Ok(LifecycleIntentOutcome::AwaitAuthorizationResolution { request_id })
            }
            InstallState::AuthorizationInFlight {
                guarded,
                post_commit_intent,
            } => {
                let request_id = guarded.request_id;
                self.install_state = Some(InstallState::AuthorizationInFlight {
                    guarded,
                    post_commit_intent: Some(select_intent(post_commit_intent, intent)),
                });
                self.publish_view(false);
                Ok(LifecycleIntentOutcome::AwaitInstalled { request_id })
            }
        }
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

fn select_intent(
    existing: Option<DeferredControllerIntent>,
    incoming: DeferredControllerIntent,
) -> DeferredControllerIntent {
    match existing {
        Some(current) if current.priority() >= incoming.priority() => current,
        _ => incoming,
    }
}
