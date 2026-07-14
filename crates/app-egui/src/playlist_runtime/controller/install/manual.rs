//! Manual preview/token integration with the generic D08/D39 install guard.

use super::*;

impl PlaylistController {
    pub(crate) fn manual_navigation_install_phase(
        &self,
    ) -> Option<(ControllerInstallPhase, MediaOpenRequestId)> {
        let state = self.install_state.as_ref()?;
        (self.manual_navigation_cursor.request_id() == Some(state.request_id()))
            .then_some((state.phase(), state.request_id()))
    }

    pub(crate) fn abort_reserved_manual_navigation_before_dispatch(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<Option<PlaylistDirtySignal>, PlaylistControllerInvariantViolation> {
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
        let GuardedInstall {
            token,
            desired_modes,
            ..
        } = guarded;
        match token.abort(&mut self.queue) {
            GuardedInstallAbort::ManualNavigation(preview) => {
                self.manual_navigation_cursor.restore_after_abort(preview)
            }
            GuardedInstallAbort::Queue => {
                return self
                    .fatal_result(PlaylistControllerInvariantViolation::TerminalRequestMismatch);
            }
        }
        self.pending_target = None;
        let dirty = self.apply_desired_modes(desired_modes)?;
        self.publish_view(false);
        Ok(dirty)
    }

    pub(crate) fn retire_awaiting_manual_navigation_request(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Result<(), PlaylistControllerInvariantViolation> {
        let Some(state) = self.install_state.take() else {
            return Ok(());
        };
        let InstallState::AwaitingReady(awaiting) = state else {
            self.install_state = Some(state);
            return self.fatal_result(PlaylistControllerInvariantViolation::UnexpectedInstallPhase);
        };
        if awaiting.request.request_id != request_id {
            self.install_state = Some(InstallState::AwaitingReady(awaiting));
            return self
                .fatal_result(PlaylistControllerInvariantViolation::TerminalRequestMismatch);
        }
        self.pending_target = None;
        self.publish_view(false);
        Ok(())
    }
}
