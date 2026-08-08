//! Bounded process-lifetime mechanism подготовки и strong player install.
mod player_staging;

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use player_core::{
    MediaInstallCancellationCause, MediaInstallCompletion, MediaInstallControlOutcome,
    MediaInstallPhase, MediaInstallRequestId, MediaInstallVideoResourcePort, PlaybackIntentUpdate,
    PlaybackIntentUpdateReceipt, PlayerCommandSender,
};

use crate::app_wake::AppWakePort;

use super::executor::{
    PreparationCancellation, PreparationExecutor, PreparationResult, PreparationResultSlot,
    PreparationWork,
};
use super::player_port::{ControlReceiptPort, InstallReceiptPort, MediaOpenPlayerPort};

use super::{
    AuthorizationDispatchResolution, CancellationDispatchOutcome, MediaOpenClientKey,
    MediaOpenCommandError, MediaOpenCompletionDriveError, MediaOpenInstallIntent,
    MediaOpenInvariantViolation, MediaOpenPhase, MediaOpenPositionPreparation, MediaOpenRequestId,
    MediaOpenSnapshot, MediaOpenSourceRequest, MediaOpenStartError, MediaOpenStartMode,
    MediaOpenStartOutcome, MediaOpenTerminalOutcome, MediaPreparationFailureKind,
    PreparedMediaDescriptor, PreparedMediaOpen, SafeMediaLabel,
    SameLineagePositionPreparationPhase,
};

enum PendingControl {
    Authorization(Box<dyn ControlReceiptPort>),
    Cancellation {
        cause: MediaInstallCancellationCause,
        receipt: Box<dyn ControlReceiptPort>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CancellationDispatchMode {
    NonBlocking,
    LosslessCleanup,
}

impl PendingControl {
    fn wait_until_outcome_available(&self) -> Result<(), ()> {
        match self {
            Self::Authorization(receipt) | Self::Cancellation { receipt, .. } => {
                receipt.wait_until_outcome_available()
            }
        }
    }
}

struct CurrentRequest {
    client_key: MediaOpenClientKey,
    request_id: MediaOpenRequestId,
    phase: MediaOpenPhase,
    cancellation: Arc<PreparationCancellation>,
    preparation_slot: Arc<PreparationResultSlot>,
    prepared_open: Option<PreparedMediaOpen>,
    descriptor: Option<PreparedMediaDescriptor>,
    player_request_id: Option<MediaInstallRequestId>,
    install_receipt: Option<Box<dyn InstallReceiptPort>>,
    pending_control: Option<PendingControl>,
    authorization_resolution: Option<AuthorizationDispatchResolution>,
    terminal: Option<MediaOpenTerminalOutcome>,
    safe_label: SafeMediaLabel,
    same_lineage_position: SameLineagePositionPreparationPhase,
}

/// Policy-neutral coordinator; controller identities и queue token здесь отсутствуют.
pub(crate) struct MediaOpenCoordinator {
    next_request_id: AtomicU64,
    executor: Arc<PreparationExecutor>,
    player_port: Option<Arc<dyn MediaOpenPlayerPort>>,
    current: Option<CurrentRequest>,
    shutting_down: bool,
}

impl MediaOpenCoordinator {
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        Self {
            next_request_id: AtomicU64::new(1),
            executor: PreparationExecutor::new(wake_port),
            player_port: None,
            current: None,
            shutting_down: false,
        }
    }

    pub(crate) fn attach_player(&mut self, player_sender: PlayerCommandSender) {
        self.player_port = Some(Arc::new(player_sender));
    }

    /// Suspend не забывает enqueue winner; pre-barrier request получает typed cancel.
    pub(crate) fn suspend_player_binding(&mut self) {
        let enqueued = self
            .current
            .as_ref()
            .is_some_and(|request| request.phase == MediaOpenPhase::EnqueuedAtPlayerOwner);
        if !enqueued {
            self.cancel_current_for_lifecycle(MediaInstallCancellationCause::LifecycleSuspended);
            self.player_port = None;
        }
    }

    pub(crate) fn start(
        &mut self,
        client_key: MediaOpenClientKey,
        source_request: MediaOpenSourceRequest,
        mode: MediaOpenStartMode,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        let safe_label = source_request.safe_label();
        self.start_with_task(client_key, mode, safe_label, move |cancellation| {
            super::preparation::prepare_source(source_request, cancellation)
        })
    }

    /// Принимает уже подготовленный source-owner-ом demuxer в тот же strong protocol.
    pub(crate) fn start_prepared(
        &mut self,
        client_key: MediaOpenClientKey,
        prepared_open: PreparedMediaOpen,
        safe_label: SafeMediaLabel,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        if self.shutting_down {
            return Err(MediaOpenStartError::ShuttingDown);
        }
        if self.current.is_some() {
            return Err(MediaOpenStartError::Busy);
        }

        let request_id = self.allocate_request_id();
        self.current = Some(CurrentRequest {
            client_key,
            request_id,
            phase: MediaOpenPhase::Prepared,
            cancellation: Arc::new(PreparationCancellation::new()),
            preparation_slot: Arc::new(PreparationResultSlot::new()),
            prepared_open: Some(prepared_open),
            descriptor: None,
            player_request_id: None,
            install_receipt: None,
            pending_control: None,
            authorization_resolution: None,
            terminal: None,
            safe_label,
            same_lineage_position: SameLineagePositionPreparationPhase::NotRequired,
        });
        Ok(MediaOpenStartOutcome::Accepted { request_id })
    }

    fn start_with_task(
        &mut self,
        client_key: MediaOpenClientKey,
        mode: MediaOpenStartMode,
        safe_label: SafeMediaLabel,
        task: impl FnOnce(&PreparationCancellation) -> PreparationResult + Send + 'static,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        if self.shutting_down {
            return Err(MediaOpenStartError::ShuttingDown);
        }
        if let Some(current) = &self.current {
            if mode == MediaOpenStartMode::CoalesceMatchingClient
                && current.client_key == client_key
                && current.terminal.is_none()
            {
                return Ok(MediaOpenStartOutcome::Coalesced {
                    request_id: current.request_id,
                });
            }
            return Err(MediaOpenStartError::Busy);
        }

        let request_id = self.allocate_request_id();
        let cancellation = Arc::new(PreparationCancellation::new());
        let preparation_slot = Arc::new(PreparationResultSlot::new());
        self.executor.submit_latest(PreparationWork::new(
            Arc::clone(&cancellation),
            Arc::clone(&preparation_slot),
            task,
        ))?;
        self.current = Some(CurrentRequest {
            client_key,
            request_id,
            phase: MediaOpenPhase::Accepted,
            cancellation,
            preparation_slot,
            prepared_open: None,
            descriptor: None,
            player_request_id: None,
            install_receipt: None,
            pending_control: None,
            authorization_resolution: None,
            terminal: None,
            safe_label,
            same_lineage_position: SameLineagePositionPreparationPhase::NotRequired,
        });
        Ok(MediaOpenStartOutcome::Accepted { request_id })
    }

    /// Caller-commanded supersede заменяет только pre-player preparation.
    pub(crate) fn supersede_prepared_or_preparing(
        &mut self,
        expected_request_id: MediaOpenRequestId,
        client_key: MediaOpenClientKey,
        source_request: MediaOpenSourceRequest,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        let Some(current) = self.current.as_ref() else {
            return self.start(client_key, source_request, MediaOpenStartMode::RequireIdle);
        };
        if current.request_id != expected_request_id
            || !matches!(
                current.phase,
                MediaOpenPhase::Accepted | MediaOpenPhase::Preparing | MediaOpenPhase::Prepared
            )
        {
            return Err(MediaOpenStartError::Busy);
        }
        current
            .cancellation
            .cancel(MediaInstallCancellationCause::Superseded);
        self.current = None;
        self.start(client_key, source_request, MediaOpenStartMode::RequireIdle)
    }

    /// Explicit matching authorization dispatch без промежуточного buffer-а.
    pub(crate) fn authorize_ready(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<AuthorizationDispatchResolution, MediaOpenCommandError> {
        self.matching_current(request_id)?;
        let player_port = self
            .player_port
            .as_ref()
            .ok_or(MediaOpenCommandError::MissingPlayerBinding)?
            .clone();
        let current = self.matching_current_mut(request_id)?;
        if current.phase != MediaOpenPhase::ReadyToCommit {
            return Err(MediaOpenCommandError::InvalidPhase {
                actual: current.phase,
            });
        }
        let player_request_id = current
            .player_request_id
            .expect("Ready request must have player request id");
        if current.same_lineage_position == SameLineagePositionPreparationPhase::ReadyToCommit {
            return Err(MediaOpenCommandError::InvalidPhase {
                actual: current.phase,
            });
        }
        current.phase = MediaOpenPhase::AuthorizationDispatchPending;
        match player_port.authorize(player_request_id) {
            Ok(receipt) => {
                current.pending_control = Some(PendingControl::Authorization(receipt));
                let resolution = AuthorizationDispatchResolution::EnqueuedAtPlayerOwner;
                current.authorization_resolution = Some(resolution);
                current.phase = MediaOpenPhase::EnqueuedAtPlayerOwner;
                Ok(resolution)
            }
            Err(rejection) => {
                let resolution =
                    AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue { rejection };
                current.authorization_resolution = Some(resolution);
                current.phase = MediaOpenPhase::ReadyToCommit;
                Err(MediaOpenCommandError::PlayerDispatch(rejection))
            }
        }
    }

    pub(crate) fn stage_same_lineage_at_player(
        &mut self,
        request_id: MediaOpenRequestId,
        intent: MediaOpenInstallIntent,
        video_resource_port: MediaInstallVideoResourcePort,
        expected_old_media_instance_id: player_core::MediaInstanceId,
    ) -> Result<MediaInstallRequestId, MediaOpenCommandError> {
        self.stage_at_player_with_position(
            request_id,
            intent,
            video_resource_port,
            player_staging::same_lineage_position(expected_old_media_instance_id),
        )
    }

    pub(crate) fn cancel_request(
        &mut self,
        request_id: MediaOpenRequestId,
        cause: MediaInstallCancellationCause,
    ) -> Result<CancellationDispatchOutcome, MediaOpenCommandError> {
        self.cancel_request_with_dispatch(request_id, cause, CancellationDispatchMode::NonBlocking)
    }

    /// После доказанного pre-barrier rejection доставляет cleanup без повторной потери на Full.
    pub(crate) fn cancel_request_lossless(
        &mut self,
        request_id: MediaOpenRequestId,
        cause: MediaInstallCancellationCause,
    ) -> Result<CancellationDispatchOutcome, MediaOpenCommandError> {
        self.cancel_request_with_dispatch(
            request_id,
            cause,
            CancellationDispatchMode::LosslessCleanup,
        )
    }

    fn cancel_request_with_dispatch(
        &mut self,
        request_id: MediaOpenRequestId,
        cause: MediaInstallCancellationCause,
        dispatch_mode: CancellationDispatchMode,
    ) -> Result<CancellationDispatchOutcome, MediaOpenCommandError> {
        let current = self.matching_current(request_id)?;
        if current.phase == MediaOpenPhase::EnqueuedAtPlayerOwner {
            return Ok(CancellationDispatchOutcome::CommitMustFinish);
        }
        if current.phase == MediaOpenPhase::AuthorizationDispatchPending
            && matches!(
                current.pending_control,
                Some(PendingControl::Authorization(_))
            )
        {
            return Ok(CancellationDispatchOutcome::CommitMustFinish);
        }
        if current.phase == MediaOpenPhase::AuthorizationDispatchPending
            && matches!(
                current.pending_control,
                Some(PendingControl::Cancellation { .. })
            )
        {
            return Ok(CancellationDispatchOutcome::DispatchPending);
        }

        let Some(player_request_id) = current.player_request_id else {
            let current = self.matching_current_mut(request_id)?;
            current.cancellation.cancel(cause);
            current.phase = MediaOpenPhase::Failed;
            current.authorization_resolution =
                Some(AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue { cause });
            current.terminal = Some(MediaOpenTerminalOutcome::Cancelled { request_id, cause });
            return Ok(CancellationDispatchOutcome::CancelledBeforePlayerStaging);
        };

        let player_port = self
            .player_port
            .as_ref()
            .ok_or(MediaOpenCommandError::MissingPlayerBinding)?
            .clone();
        let current = self.matching_current_mut(request_id)?;
        current.cancellation.cancel(cause);
        let dispatch_result = match dispatch_mode {
            CancellationDispatchMode::NonBlocking => player_port.cancel(player_request_id, cause),
            CancellationDispatchMode::LosslessCleanup => {
                player_port.cancel_lossless(player_request_id, cause)
            }
        };
        match dispatch_result {
            Ok(receipt) => {
                current.phase = MediaOpenPhase::AuthorizationDispatchPending;
                current.pending_control = Some(PendingControl::Cancellation { cause, receipt });
                Ok(CancellationDispatchOutcome::DispatchPending)
            }
            Err(rejection) => {
                if dispatch_mode == CancellationDispatchMode::LosslessCleanup {
                    self.publish_fatal(
                        MediaOpenInvariantViolation::LosslessCancellationDispatchFailed,
                    );
                }
                Err(MediaOpenCommandError::PlayerDispatch(rejection))
            }
        }
    }

    /// D52 update адресуется только exact player request-у; fallback transport отсутствует.
    pub(crate) fn update_playback_intent(
        &self,
        request_id: MediaOpenRequestId,
        revision: player_core::PlaybackIntentRevision,
        intent: player_core::PlaybackIntent,
    ) -> Result<PlaybackIntentUpdateReceipt, MediaOpenCommandError> {
        let current = self.matching_current(request_id)?;
        let player_request_id =
            current
                .player_request_id
                .ok_or(MediaOpenCommandError::InvalidPhase {
                    actual: current.phase,
                })?;
        self.player_port
            .as_ref()
            .ok_or(MediaOpenCommandError::MissingPlayerBinding)?
            .update_intent(PlaybackIntentUpdate {
                request_id: player_request_id,
                revision,
                intent,
            })
            .map_err(MediaOpenCommandError::PlayerDispatch)
    }

    /// Неблокирующе продвигает preparation/player receipts; вызывается по wake/UI drain.
    pub(crate) fn drain(&mut self) -> bool {
        let mut changed = self.drain_preparation();
        changed |= self.drain_player_staging();
        changed |= self.drain_control();
        changed
    }

    /// Ждёт один request-owned progress edge без polling spin и без auto-authorization.
    pub(crate) fn wait_for_progress(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<MediaOpenPhase, MediaOpenCompletionDriveError> {
        let phase = self.matching_current(request_id)?.phase;
        let wait_result = {
            let current = self.matching_current(request_id)?;
            match phase {
                MediaOpenPhase::Preparing => current
                    .preparation_slot
                    .wait_until_result_available()
                    .map_err(|_| MediaOpenCompletionDriveError::MissingPreparationResolution),
                MediaOpenPhase::PlayerStaging => current
                    .install_receipt
                    .as_ref()
                    .expect("PlayerStaging must own install receipt")
                    .wait_until_signal_available()
                    .map_err(|_| MediaOpenCompletionDriveError::MissingPlayerResolution),
                MediaOpenPhase::AuthorizationDispatchPending
                | MediaOpenPhase::EnqueuedAtPlayerOwner => current
                    .pending_control
                    .as_ref()
                    .expect("dispatch phase must own control receipt")
                    .wait_until_outcome_available()
                    .map_err(|_| MediaOpenCompletionDriveError::MissingPlayerResolution),
                MediaOpenPhase::Accepted
                | MediaOpenPhase::Prepared
                | MediaOpenPhase::ReadyToCommit
                | MediaOpenPhase::Installed
                | MediaOpenPhase::Failed => Ok(()),
            }
        };
        if let Err(error) = wait_result {
            let violation = match error {
                MediaOpenCompletionDriveError::MissingPreparationResolution => {
                    MediaOpenInvariantViolation::PreparationStateLost
                }
                MediaOpenCompletionDriveError::MissingPlayerResolution => {
                    if phase == MediaOpenPhase::PlayerStaging {
                        MediaOpenInvariantViolation::MissingPlayerInstallResolution
                    } else {
                        MediaOpenInvariantViolation::MissingPlayerControlResolution
                    }
                }
                MediaOpenCompletionDriveError::Command(_) => unreachable!(),
            };
            self.publish_fatal(violation);
            return Err(error);
        }
        self.drain();
        Ok(self.matching_current(request_id)?.phase)
    }

    pub(crate) fn snapshot(&self) -> Option<MediaOpenSnapshot> {
        self.current.as_ref().map(|current| MediaOpenSnapshot {
            client_key: current.client_key,
            request_id: current.request_id,
            phase: current.phase,
            descriptor: current.descriptor.clone().or_else(|| {
                current
                    .prepared_open
                    .as_ref()
                    .map(|prepared| prepared.descriptor.clone())
            }),
            authorization_resolution: current.authorization_resolution,
            same_lineage_position: current.same_lineage_position,
        })
    }

    /// Забирает exactly-once terminal и освобождает authoritative request slot.
    pub(crate) fn take_terminal(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<Option<MediaOpenTerminalOutcome>, MediaOpenCommandError> {
        let current = self.matching_current_mut(request_id)?;
        let Some(terminal) = current.terminal.take() else {
            return Ok(None);
        };
        self.current = None;
        Ok(Some(terminal))
    }

    pub(crate) fn shutdown(&mut self) {
        self.shutting_down = true;
        self.cancel_current_for_lifecycle(MediaInstallCancellationCause::LifecycleShutdown);
        self.executor.shutdown();
    }

    /// Закрывает coordinator admission и bounded-завершает preparation executor.
    pub(crate) fn shutdown_until(
        &mut self,
        deadline: crate::process_shutdown::ShutdownDeadline,
    ) -> crate::process_shutdown::ProcessOwnerShutdownOutcome {
        self.shutdown();
        self.executor.shutdown_until(deadline)
    }

    fn drain_preparation(&mut self) -> bool {
        if self.executor.state_was_lost() {
            self.publish_fatal(MediaOpenInvariantViolation::PreparationStateLost);
            return true;
        }
        let Some(current) = self.current.as_mut() else {
            return false;
        };
        let mut changed = false;
        if current.phase == MediaOpenPhase::Accepted {
            current.phase = MediaOpenPhase::Preparing;
            changed = true;
        }
        if current.phase != MediaOpenPhase::Preparing {
            return changed;
        }
        let result = match current.preparation_slot.take() {
            Ok(Some(result)) => result,
            Ok(None) => return changed,
            Err(()) => {
                self.publish_fatal(MediaOpenInvariantViolation::PreparationStateLost);
                return true;
            }
        };
        match result {
            Ok(prepared_open) if !current.cancellation.is_cancelled() => {
                current.prepared_open = Some(prepared_open);
                current.phase = MediaOpenPhase::Prepared;
            }
            Ok(_) | Err(MediaPreparationFailureKind::Cancelled) => {
                let cancellation_cause = match current.cancellation.cause() {
                    Ok(cause) => cause,
                    Err(()) => {
                        self.publish_fatal(MediaOpenInvariantViolation::PreparationStateLost);
                        return true;
                    }
                };
                if let Some(cause) = cancellation_cause {
                    current.terminal = Some(MediaOpenTerminalOutcome::Cancelled {
                        request_id: current.request_id,
                        cause,
                    });
                } else {
                    current.terminal = Some(MediaOpenTerminalOutcome::PreparationFailed {
                        request_id: current.request_id,
                        safe_label: current.safe_label.clone(),
                        kind: MediaPreparationFailureKind::Cancelled,
                    });
                }
                current.phase = MediaOpenPhase::Failed;
            }
            Err(kind) => {
                current.terminal = Some(MediaOpenTerminalOutcome::PreparationFailed {
                    request_id: current.request_id,
                    safe_label: current.safe_label.clone(),
                    kind,
                });
                current.phase = MediaOpenPhase::Failed;
            }
        }
        true
    }

    fn drain_control(&mut self) -> bool {
        let Some(current) = self.current.as_mut() else {
            return false;
        };
        let Some(control) = current.pending_control.as_ref() else {
            return false;
        };
        let outcome = match control {
            PendingControl::Authorization(receipt) => receipt.take_outcome(),
            PendingControl::Cancellation { receipt, .. } => receipt.take_outcome(),
        };
        let outcome = match outcome {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return false,
            Err(()) => {
                self.publish_fatal(MediaOpenInvariantViolation::MissingPlayerControlResolution);
                return true;
            }
        };
        let control = current
            .pending_control
            .take()
            .expect("control existed while polling");
        match (control, outcome) {
            (
                PendingControl::Cancellation { cause, .. },
                MediaInstallControlOutcome::CancellationAccepted,
            ) => {
                let completion = current
                    .install_receipt
                    .as_ref()
                    .and_then(|receipt| receipt.take_completion());
                let matching_cancel = matches!(
                    completion,
                    Some(MediaInstallCompletion::Cancelled {
                        request_id,
                        cause: terminal_cause,
                    }) if Some(request_id) == current.player_request_id && terminal_cause == cause
                );
                if !matching_cancel {
                    self.publish_fatal(
                        MediaOpenInvariantViolation::MissingTerminalAfterPlayerControl,
                    );
                    return true;
                }
                current.authorization_resolution =
                    Some(AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue { cause });
                current.phase = MediaOpenPhase::Failed;
                current.terminal = Some(MediaOpenTerminalOutcome::Cancelled {
                    request_id: current.request_id,
                    cause,
                });
            }
            (
                PendingControl::Authorization(_),
                MediaInstallControlOutcome::AuthorizationAccepted,
            ) => {
                current.authorization_resolution =
                    Some(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner);
                current.phase = MediaOpenPhase::EnqueuedAtPlayerOwner;
                let completion = current
                    .install_receipt
                    .as_ref()
                    .and_then(|receipt| receipt.take_completion());
                let Some(completion @ MediaInstallCompletion::Installed { request_id, .. }) =
                    completion
                else {
                    self.publish_fatal(
                        MediaOpenInvariantViolation::MissingInstalledAfterPlayerEnqueue,
                    );
                    return true;
                };
                if Some(request_id) != current.player_request_id {
                    self.publish_fatal(MediaOpenInvariantViolation::MismatchedPlayerRequest);
                    return true;
                }
                current.phase = MediaOpenPhase::Installed;
                current.terminal = Some(MediaOpenTerminalOutcome::Installed {
                    request_id: current.request_id,
                    player_request_id: request_id,
                    descriptor: Box::new(
                        current
                            .descriptor
                            .clone()
                            .expect("staged request must retain descriptor"),
                    ),
                    completion,
                });
            }
            (
                PendingControl::Authorization(_),
                MediaInstallControlOutcome::AuthorizationRejectedBeforeCommit,
            ) => {
                let completion = current
                    .install_receipt
                    .as_ref()
                    .and_then(|receipt| receipt.take_completion());
                let Some(completion @ MediaInstallCompletion::Failed { request_id, .. }) =
                    completion
                else {
                    self.publish_fatal(
                        MediaOpenInvariantViolation::MissingTerminalAfterPlayerControl,
                    );
                    return true;
                };
                if Some(request_id) != current.player_request_id {
                    self.publish_fatal(MediaOpenInvariantViolation::MismatchedPlayerRequest);
                    return true;
                }
                current.phase = MediaOpenPhase::Failed;
                current.terminal = Some(MediaOpenTerminalOutcome::PlayerFailed {
                    request_id: current.request_id,
                    completion,
                });
            }
            _ => {
                self.publish_fatal(MediaOpenInvariantViolation::UnexpectedAuthorizationOutcome);
                return true;
            }
        }
        true
    }

    fn publish_fatal(&mut self, violation: MediaOpenInvariantViolation) {
        if let Some(current) = self.current.as_mut() {
            current.phase = MediaOpenPhase::Failed;
            current.terminal = Some(MediaOpenTerminalOutcome::FatalInvariant {
                request_id: current.request_id,
                violation,
            });
        }
    }

    fn matching_current(
        &self,
        request_id: MediaOpenRequestId,
    ) -> Result<&CurrentRequest, MediaOpenCommandError> {
        let current = self
            .current
            .as_ref()
            .ok_or(MediaOpenCommandError::NoCurrentRequest)?;
        if current.request_id != request_id {
            return Err(MediaOpenCommandError::StaleRequest);
        }
        Ok(current)
    }

    fn matching_current_mut(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<&mut CurrentRequest, MediaOpenCommandError> {
        let current = self
            .current
            .as_mut()
            .ok_or(MediaOpenCommandError::NoCurrentRequest)?;
        if current.request_id != request_id {
            return Err(MediaOpenCommandError::StaleRequest);
        }
        Ok(current)
    }

    fn cancel_current_for_lifecycle(&mut self, cause: MediaInstallCancellationCause) {
        let Some(request_id) = self.current.as_ref().map(|current| current.request_id) else {
            return;
        };
        if let Err(error) = self.cancel_request(request_id, cause) {
            tracing::error!(
                ?error,
                ?cause,
                "Lifecycle cancel не получил authoritative coordinator resolution"
            );
            self.publish_fatal(MediaOpenInvariantViolation::LifecycleCancellationDispatchFailed);
        }
    }

    fn allocate_request_id(&self) -> MediaOpenRequestId {
        let raw = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let non_zero = NonZeroU64::new(raw).expect("media-open request identity overflow");
        MediaOpenRequestId::from_non_zero(non_zero)
    }
}

impl MediaOpenSourceRequest {
    fn safe_label(&self) -> SafeMediaLabel {
        match self {
            Self::Local { path, .. } => SafeMediaLabel::from_local_path(path),
            Self::Direct { locator, .. } => {
                SafeMediaLabel::from_service_safe_label(locator.safe_label())
            }
            Self::YtDlp { locator, .. } => {
                SafeMediaLabel::from_service_safe_label(locator.safe_label())
            }
            Self::PlaybackWindow { source, .. } => source.safe_label(),
        }
    }
}

#[cfg(test)]
impl MediaOpenCoordinator {
    pub(super) fn start_fake(
        &mut self,
        client_key: MediaOpenClientKey,
        safe_label: SafeMediaLabel,
        task: impl FnOnce() -> PreparationResult + Send + 'static,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        self.start_with_task(
            client_key,
            MediaOpenStartMode::RequireIdle,
            safe_label,
            move |_cancellation| task(),
        )
    }

    fn attach_fake_player(&mut self, player_port: Arc<dyn MediaOpenPlayerPort>) {
        self.player_port = Some(player_port);
    }

    fn supersede_fake(
        &mut self,
        expected_request_id: MediaOpenRequestId,
        client_key: MediaOpenClientKey,
        safe_label: SafeMediaLabel,
        task: impl FnOnce() -> PreparationResult + Send + 'static,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        let current = self.current.as_ref().ok_or(MediaOpenStartError::Busy)?;
        if current.request_id != expected_request_id
            || !matches!(
                current.phase,
                MediaOpenPhase::Accepted | MediaOpenPhase::Preparing | MediaOpenPhase::Prepared
            )
        {
            return Err(MediaOpenStartError::Busy);
        }
        current
            .cancellation
            .cancel(MediaInstallCancellationCause::Superseded);
        self.current = None;
        self.start_fake(client_key, safe_label, task)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime};

    use media_core::{DemuxSeekResult, Demuxer};
    use playlist_discovery::{LocalMediaFingerprint, LocalMediaKind};
    use video_backend_api::{
        DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
        DetachedVideoBackendPortError, DetachedVideoBackendReply, DetachedVideoBackendRequest,
        DetachedVideoBackendResourcePort,
    };

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::media_open::{
        ActiveMediaSource, MAX_NON_CANCELLABLE_STALE_PREPARATIONS, PlayerDispatchRejection,
        SafeMediaLabel,
    };

    mod same_lineage_tests;

    #[derive(Default)]
    struct FakeDemuxer;

    impl Demuxer for FakeDemuxer {
        fn tracks(&self) -> &[media_core::TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
            Ok(media_core::DemuxReadEvent::EndOfStream)
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            panic!("fake demuxer seek is outside media-open tests")
        }
    }

    struct UnusedVideoResourcePort;

    impl DetachedVideoBackendResourcePort for UnusedVideoResourcePort {
        type RequestId = MediaInstallRequestId;

        fn request_detached_backend(
            &mut self,
            _request: DetachedVideoBackendRequest<Self::RequestId>,
        ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError>
        {
            panic!("fake player port must not inspect app video resource port")
        }

        fn publish_candidate_status(
            &mut self,
            _status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
        ) -> Result<(), DetachedVideoBackendPortError> {
            panic!("fake player port must not publish app candidate status")
        }

        fn cancel_candidate(
            &mut self,
            _request_id: Self::RequestId,
            _cause: DetachedVideoBackendCandidateCancellationCause,
        ) -> Result<(), DetachedVideoBackendPortError> {
            panic!("fake player port must not cancel app candidate")
        }
    }

    #[derive(Default)]
    struct FakeInstallSlots {
        ready: Option<MediaInstallPhase>,
        completion: Option<MediaInstallCompletion>,
    }

    struct FakeInstallReceipt {
        slots: Arc<Mutex<FakeInstallSlots>>,
    }

    impl InstallReceiptPort for FakeInstallReceipt {
        fn take_ready(&self) -> Option<MediaInstallPhase> {
            self.slots.lock().expect("install slots").ready.take()
        }

        fn take_completion(&self) -> Option<MediaInstallCompletion> {
            self.slots.lock().expect("install slots").completion.take()
        }

        fn wait_until_signal_available(&self) -> Result<(), ()> {
            let slots = self.slots.lock().expect("install slots");
            if slots.ready.is_some() || slots.completion.is_some() {
                Ok(())
            } else {
                Err(())
            }
        }
    }

    enum FakeControlState {
        Pending,
        Outcome(MediaInstallControlOutcome),
        Missing,
    }

    struct FakeControlReceipt {
        state: Arc<Mutex<FakeControlState>>,
    }

    impl ControlReceiptPort for FakeControlReceipt {
        fn take_outcome(&self) -> Result<Option<MediaInstallControlOutcome>, ()> {
            let mut state = self.state.lock().expect("control state");
            match std::mem::replace(&mut *state, FakeControlState::Pending) {
                FakeControlState::Pending => Ok(None),
                FakeControlState::Outcome(outcome) => Ok(Some(outcome)),
                FakeControlState::Missing => Err(()),
            }
        }

        fn wait_until_outcome_available(&self) -> Result<(), ()> {
            match *self.state.lock().expect("control state") {
                FakeControlState::Outcome(_) => Ok(()),
                FakeControlState::Pending | FakeControlState::Missing => Err(()),
            }
        }
    }

    struct FakePlayerState {
        install_slots: Arc<Mutex<FakeInstallSlots>>,
        control_states: VecDeque<Arc<Mutex<FakeControlState>>>,
        authorize_rejection: Option<PlayerDispatchRejection>,
        authorize_calls: usize,
        prepare_position_calls: usize,
        cancel_calls: Vec<MediaInstallCancellationCause>,
        staged_request_id: Option<MediaInstallRequestId>,
        intent_updates: Vec<PlaybackIntentUpdate>,
    }

    struct FakePlayerPort {
        state: Arc<Mutex<FakePlayerState>>,
    }

    impl MediaOpenPlayerPort for FakePlayerPort {
        fn stage(
            &self,
            request_id: MediaInstallRequestId,
            _prepared_media: player_core::PreparedMedia,
            _intent: MediaOpenInstallIntent,
            _video_resource_port: MediaInstallVideoResourcePort,
            _position_preparation: MediaOpenPositionPreparation,
        ) -> Result<Box<dyn InstallReceiptPort>, PlayerDispatchRejection> {
            let mut state = self.state.lock().expect("fake player state");
            state.staged_request_id = Some(request_id);
            Ok(Box::new(FakeInstallReceipt {
                slots: Arc::clone(&state.install_slots),
            }))
        }

        fn prepare_position(
            &self,
            _request_id: MediaInstallRequestId,
        ) -> Result<(), PlayerDispatchRejection> {
            self.state
                .lock()
                .expect("fake player state")
                .prepare_position_calls += 1;
            Ok(())
        }

        fn authorize(
            &self,
            _request_id: MediaInstallRequestId,
        ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
            let mut state = self.state.lock().expect("fake player state");
            state.authorize_calls += 1;
            if let Some(rejection) = state.authorize_rejection {
                return Err(rejection);
            }
            let control_state = state
                .control_states
                .pop_front()
                .expect("authorization control state queued");
            Ok(Box::new(FakeControlReceipt {
                state: control_state,
            }))
        }

        fn cancel(
            &self,
            _request_id: MediaInstallRequestId,
            cause: MediaInstallCancellationCause,
        ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
            let mut state = self.state.lock().expect("fake player state");
            state.cancel_calls.push(cause);
            let control_state = state
                .control_states
                .pop_front()
                .expect("cancellation control state queued");
            Ok(Box::new(FakeControlReceipt {
                state: control_state,
            }))
        }

        fn update_intent(
            &self,
            update: PlaybackIntentUpdate,
        ) -> Result<PlaybackIntentUpdateReceipt, PlayerDispatchRejection> {
            self.state
                .lock()
                .expect("fake player state")
                .intent_updates
                .push(update);
            Err(PlayerDispatchRejection::Disconnected)
        }
    }

    fn coordinator() -> MediaOpenCoordinator {
        MediaOpenCoordinator::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime))
    }

    fn client(value: u64) -> MediaOpenClientKey {
        MediaOpenClientKey::from_non_zero(NonZeroU64::new(value).expect("non-zero client"))
    }

    fn fake_prepared_with_descriptor(descriptor: PreparedMediaDescriptor) -> PreparedMediaOpen {
        PreparedMediaOpen {
            prepared_media: player_core::PreparedMedia::from_external_label(
                "safe.test",
                Box::new(FakeDemuxer),
            ),
            descriptor,
        }
    }

    fn fake_prepared() -> PreparedMediaOpen {
        fake_prepared_with_descriptor(PreparedMediaDescriptor::Local {
            media_kind: LocalMediaKind::AudioOnly,
            tracks: Vec::new(),
            duration: None,
            metadata: media_core::MediaTagMetadata::default(),
            fingerprint: LocalMediaFingerprint::new(7, SystemTime::UNIX_EPOCH),
            source: ActiveMediaSource::LocalFile("fixture.wav".into()),
            safe_label: SafeMediaLabel::from_service_safe_label("fixture.wav"),
            fingerprint_validation: crate::media_open::LocalFingerprintValidation::Matched,
        })
    }

    fn wait_until_prepared(coordinator: &mut MediaOpenCoordinator) -> MediaOpenRequestId {
        for _ in 0..1_000 {
            coordinator.drain();
            let snapshot = coordinator.snapshot().expect("current request");
            if snapshot.phase == MediaOpenPhase::Prepared {
                return snapshot.request_id;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("fake preparation did not complete")
    }

    fn attach_fake_player(
        coordinator: &mut MediaOpenCoordinator,
        authorize_rejection: Option<PlayerDispatchRejection>,
        control_states: Vec<Arc<Mutex<FakeControlState>>>,
    ) -> Arc<Mutex<FakePlayerState>> {
        let state = Arc::new(Mutex::new(FakePlayerState {
            install_slots: Arc::new(Mutex::new(FakeInstallSlots::default())),
            control_states: control_states.into(),
            authorize_rejection,
            authorize_calls: 0,
            prepare_position_calls: 0,
            cancel_calls: Vec::new(),
            staged_request_id: None,
            intent_updates: Vec::new(),
        }));
        coordinator.attach_fake_player(Arc::new(FakePlayerPort {
            state: Arc::clone(&state),
        }));
        state
    }

    #[test]
    fn ready_passes_through_without_auto_authorization_then_enqueue_wins() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut coordinator);
        let authorization_state = Arc::new(Mutex::new(FakeControlState::Pending));
        let player_state = attach_fake_player(
            &mut coordinator,
            None,
            vec![Arc::clone(&authorization_state)],
        );
        let player_request_id = coordinator
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPaused,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");
        player_state
            .lock()
            .expect("player state")
            .install_slots
            .lock()
            .expect("install slots")
            .ready = Some(MediaInstallPhase::ReadyToCommit {
            request_id: player_request_id,
        });
        assert_eq!(
            coordinator.wait_for_progress(request_id),
            Ok(MediaOpenPhase::ReadyToCommit)
        );
        assert_eq!(
            coordinator.snapshot().expect("snapshot").phase,
            MediaOpenPhase::ReadyToCommit
        );
        assert_eq!(
            player_state.lock().expect("player state").authorize_calls,
            0
        );

        assert_eq!(
            coordinator.authorize_ready(request_id),
            Ok(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner)
        );
        assert_eq!(
            coordinator.snapshot().expect("snapshot").phase,
            MediaOpenPhase::EnqueuedAtPlayerOwner
        );
        coordinator.suspend_player_binding();
        assert_eq!(
            coordinator.snapshot().expect("snapshot").phase,
            MediaOpenPhase::EnqueuedAtPlayerOwner
        );
        assert_eq!(
            coordinator.cancel_request(request_id, MediaInstallCancellationCause::TransportStop,),
            Ok(CancellationDispatchOutcome::CommitMustFinish)
        );

        let installed = MediaInstallCompletion::Installed {
            request_id: player_request_id,
            media_instance_id: player_core::MediaInstanceId::from_non_zero(
                NonZeroU64::new(9).expect("non-zero instance"),
            ),
            applied_intent_revision: player_core::PlaybackIntentRevision::INITIAL,
            applied_intent: player_core::PlaybackIntent::StartPaused,
        };
        player_state
            .lock()
            .expect("player state")
            .install_slots
            .lock()
            .expect("install slots")
            .completion = Some(installed);
        *authorization_state.lock().expect("authorization state") =
            FakeControlState::Outcome(MediaInstallControlOutcome::AuthorizationAccepted);
        assert_eq!(
            coordinator.wait_for_progress(request_id),
            Ok(MediaOpenPhase::Installed)
        );
        assert!(matches!(
            coordinator.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::Installed { .. }))
        ));
    }

    #[test]
    fn missing_player_install_resolution_is_fatal_before_ready() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(15),
                SafeMediaLabel::from_service_safe_label("missing-install.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut coordinator);
        attach_fake_player(&mut coordinator, None, Vec::new());
        coordinator
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPaused,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");

        assert_eq!(
            coordinator.wait_for_progress(request_id),
            Err(MediaOpenCompletionDriveError::MissingPlayerResolution)
        );
        assert!(matches!(
            coordinator.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::FatalInvariant {
                violation: MediaOpenInvariantViolation::MissingPlayerInstallResolution,
                ..
            }))
        ));
    }

    #[test]
    fn accepted_phase_is_observable_before_preparation_drain() {
        let mut coordinator = coordinator();
        let accepted = coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let MediaOpenStartOutcome::Accepted { request_id } = accepted else {
            panic!("idle coordinator cannot coalesce first request");
        };

        let snapshot = coordinator.snapshot().expect("accepted request snapshot");
        assert_eq!(snapshot.request_id, request_id);
        assert_eq!(snapshot.phase, MediaOpenPhase::Accepted);
    }

    #[test]
    fn caller_prepared_ingress_enters_same_protocol_without_auto_authorization() {
        let mut coordinator = coordinator();
        let safe_label = SafeMediaLabel::from_service_safe_label("fixture.wav");
        let prepared_open = PreparedMediaOpen::from_caller_prepared(
            player_core::PreparedMedia::from_external_label("fixture.wav", Box::new(FakeDemuxer)),
            ActiveMediaSource::LocalFile("fixture.wav".into()),
            safe_label.clone(),
        );

        let accepted = coordinator
            .start_prepared(client(1), prepared_open, safe_label)
            .expect("caller-prepared request accepted");
        let MediaOpenStartOutcome::Accepted { request_id } = accepted else {
            panic!("idle coordinator cannot coalesce prepared compatibility request");
        };
        let snapshot = coordinator.snapshot().expect("prepared request snapshot");

        assert_eq!(snapshot.request_id, request_id);
        assert_eq!(snapshot.phase, MediaOpenPhase::Prepared);
        assert!(snapshot.authorization_resolution.is_none());
        assert!(matches!(
            snapshot.descriptor,
            Some(PreparedMediaDescriptor::CallerPrepared { .. })
        ));
        assert!(coordinator.take_terminal(request_id).unwrap().is_none());
    }

    #[test]
    fn preparation_panic_becomes_typed_terminal_instead_of_lost_result() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || panic!("synthetic preparation panic"),
            )
            .expect("request accepted before worker executes task");
        let request_id = coordinator.snapshot().expect("accepted request").request_id;

        for _ in 0..1_000 {
            coordinator.drain();
            if matches!(
                coordinator.take_terminal(request_id),
                Ok(Some(MediaOpenTerminalOutcome::PreparationFailed {
                    kind: MediaPreparationFailureKind::WorkerPanicked,
                    ..
                }))
            ) {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("typed worker-panic terminal was not published");
    }

    #[test]
    fn local_and_direct_descriptors_follow_the_same_prepared_phase() {
        let direct_locator = service_direct_media::parse_direct_media_url(
            "https://media.example.test/movie.mp4?token=secret",
        )
        .expect("direct locator");
        let descriptors = [
            PreparedMediaDescriptor::Local {
                media_kind: LocalMediaKind::AudioOnly,
                tracks: Vec::new(),
                duration: None,
                metadata: media_core::MediaTagMetadata::default(),
                fingerprint: LocalMediaFingerprint::new(7, SystemTime::UNIX_EPOCH),
                source: ActiveMediaSource::LocalFile("fixture.wav".into()),
                safe_label: SafeMediaLabel::from_service_safe_label("fixture.wav"),
                fingerprint_validation: crate::media_open::LocalFingerprintValidation::Matched,
            },
            PreparedMediaDescriptor::Direct {
                tracks: Vec::new(),
                duration: None,
                metadata: media_core::MediaTagMetadata::default(),
                source: ActiveMediaSource::DirectMediaUrl(direct_locator),
                safe_label: SafeMediaLabel::from_service_safe_label("media.example.test"),
            },
        ];

        for (index, descriptor) in descriptors.into_iter().enumerate() {
            let mut coordinator = coordinator();
            coordinator
                .start_fake(
                    client((index + 1) as u64),
                    SafeMediaLabel::from_service_safe_label("safe.test"),
                    move || Ok(fake_prepared_with_descriptor(descriptor)),
                )
                .expect("source-neutral request accepted");
            let _request_id = wait_until_prepared(&mut coordinator);
            assert!(matches!(
                coordinator
                    .snapshot()
                    .expect("prepared snapshot")
                    .descriptor,
                Some(PreparedMediaDescriptor::Local { .. })
                    | Some(PreparedMediaDescriptor::Direct { .. })
            ));
        }
    }

    #[test]
    fn downstream_authorization_rejection_is_pre_enqueue_resolution() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut coordinator);
        let cancellation_state = Arc::new(Mutex::new(FakeControlState::Pending));
        let player_state = attach_fake_player(
            &mut coordinator,
            Some(PlayerDispatchRejection::Backpressure),
            vec![Arc::clone(&cancellation_state)],
        );
        let player_request_id = coordinator
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPlaying,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");
        player_state
            .lock()
            .expect("player state")
            .install_slots
            .lock()
            .expect("install slots")
            .ready = Some(MediaInstallPhase::ReadyToCommit {
            request_id: player_request_id,
        });
        coordinator.drain();

        assert_eq!(
            coordinator.authorize_ready(request_id),
            Err(MediaOpenCommandError::PlayerDispatch(
                PlayerDispatchRejection::Backpressure
            ))
        );
        let snapshot = coordinator.snapshot().expect("snapshot");
        assert_eq!(snapshot.phase, MediaOpenPhase::ReadyToCommit);
        assert_eq!(
            snapshot.authorization_resolution,
            Some(
                AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                    rejection: PlayerDispatchRejection::Backpressure
                }
            )
        );

        assert_eq!(
            coordinator.cancel_request_lossless(
                request_id,
                MediaInstallCancellationCause::StructuralInvalidation,
            ),
            Ok(CancellationDispatchOutcome::DispatchPending)
        );
        player_state
            .lock()
            .expect("player state")
            .install_slots
            .lock()
            .expect("install slots")
            .completion = Some(MediaInstallCompletion::Cancelled {
            request_id: player_request_id,
            cause: MediaInstallCancellationCause::StructuralInvalidation,
        });
        *cancellation_state.lock().expect("cancellation state") =
            FakeControlState::Outcome(MediaInstallControlOutcome::CancellationAccepted);
        assert_eq!(
            coordinator.wait_for_progress(request_id),
            Ok(MediaOpenPhase::Failed)
        );
        assert!(matches!(
            coordinator.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::Cancelled {
                cause: MediaInstallCancellationCause::StructuralInvalidation,
                ..
            }))
        ));
    }

    #[test]
    fn cancellation_causes_remain_distinct_before_player_staging() {
        let causes = [
            MediaInstallCancellationCause::UserCancelled,
            MediaInstallCancellationCause::Superseded,
            MediaInstallCancellationCause::TransportStop,
            MediaInstallCancellationCause::StructuralInvalidation,
            MediaInstallCancellationCause::LifecycleSuspended,
            MediaInstallCancellationCause::LifecycleShutdown,
        ];
        for (index, cause) in causes.into_iter().enumerate() {
            let mut coordinator = coordinator();
            coordinator
                .start_fake(
                    client((index + 1) as u64),
                    SafeMediaLabel::from_service_safe_label("safe.test"),
                    || Ok(fake_prepared()),
                )
                .expect("start accepted");
            let request_id = coordinator.snapshot().expect("accepted request").request_id;
            assert_eq!(
                coordinator.cancel_request(request_id, cause),
                Ok(CancellationDispatchOutcome::CancelledBeforePlayerStaging)
            );
            assert!(matches!(
                coordinator.take_terminal(request_id),
                Ok(Some(MediaOpenTerminalOutcome::Cancelled { cause: actual, .. })) if actual == cause
            ));
        }
    }

    #[test]
    fn caller_coalesce_and_supersede_keep_only_latest_logical_request() {
        let mut coordinator = coordinator();
        let (release_tx, release_rx) = mpsc::channel();
        let first = coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("first"),
                move || {
                    release_rx.recv().expect("release stale work");
                    Ok(fake_prepared())
                },
            )
            .expect("first start");
        let first_id = match first {
            MediaOpenStartOutcome::Accepted { request_id } => request_id,
            MediaOpenStartOutcome::Coalesced { .. } => panic!("first request cannot coalesce"),
        };
        let coalesced = coordinator
            .start_with_task(
                client(1),
                MediaOpenStartMode::CoalesceMatchingClient,
                SafeMediaLabel::from_service_safe_label("ignored"),
                |_cancellation| Ok(fake_prepared()),
            )
            .expect("coalesce accepted");
        assert_eq!(
            coalesced,
            MediaOpenStartOutcome::Coalesced {
                request_id: first_id
            }
        );
        coordinator
            .supersede_fake(
                first_id,
                client(2),
                SafeMediaLabel::from_service_safe_label("second"),
                || Ok(fake_prepared()),
            )
            .expect("supersede accepted");
        let _stale_work_was_running = release_tx.send(()).is_ok();
        let latest_id = wait_until_prepared(&mut coordinator);
        assert_ne!(latest_id, first_id);
        assert_eq!(MAX_NON_CANCELLABLE_STALE_PREPARATIONS, 1);
    }

    #[test]
    fn stale_request_command_is_not_reported_as_player_backpressure() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("first"),
                || Ok(fake_prepared()),
            )
            .expect("first request accepted");
        let first_request_id = wait_until_prepared(&mut coordinator);
        coordinator
            .supersede_fake(
                first_request_id,
                client(2),
                SafeMediaLabel::from_service_safe_label("second"),
                || Ok(fake_prepared()),
            )
            .expect("second request supersedes first");

        assert_eq!(
            coordinator.cancel_request(
                first_request_id,
                MediaInstallCancellationCause::UserCancelled,
            ),
            Err(MediaOpenCommandError::StaleRequest)
        );
    }

    #[test]
    fn repeated_cancel_does_not_replace_authoritative_control_receipt() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut coordinator);
        let control_state = Arc::new(Mutex::new(FakeControlState::Pending));
        let player_state =
            attach_fake_player(&mut coordinator, None, vec![Arc::clone(&control_state)]);
        coordinator
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPaused,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");

        assert_eq!(
            coordinator.cancel_request(request_id, MediaInstallCancellationCause::UserCancelled,),
            Ok(CancellationDispatchOutcome::DispatchPending)
        );
        assert_eq!(
            coordinator.cancel_request(
                request_id,
                MediaInstallCancellationCause::LifecycleSuspended,
            ),
            Ok(CancellationDispatchOutcome::DispatchPending)
        );
        assert_eq!(
            player_state
                .lock()
                .expect("player state")
                .cancel_calls
                .len(),
            1
        );
    }

    #[test]
    fn d52_update_forwards_exact_player_request_revision_and_intent() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut coordinator);
        let player_state = attach_fake_player(&mut coordinator, None, Vec::new());
        let player_request_id = coordinator
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPaused,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");

        assert!(matches!(
            coordinator.update_playback_intent(
                request_id,
                player_core::PlaybackIntentRevision::INITIAL,
                player_core::PlaybackIntent::StartPlaying,
            ),
            Err(MediaOpenCommandError::PlayerDispatch(
                PlayerDispatchRejection::Disconnected,
            ))
        ));
        assert_eq!(
            player_state.lock().expect("player state").intent_updates,
            vec![PlaybackIntentUpdate {
                request_id: player_request_id,
                revision: player_core::PlaybackIntentRevision::INITIAL,
                intent: player_core::PlaybackIntent::StartPlaying,
            }]
        );
    }

    #[test]
    fn cancel_control_and_missing_resolution_are_authoritative() {
        let mut cancel_coordinator = coordinator();
        cancel_coordinator
            .start_fake(
                client(1),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut cancel_coordinator);
        let cancel_state = Arc::new(Mutex::new(FakeControlState::Pending));
        let player_state = attach_fake_player(
            &mut cancel_coordinator,
            None,
            vec![Arc::clone(&cancel_state)],
        );
        cancel_coordinator
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPaused,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");
        assert_eq!(
            cancel_coordinator.cancel_request(
                request_id,
                MediaInstallCancellationCause::LifecycleSuspended,
            ),
            Ok(CancellationDispatchOutcome::DispatchPending)
        );
        let cancel_player_request_id = player_state
            .lock()
            .expect("player state")
            .staged_request_id
            .expect("staged request id");
        player_state
            .lock()
            .expect("player state")
            .install_slots
            .lock()
            .expect("install slots")
            .completion = Some(MediaInstallCompletion::Cancelled {
            request_id: cancel_player_request_id,
            cause: MediaInstallCancellationCause::LifecycleSuspended,
        });
        *cancel_state.lock().expect("cancel state") =
            FakeControlState::Outcome(MediaInstallControlOutcome::CancellationAccepted);
        assert_eq!(
            cancel_coordinator.wait_for_progress(request_id),
            Ok(MediaOpenPhase::Failed)
        );
        assert!(matches!(
            cancel_coordinator.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::Cancelled {
                cause: MediaInstallCancellationCause::LifecycleSuspended,
                ..
            }))
        ));

        let mut missing = coordinator();
        missing
            .start_fake(
                client(2),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut missing);
        let missing_state = Arc::new(Mutex::new(FakeControlState::Missing));
        let player_state = attach_fake_player(&mut missing, None, vec![Arc::clone(&missing_state)]);
        missing
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPaused,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");
        missing
            .cancel_request(request_id, MediaInstallCancellationCause::UserCancelled)
            .expect("cancel dispatched");
        assert_eq!(
            missing.wait_for_progress(request_id),
            Err(MediaOpenCompletionDriveError::MissingPlayerResolution)
        );
        assert!(matches!(
            missing.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::FatalInvariant {
                violation: MediaOpenInvariantViolation::MissingPlayerControlResolution,
                ..
            }))
        ));
        drop(player_state);
    }

    #[test]
    fn authorization_ack_without_installed_terminal_is_fatal() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client(3),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = wait_until_prepared(&mut coordinator);
        let authorization_state = Arc::new(Mutex::new(FakeControlState::Outcome(
            MediaInstallControlOutcome::AuthorizationAccepted,
        )));
        let player_state = attach_fake_player(
            &mut coordinator,
            None,
            vec![Arc::clone(&authorization_state)],
        );
        let player_request_id = coordinator
            .stage_at_player(
                request_id,
                MediaOpenInstallIntent {
                    intent: player_core::PlaybackIntent::StartPaused,
                    revision: player_core::PlaybackIntentRevision::INITIAL,
                },
                MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            )
            .expect("stage accepted");
        player_state
            .lock()
            .expect("player state")
            .install_slots
            .lock()
            .expect("install slots")
            .ready = Some(MediaInstallPhase::ReadyToCommit {
            request_id: player_request_id,
        });
        coordinator.drain();
        coordinator
            .authorize_ready(request_id)
            .expect("authorization enqueued");

        assert!(coordinator.drain());
        assert!(matches!(
            coordinator.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::FatalInvariant {
                violation: MediaOpenInvariantViolation::MissingInstalledAfterPlayerEnqueue,
                ..
            }))
        ));
    }
}
