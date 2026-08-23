//! Process-lifetime active-media suspend/resume checkpoint.
//!
//! Здесь нет disk DTO и renderer/GPU owners. Runtime хранит только reconstructible source,
//! app lineage, последнюю подтверждённую позицию и stable transport intent.

use std::time::Duration;

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackState, PlayerSnapshot};

use super::controller::{ControllerInstallPhase, PlaylistControllerInvariantViolation};
use super::identity::ActiveMediaIdentity;
use super::{
    LifecycleTimelineCheckpointPosition, PlaylistBindingGeneration, PlaylistLoadGateState,
    PlaylistMediaOpenGateError, PlaylistRuntime, PlaylistRuntimeBinding,
};
use crate::media_open::{ActiveMediaSource, MediaOpenRequestId};
use crate::media_open::{MediaOpenPhase, MediaOpenTerminalOutcome};

/// Stable intent, который разрешено восстановить только после seek/non-seekable resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumePlaybackIntent {
    Playing,
    Paused,
}

/// Runtime-only checkpoint; старые instance/binding используются только для stale rejection.
#[derive(Clone)]
pub(crate) struct SuspendedMediaCheckpoint {
    pub(crate) source: ActiveMediaSource,
    pub(crate) expected_active: ActiveMediaIdentity,
    pub(crate) position: SuspendedTimelineResumePosition,
    pub(crate) intent: ResumePlaybackIntent,
    pub(crate) consumed_eof_edge: bool,
    pub(crate) terminal_failure: bool,
}

/// Typed bounded warning: source открыт, но exact resume position недоступна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResumePositionWarning {
    pub(crate) requested_position: Duration,
    pub(crate) available_position: Duration,
}

/// Recoverable lifecycle failure не запускает playlist error-policy navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeCheckpointError {
    MissingReopenableSource,
    StalePlayerBinding,
    StalePlayerInstance,
    PendingInstallInvariant,
    MissingAuthorizationResolution,
    MissingInstalledTerminal,
    PreparationFailed,
    InstallFailed,
    SeekFailed,
    CandidateReleaseFailed,
    IntentRestoreFailed,
    ControllerInvariant,
}

/// Read model lifecycle checkpoint-а; payload остаётся secret-safe и bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeCheckpointStatus {
    Empty,
    Ready,
    TerminalFailureNeedsExplicitRetry,
    Resuming,
    RecoverableFailure(ResumeCheckpointError),
    ResumedWithPositionWarning(ResumePositionWarning),
}

/// Suspend outcome не выдаёт missing/fatal invariant за successful checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuspendCheckpointOutcome {
    NoActiveMedia,
    Captured,
    CapturedTerminalFailure,
}

/// Cloneable resume input; renderer-bound candidate ownership сюда не попадает.
#[derive(Clone)]
pub(crate) struct ResumeAttempt {
    pub(crate) source: ActiveMediaSource,
    pub(crate) expected_active: ActiveMediaIdentity,
    pub(crate) position: SuspendedTimelineResumePosition,
    pub(crate) intent: ResumePlaybackIntent,
}

/// Explicit suspend/settings timeline intent без fake live position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuspendedTimelineResumePosition {
    /// Fresh live opener остаётся на своей authoritative logical позиции.
    KeepStart,
    /// Static media восстанавливает подтверждённую позицию.
    SeekTo(Duration),
}

/// Process-lifetime storage active source + optional suspended checkpoint.
#[derive(Default)]
pub(crate) struct SuspendedMediaState {
    active_source: Option<ActiveMediaSource>,
    checkpoint: Option<SuspendedMediaCheckpoint>,
    status: Option<ResumeCheckpointStatus>,
}

impl SuspendedMediaState {
    fn status(&self) -> ResumeCheckpointStatus {
        self.status.unwrap_or(ResumeCheckpointStatus::Empty)
    }
}

impl PlaylistRuntime {
    /// Same-lineage staging передаёт player-у exact old instance без app-side position.
    pub(crate) fn stage_same_lineage_media_open_at_player(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
        intent: crate::media_open::MediaOpenInstallIntent,
        video_resource_port: player_core::MediaInstallVideoResourcePort,
        expected_old_media_instance_id: player_core::MediaInstanceId,
    ) -> Result<player_core::MediaInstallRequestId, PlaylistMediaOpenGateError> {
        if !matches!(self.load_gate, PlaylistLoadGateState::Open(_)) {
            return Err(PlaylistMediaOpenGateError::LoadDecisionPending);
        }
        self.media_open
            .stage_same_lineage_at_player(
                request_id,
                intent,
                video_resource_port,
                expected_old_media_instance_id,
            )
            .map_err(PlaylistMediaOpenGateError::Coordinator)
    }

    pub(crate) fn prepare_same_lineage_media_open_position(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
    ) -> Result<(), PlaylistMediaOpenGateError> {
        if !matches!(self.load_gate, PlaylistLoadGateState::Open(_)) {
            return Err(PlaylistMediaOpenGateError::LoadDecisionPending);
        }
        self.media_open
            .prepare_same_lineage_position(request_id)
            .map_err(PlaylistMediaOpenGateError::Coordinator)
    }

    pub(crate) fn authorize_ready_same_lineage_media_open(
        &mut self,
        request_id: crate::media_open::MediaOpenRequestId,
    ) -> Result<(), PlaylistMediaOpenGateError> {
        if !matches!(self.load_gate, PlaylistLoadGateState::Open(_)) {
            return Err(PlaylistMediaOpenGateError::LoadDecisionPending);
        }
        self.media_open
            .authorize_ready_same_lineage(request_id)
            .map_err(PlaylistMediaOpenGateError::Coordinator)
    }

    /// Terminal-resolve-ит pending install до того, как shell снимет новый player snapshot.
    pub(crate) fn resolve_pending_media_for_suspend(
        &mut self,
    ) -> Result<(), ResumeCheckpointError> {
        let lifecycle = self
            .controller
            .as_mut()
            .ok_or(ResumeCheckpointError::ControllerInvariant)?
            .request_lifecycle_intent(super::controller::DeferredControllerIntent::Suspend)
            .map_err(|_| ResumeCheckpointError::ControllerInvariant)?;
        match lifecycle {
            super::controller::LifecycleIntentOutcome::Immediate {
                aborted_request_id, ..
            } => {
                if let Some(request_id) = aborted_request_id {
                    self.resolve_media_open_for_suspend(request_id)?;
                }
            }
            super::controller::LifecycleIntentOutcome::CancelPendingRequest {
                request_id, ..
            }
            | super::controller::LifecycleIntentOutcome::AwaitAuthorizationResolution {
                request_id,
            }
            | super::controller::LifecycleIntentOutcome::AwaitInstalled { request_id } => {
                self.resolve_media_open_for_suspend(request_id)?;
            }
            super::controller::LifecycleIntentOutcome::NoPendingInstall => {
                // External strong-open preparation может ещё жить в coordinator-е до того,
                // как controller получил install guard. Suspend обязан terminal-resolve-ить
                // и такой request до detach, а не оставлять скрытую подготовку в runtime.
                if let Some(snapshot) = self.media_open_snapshot() {
                    self.resolve_media_open_for_suspend(snapshot.request_id)?;
                }
            }
            super::controller::LifecycleIntentOutcome::Fatal(_) => {
                return Err(ResumeCheckpointError::ControllerInvariant);
            }
        }
        Ok(())
    }

    fn resolve_media_open_for_suspend(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<(), ResumeCheckpointError> {
        let initial = self
            .media_open_snapshot()
            .ok_or(ResumeCheckpointError::MissingAuthorizationResolution)?;
        if initial.request_id != request_id {
            return Err(ResumeCheckpointError::MissingAuthorizationResolution);
        }
        if initial.phase != MediaOpenPhase::EnqueuedAtPlayerOwner {
            self.cancel_media_open(
                request_id,
                player_core::MediaInstallCancellationCause::LifecycleSuspended,
            )
            .map_err(|_| ResumeCheckpointError::MissingAuthorizationResolution)?;
        }

        loop {
            let snapshot = self
                .media_open_snapshot()
                .ok_or(ResumeCheckpointError::MissingAuthorizationResolution)?;
            if snapshot.request_id != request_id {
                return Err(ResumeCheckpointError::MissingAuthorizationResolution);
            }
            if self.controller.as_ref().is_some_and(|controller| {
                controller.install_phase()
                    == Some(ControllerInstallPhase::AuthorizationDispatchPending)
            }) && let Some(resolution) = snapshot.authorization_resolution
            {
                self.controller
                    .as_mut()
                    .ok_or(ResumeCheckpointError::ControllerInvariant)?
                    .resolve_authorization_dispatch(request_id, resolution)
                    .map_err(|_| ResumeCheckpointError::ControllerInvariant)?;
            }
            match snapshot.phase {
                MediaOpenPhase::Accepted
                | MediaOpenPhase::Preparing
                | MediaOpenPhase::PlayerStaging
                | MediaOpenPhase::AuthorizationDispatchPending
                | MediaOpenPhase::EnqueuedAtPlayerOwner => {
                    self.wait_for_media_open_progress(request_id)
                        .map_err(|_| ResumeCheckpointError::MissingAuthorizationResolution)?;
                }
                MediaOpenPhase::Installed => {
                    let terminal = self
                        .take_media_open_terminal(request_id)
                        .map_err(|_| ResumeCheckpointError::MissingInstalledTerminal)?
                        .ok_or(ResumeCheckpointError::MissingInstalledTerminal)?;
                    let MediaOpenTerminalOutcome::Installed {
                        player_request_id,
                        descriptor,
                        completion,
                        ..
                    } = terminal
                    else {
                        return Err(ResumeCheckpointError::MissingInstalledTerminal);
                    };
                    let player_core::MediaInstallCompletion::Installed {
                        media_instance_id, ..
                    } = completion
                    else {
                        return Err(ResumeCheckpointError::MissingInstalledTerminal);
                    };
                    let binding_generation = self
                        .current_binding()
                        .ok_or(ResumeCheckpointError::StalePlayerBinding)?
                        .binding_generation();
                    let active = self
                        .on_playlist_installed(
                            request_id,
                            player_request_id,
                            media_instance_id,
                            binding_generation,
                        )
                        .map_err(|_| ResumeCheckpointError::ControllerInvariant)?
                        .active_media
                        .ok_or(ResumeCheckpointError::ControllerInvariant)?;
                    self.suspended_media.active_source = Some(descriptor.active_source());
                    if self.removal_undo.as_ref().is_some_and(|undo| {
                        undo.active_lineage_at_removal() != Some(active.lineage_id())
                    }) {
                        self.removal_undo = None;
                    }
                    return Ok(());
                }
                MediaOpenPhase::Failed => {
                    let terminal = self
                        .take_media_open_terminal(request_id)
                        .map_err(|_| ResumeCheckpointError::MissingAuthorizationResolution)?
                        .ok_or(ResumeCheckpointError::MissingAuthorizationResolution)?;
                    return match terminal {
                        MediaOpenTerminalOutcome::Cancelled { .. } => Ok(()),
                        MediaOpenTerminalOutcome::FatalInvariant { .. } => {
                            Err(ResumeCheckpointError::MissingAuthorizationResolution)
                        }
                        _ => Err(ResumeCheckpointError::InstallFailed),
                    };
                }
                MediaOpenPhase::Prepared | MediaOpenPhase::ReadyToCommit => {
                    return Err(ResumeCheckpointError::MissingAuthorizationResolution);
                }
            }
        }
    }

    /// Регистрирует every successful strong install в единственном controller lineage owner-е.
    pub(crate) fn register_successful_strong_install(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        binding: PlaylistRuntimeBinding,
        source: ActiveMediaSource,
        install_intent: player_core::PlaybackIntent,
    ) -> Result<ActiveMediaIdentity, ResumeCheckpointError> {
        self.validate_binding(binding)
            .map_err(|_| ResumeCheckpointError::StalePlayerBinding)?;
        let binding_generation = binding.binding_generation();
        let controller = self
            .controller
            .as_ref()
            .ok_or(ResumeCheckpointError::ControllerInvariant)?;
        let pending_request = controller.install_request_id();
        let install_phase = controller.install_phase();
        let active_media = match (install_phase, pending_request) {
            (Some(ControllerInstallPhase::AuthorizationInFlight), Some(pending_request))
                if pending_request == request_id =>
            {
                self.on_playlist_installed(
                    request_id,
                    player_request_id,
                    media_instance_id,
                    binding_generation,
                )
                .map_err(|_| ResumeCheckpointError::ControllerInvariant)?
                .active_media
                .ok_or(ResumeCheckpointError::ControllerInvariant)?
            }
            (None, None) => self
                .controller
                .as_mut()
                .ok_or(ResumeCheckpointError::ControllerInvariant)?
                .register_external_strong_install(
                    media_instance_id,
                    binding_generation,
                    match install_intent {
                        player_core::PlaybackIntent::StartPlaying => {
                            super::controller::StablePlaybackIntent::Playing
                        }
                        player_core::PlaybackIntent::StartPaused => {
                            super::controller::StablePlaybackIntent::Paused
                        }
                    },
                )
                .map_err(|_| ResumeCheckpointError::ControllerInvariant)?,
            _ => return Err(ResumeCheckpointError::PendingInstallInvariant),
        };
        self.suspended_media.active_source = Some(source);
        self.suspended_media.checkpoint = None;
        self.suspended_media.status = Some(ResumeCheckpointStatus::Empty);
        if self
            .removal_undo
            .as_ref()
            .is_some_and(|undo| undo.active_lineage_at_removal() != Some(active_media.lineage_id()))
        {
            self.removal_undo = None;
        }
        Ok(active_media)
    }

    /// Согласует controller/source projections после exact release неуспешного Installed.
    pub(crate) fn reconcile_released_post_installed_candidate(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
    ) -> Result<(), ResumeCheckpointError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(ResumeCheckpointError::ControllerInvariant)?;
        let dirty_before = controller.dirty_revision();
        controller
            .reconcile_released_post_installed_candidate(request_id, player_request_id)
            .map_err(|_| ResumeCheckpointError::ControllerInvariant)?;
        self.publish_controller_mutation_if_dirty(dirty_before);
        self.suspended_media.active_source = None;
        self.suspended_media.checkpoint = None;
        self.suspended_media.status = Some(ResumeCheckpointStatus::Empty);
        Ok(())
    }

    /// Release dispatch/receipt failure переводит controller в явный fatal invariant.
    pub(crate) fn report_post_installed_compensation_failure(&mut self) {
        if let Some(controller) = self.controller.as_mut() {
            controller.report_post_installed_compensation_failure();
        }
    }

    /// Explicit different Play/open supersede-ит failed/suspended checkpoint immediately.
    pub(crate) fn supersede_suspended_media_checkpoint(&mut self) {
        self.suspended_media.checkpoint = None;
        self.suspended_media.status = Some(ResumeCheckpointStatus::Empty);
    }

    /// Захватывает согласованный source/identity/snapshot до detach player binding-а.
    #[cfg(test)]
    pub(crate) fn capture_suspended_media_checkpoint(
        &mut self,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
    ) -> Result<SuspendCheckpointOutcome, ResumeCheckpointError> {
        self.capture_suspended_media_checkpoint_with_timeline_position(
            binding,
            snapshot,
            LifecycleTimelineCheckpointPosition::LatestSnapshot,
        )
    }

    /// Захватывает checkpoint после typed settlement pending timeline seek-а.
    pub(crate) fn capture_suspended_media_checkpoint_after_seek_settlement(
        &mut self,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
        timeline_position: LifecycleTimelineCheckpointPosition,
    ) -> Result<SuspendCheckpointOutcome, ResumeCheckpointError> {
        self.capture_suspended_media_checkpoint_with_timeline_position(
            binding,
            snapshot,
            timeline_position,
        )
    }

    /// Общая реализация сохраняет source/identity/intent независимо от источника позиции.
    fn capture_suspended_media_checkpoint_with_timeline_position(
        &mut self,
        binding: PlaylistRuntimeBinding,
        snapshot: &PlayerSnapshot,
        timeline_position: LifecycleTimelineCheckpointPosition,
    ) -> Result<SuspendCheckpointOutcome, ResumeCheckpointError> {
        self.validate_binding(binding)
            .map_err(|_| ResumeCheckpointError::StalePlayerBinding)?;
        let controller = self
            .controller
            .as_mut()
            .ok_or(ResumeCheckpointError::ControllerInvariant)?;
        let Some(active_media) = controller.active_media() else {
            self.suspended_media.checkpoint = None;
            self.suspended_media.status = Some(ResumeCheckpointStatus::Empty);
            return Ok(SuspendCheckpointOutcome::NoActiveMedia);
        };
        let Some(source) = self.suspended_media.active_source.clone() else {
            return Err(ResumeCheckpointError::MissingReopenableSource);
        };
        if active_media.player_binding_generation() != binding.binding_generation() {
            return Err(ResumeCheckpointError::StalePlayerBinding);
        }
        if snapshot.media_instance_id != Some(active_media.media_instance_id()) {
            return Err(ResumeCheckpointError::StalePlayerInstance);
        }

        let terminal_failure = snapshot.playback_state == PlaybackState::Failed;
        let consumed_eof_edge = snapshot.playback_state == PlaybackState::Ended;
        if terminal_failure || consumed_eof_edge {
            let consumed =
                controller.consume_terminal_edge_for_suspend(active_media, snapshot.playback_state);
            if !consumed {
                return Err(ResumeCheckpointError::ControllerInvariant);
            }
        }
        let stable_intent = controller.stable_playback_intent();
        let intent = if consumed_eof_edge {
            ResumePlaybackIntent::Paused
        } else {
            match stable_intent {
                super::controller::StablePlaybackIntent::Playing => ResumePlaybackIntent::Playing,
                super::controller::StablePlaybackIntent::Paused => ResumePlaybackIntent::Paused,
            }
        };
        let position = if snapshot.timeline.mode == media_core::TimelineMode::Live {
            SuspendedTimelineResumePosition::KeepStart
        } else if consumed_eof_edge {
            SuspendedTimelineResumePosition::SeekTo(
                snapshot.duration.unwrap_or(snapshot.current_position),
            )
        } else if let Some(settled_position) = timeline_position.explicit_position() {
            SuspendedTimelineResumePosition::SeekTo(settled_position)
        } else {
            SuspendedTimelineResumePosition::SeekTo(snapshot.current_position)
        };
        self.suspended_media.checkpoint = Some(SuspendedMediaCheckpoint {
            source,
            expected_active: active_media,
            position,
            intent,
            consumed_eof_edge,
            terminal_failure,
        });
        self.suspended_media.status = Some(if terminal_failure {
            ResumeCheckpointStatus::TerminalFailureNeedsExplicitRetry
        } else {
            ResumeCheckpointStatus::Ready
        });
        Ok(if terminal_failure {
            SuspendCheckpointOutcome::CapturedTerminalFailure
        } else {
            SuspendCheckpointOutcome::Captured
        })
    }

    /// Начинает automatic resume либо explicit retry terminal/recoverable failure.
    pub(crate) fn begin_suspended_media_resume(
        &mut self,
        explicit_retry: bool,
    ) -> Option<ResumeAttempt> {
        let checkpoint = self.suspended_media.checkpoint.as_ref()?;
        let retryable = match self.suspended_media.status() {
            ResumeCheckpointStatus::TerminalFailureNeedsExplicitRetry => {
                checkpoint.terminal_failure
            }
            ResumeCheckpointStatus::RecoverableFailure(_) => true,
            _ => false,
        };
        let allowed = matches!(self.suspended_media.status(), ResumeCheckpointStatus::Ready)
            || (explicit_retry && retryable);
        if !allowed {
            return None;
        }
        let attempt = ResumeAttempt {
            source: checkpoint.source.clone(),
            expected_active: checkpoint.expected_active,
            position: checkpoint.position,
            intent: checkpoint.intent,
        };
        let _consumed_eof_edge = checkpoint.consumed_eof_edge;
        self.suspended_media.status = Some(ResumeCheckpointStatus::Resuming);
        Some(attempt)
    }

    /// Failure сохраняет checkpoint и не изменяет queue/traversal/lineage.
    pub(crate) fn fail_suspended_media_resume(&mut self, error: ResumeCheckpointError) {
        if self.suspended_media.checkpoint.is_some() {
            self.suspended_media.status = Some(ResumeCheckpointStatus::RecoverableFailure(error));
        }
    }

    /// Repeated suspend после terminal cancellation re-arms тот же checkpoint exactly once.
    pub(crate) fn pause_suspended_media_resume_for_suspend(&mut self) {
        if let Some(checkpoint) = self.suspended_media.checkpoint.as_ref() {
            self.suspended_media.status = Some(if checkpoint.terminal_failure {
                ResumeCheckpointStatus::TerminalFailureNeedsExplicitRetry
            } else {
                ResumeCheckpointStatus::Ready
            });
        }
    }

    /// Successful install→seek→intent atomically публикует same-lineage new instance.
    pub(crate) fn complete_suspended_media_resume(
        &mut self,
        expected_active: ActiveMediaIdentity,
        media_instance_id: MediaInstanceId,
        binding_generation: PlaylistBindingGeneration,
        warning: Option<ResumePositionWarning>,
    ) -> Result<ActiveMediaIdentity, ResumeCheckpointError> {
        let consumed_eof_edge = self
            .suspended_media
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.consumed_eof_edge);
        let outcome = self
            .controller
            .as_mut()
            .ok_or(ResumeCheckpointError::ControllerInvariant)?
            .rebind_active_media_same_lineage(
                expected_active,
                media_instance_id,
                binding_generation,
            );
        let active_media = match outcome {
            super::controller::ControllerActiveMediaRebindOutcome::Rebound { active_media } => {
                active_media
            }
            super::controller::ControllerActiveMediaRebindOutcome::Stale { .. } => {
                return Err(ResumeCheckpointError::StalePlayerInstance);
            }
        };
        if consumed_eof_edge
            && !self
                .controller
                .as_mut()
                .ok_or(ResumeCheckpointError::ControllerInvariant)?
                .carry_consumed_eof_edge_after_rebind(expected_active, active_media)
        {
            return Err(ResumeCheckpointError::ControllerInvariant);
        }
        self.suspended_media.checkpoint = None;
        self.suspended_media.status = Some(match warning {
            Some(warning) => ResumeCheckpointStatus::ResumedWithPositionWarning(warning),
            None => ResumeCheckpointStatus::Empty,
        });
        Ok(active_media)
    }

    /// S25/S36 публикуют новый player instance той же lineage без queue/traversal commit-а.
    pub(crate) fn complete_same_item_media_switch(
        &mut self,
        expected_active: ActiveMediaIdentity,
        media_instance_id: MediaInstanceId,
        binding: PlaylistRuntimeBinding,
        source: ActiveMediaSource,
    ) -> Result<ActiveMediaIdentity, ResumeCheckpointError> {
        self.validate_binding(binding)
            .map_err(|_| ResumeCheckpointError::StalePlayerBinding)?;
        let outcome = self
            .controller
            .as_mut()
            .ok_or(ResumeCheckpointError::ControllerInvariant)?
            .rebind_active_media_same_lineage(
                expected_active,
                media_instance_id,
                binding.binding_generation(),
            );
        let active_media = match outcome {
            super::controller::ControllerActiveMediaRebindOutcome::Rebound { active_media } => {
                active_media
            }
            super::controller::ControllerActiveMediaRebindOutcome::Stale { .. } => {
                return Err(ResumeCheckpointError::StalePlayerInstance);
            }
        };
        self.suspended_media.active_source = Some(source);
        Ok(active_media)
    }

    /// Typed read model для UI/tests; checkpoint payload и source не раскрываются.
    pub(crate) fn suspended_media_status(&self) -> ResumeCheckpointStatus {
        self.suspended_media.status()
    }

    /// Lifecycle shell различает fresh capture и повторный suspend уже сохранённой lineage.
    pub(crate) fn has_suspended_media_checkpoint(&self) -> bool {
        self.suspended_media.checkpoint.is_some()
    }

    /// Current exact binding для renderer-bound resume orchestration.
    pub(crate) fn current_binding(&self) -> Option<PlaylistRuntimeBinding> {
        match self.lifecycle {
            super::PlaylistRuntimeLifecycle::Bound(binding) => Some(binding),
            _ => None,
        }
    }
}

impl From<PlaylistControllerInvariantViolation> for ResumeCheckpointError {
    fn from(_: PlaylistControllerInvariantViolation) -> Self {
        Self::ControllerInvariant
    }
}

#[cfg(test)]
mod tests;
