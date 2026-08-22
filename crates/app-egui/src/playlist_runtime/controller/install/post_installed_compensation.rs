//! Controller-owned reconciliation после exact release post-Installed candidate-а.

use super::{
    ControllerTerminalDrain, ControllerTerminalResolution, GuardedInstall, GuardedInstallAbort,
    InstallState, PlaylistControllerInvariantViolation,
};
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::controller::{AppTransportDisposition, PlaylistController};
use player_core::MediaInstallRequestId;

impl PlaylistController {
    /// Снимает reservation только после подтверждённого player-owned release.
    pub(crate) fn reconcile_released_post_installed_candidate(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
    ) -> Result<Option<ControllerTerminalDrain>, PlaylistControllerInvariantViolation> {
        if let Some(violation) = self.fatal_invariant {
            return Err(violation);
        }
        let drain = match self.install_state.take() {
            Some(InstallState::AuthorizationInFlight {
                guarded,
                post_commit_intent,
            }) => {
                if guarded.request_id != request_id {
                    self.install_state = Some(InstallState::AuthorizationInFlight {
                        guarded,
                        post_commit_intent,
                    });
                    return self.fatal_result(
                        PlaylistControllerInvariantViolation::TerminalRequestMismatch,
                    );
                }
                if guarded.player_request_id != player_request_id {
                    self.install_state = Some(InstallState::AuthorizationInFlight {
                        guarded,
                        post_commit_intent,
                    });
                    return self
                        .fatal_result(PlaylistControllerInvariantViolation::PlayerRequestMismatch);
                }
                let GuardedInstall {
                    target_item_id,
                    token,
                    desired_modes,
                    ..
                } = guarded;
                match token.abort(&mut self.queue) {
                    GuardedInstallAbort::Queue => {}
                    GuardedInstallAbort::ManualNavigation(preview) => self
                        .manual_navigation_cursor
                        .mark_failed_after_abort(preview),
                    GuardedInstallAbort::AutomaticTraversal(plan) => {
                        if let Some(item_id) = target_item_id {
                            self.retain_released_automatic_plan(request_id, item_id, plan);
                        }
                    }
                }
                self.pending_target = None;
                self.clear_released_active_media_projection();
                let dirty = self.apply_desired_modes(desired_modes)?;
                Some(ControllerTerminalDrain {
                    request_id,
                    active_media: None,
                    dirty,
                    deferred_intent: post_commit_intent,
                    resolution: ControllerTerminalResolution::ReleasedAfterPostInstalledFailure,
                })
            }
            None => {
                self.clear_released_active_media_projection();
                None
            }
            Some(state) => {
                self.install_state = Some(state);
                return self
                    .fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
            }
        };
        self.publish_view(false);
        Ok(drain)
    }

    /// Неизвестный итог release запрещает controller-у публиковать ложное recovery.
    pub(crate) fn report_post_installed_compensation_failure(&mut self) {
        self.set_fatal(PlaylistControllerInvariantViolation::PostInstalledCandidateReleaseFailed);
    }

    /// После exact player release controller больше не публикует старый instance как active.
    fn clear_released_active_media_projection(&mut self) {
        self.active_media = None;
        self.detached_active_tombstone = None;
        self.replacement_detached_disposition = None;
        self.transport_disposition = AppTransportDisposition::Stopped;
    }
}
