//! Runtime-side execution seams для Session 18A transport adapter-а.
//!
//! Эти методы не открывают media и не отправляют player commands: они сохраняют queue/controller
//! ownership, выдают locator по exact plan и принимают correlated D08/D39 request обратно.

use player_core::MediaInstallRequestId;
use playlist_core::PlaylistLocator;

use super::controller::{
    AutomaticLifecycleOutcome, ControllerStableIntentDispatch, PlannedPlaylistInstall,
    PlaylistInstallRequest,
};
use super::controller::{ManualNavigationCancelOutcome, ManualNavigationFailureOutcome};
use super::discovery::PlaylistDiscoveryNavigationStatus;
use super::identity::TransportActionOrigin;
use super::{PlaylistMediaOpenGateError, PlaylistRuntime};
use crate::media_open::MediaOpenRequestId;

/// Cancel различает manual/automatic wait и безопасный no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistTransportCancelOutcome {
    CancelledManual,
    CancelledAutomatic,
    NoPendingWait,
}

impl PlaylistRuntime {
    /// Commit-ит app-level Stopped только после exact player owner success.
    pub(crate) fn apply_neutral_stop_outcome(
        &mut self,
        outcome: &player_core::ExactMediaTransportOutcome,
    ) -> bool {
        self.controller
            .as_mut()
            .is_some_and(|controller| controller.apply_neutral_stop_outcome(outcome))
    }

    /// Locator выдаётся только для exact queue revision/item из controller plan-а.
    pub(crate) fn locator_for_planned_install(
        &self,
        install: &PlannedPlaylistInstall,
    ) -> Result<PlaylistLocator, PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_ref()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?;
        if controller.queue().revision_snapshot() != install.expected_queue_revision {
            return Err(PlaylistMediaOpenGateError::StalePlannedTarget);
        }
        controller
            .queue()
            .item(install.item_id)
            .map(|item| item.locator().clone())
            .ok_or(PlaylistMediaOpenGateError::StalePlannedTarget)
    }

    /// Связывает обычный manual/automatic plan с уже staged player request.
    pub(crate) fn accept_planned_playlist_install(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        install: PlannedPlaylistInstall,
    ) -> Result<(), PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?;
        controller
            .accept_install_request(playlist_install_request(
                request_id,
                player_request_id,
                install,
            ))
            .map_err(PlaylistMediaOpenGateError::InstallAdmission)
    }

    /// D53 заменяет только exact AwaitingReady request; FIFO не создаётся.
    pub(crate) fn accept_superseding_playlist_install(
        &mut self,
        expected_request_id: MediaOpenRequestId,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        install: PlannedPlaylistInstall,
    ) -> Result<(), PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?;
        controller
            .supersede_install_request_before_ready(
                expected_request_id,
                playlist_install_request(request_id, player_request_id, install),
            )
            .map_err(PlaylistMediaOpenGateError::InstallAdmission)
    }

    /// Toggle опирается на controller-owned stable intent, а не на transient player state.
    pub(crate) fn toggle_ui_stable_transport_intent(
        &mut self,
    ) -> Option<ControllerStableIntentDispatch> {
        self.controller
            .as_mut()?
            .toggle_stable_transport_intent(TransportActionOrigin::Ui)
    }

    /// D52 адресует coordinator request, не заставляя AppState угадывать ID mapping.
    pub(crate) fn apply_stable_pending_intent_update(
        &self,
        dispatch: &ControllerStableIntentDispatch,
    ) -> Result<
        Option<(MediaOpenRequestId, player_core::PlaybackIntentUpdateReceipt)>,
        crate::media_open::MediaOpenCommandError,
    > {
        let Some(update) = dispatch.pending_update else {
            return Ok(None);
        };
        let request_id = self
            .controller
            .as_ref()
            .and_then(|controller| controller.install_request_id())
            .ok_or(crate::media_open::MediaOpenCommandError::StaleRequest)?;
        let receipt =
            self.media_open
                .update_playback_intent(request_id, update.revision, update.intent)?;
        Ok(Some((request_id, receipt)))
    }

    /// Preparation/player failure сохраняет controller-owned retry cursor.
    pub(crate) fn report_playlist_navigation_failure(
        &mut self,
        request_id: MediaOpenRequestId,
        item_id: playlist_core::PlaylistItemId,
    ) {
        if let Some(controller) = self.controller.as_mut() {
            let outcome = controller.report_manual_navigation_target_failure(request_id);
            if matches!(outcome, ManualNavigationFailureOutcome::NotManualNavigation) {
                controller.report_unstaged_manual_navigation_target_failure(item_id);
            }
            self.discovery.synchronize_navigation_interest(controller);
        }
    }

    /// Синхронная source-boundary ошибка возникает ещё до появления media-open request ID.
    pub(crate) fn report_unstaged_playlist_navigation_failure(
        &mut self,
        item_id: playlist_core::PlaylistItemId,
    ) {
        if let Some(controller) = self.controller.as_mut() {
            controller.report_unstaged_manual_navigation_target_failure(item_id);
            self.discovery.synchronize_navigation_interest(controller);
        }
    }

    /// D58-like explicit Cancel убирает только navigation interest, bulk scan продолжает жить.
    pub(crate) fn cancel_global_playlist_navigation_wait(
        &mut self,
    ) -> PlaylistTransportCancelOutcome {
        let status = self.playlist_discovery_navigation_status();
        let Some(controller) = self.controller.as_mut() else {
            return PlaylistTransportCancelOutcome::NoPendingWait;
        };
        let outcome = match status {
            PlaylistDiscoveryNavigationStatus::WaitingManual {
                wait_id, scope_id, ..
            } if controller.cancel_manual_navigation_wait(wait_id, scope_id) => {
                PlaylistTransportCancelOutcome::CancelledManual
            }
            PlaylistDiscoveryNavigationStatus::WaitingAutomatic { .. } => {
                let automatic = controller.cancel_deferred_automatic_advance();
                if matches!(automatic, AutomaticLifecycleOutcome::NoAction) {
                    PlaylistTransportCancelOutcome::NoPendingWait
                } else {
                    PlaylistTransportCancelOutcome::CancelledAutomatic
                }
            }
            _ => PlaylistTransportCancelOutcome::NoPendingWait,
        };
        self.discovery.synchronize_navigation_interest(controller);
        outcome
    }

    /// Один UI intent маршрутизируется либо в D55 cursor Cancel, либо в D50 wait Cancel.
    pub(crate) fn cancel_playlist_navigation_from_ui(&mut self) -> bool {
        if self.controller.as_ref().is_some_and(|controller| {
            controller
                .view_snapshot()
                .awaiting_user_after_navigation_failure()
        }) {
            let Some(controller) = self.controller.as_mut() else {
                return false;
            };
            let outcome = controller.cancel_manual_navigation();
            self.discovery.synchronize_navigation_interest(controller);
            return match outcome {
                ManualNavigationCancelOutcome::NoManualNavigation => false,
                ManualNavigationCancelOutcome::Fatal(_) => {
                    self.set_playlist_safe_feedback("Не удалось отменить переход");
                    true
                }
                ManualNavigationCancelOutcome::Discarded(_)
                | ManualNavigationCancelOutcome::CancelPending { .. }
                | ManualNavigationCancelOutcome::AwaitAuthorizationResolution { .. }
                | ManualNavigationCancelOutcome::AwaitInstalled { .. } => true,
            };
        }
        !matches!(
            self.cancel_global_playlist_navigation_wait(),
            PlaylistTransportCancelOutcome::NoPendingWait
        )
    }
}

fn playlist_install_request(
    request_id: MediaOpenRequestId,
    player_request_id: MediaInstallRequestId,
    install: PlannedPlaylistInstall,
) -> PlaylistInstallRequest {
    PlaylistInstallRequest {
        request_id,
        player_request_id,
        target_item_id: Some(install.item_id),
        origin: install.pending_origin,
        intent_revision: install.intent_revision,
        expected_queue_revision: install.expected_queue_revision,
        mutation: install.mutation,
    }
}
