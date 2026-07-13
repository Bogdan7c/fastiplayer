use super::*;
use crate::media_install::MediaInstallProtocol;
use crate::session::CompatibilityMediaInstallOutcome;
use crate::{MediaInstallControl, MediaInstallControlOutcome};

impl PlayerWorkerRuntime {
    /// Адаптирует существующий destructive single-media call graph к новому completion port.
    ///
    /// Важное ограничение: legacy lifecycle сначала выполняется целиком. Только successful
    /// outcome ретроспективно проходит ready + internal authorization в том же worker turn.
    /// Поэтому adapter сохраняет observable single-media behavior, но не называется strong
    /// transaction и не используется как доказательство old-resource preservation.
    pub(super) fn handle_compatibility_media_install(
        &mut self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        autoplay: bool,
        install_port: Arc<dyn MediaInstallPhaseCompletionPort>,
    ) {
        self.session
            .cancel_active_staged_media_install(crate::MediaInstallCancellationCause::Superseded);
        let mut protocol = MediaInstallProtocol::accept(request_id, install_port);

        match self
            .session
            .load_prepared_media_compatibility(prepared_media, autoplay)
        {
            CompatibilityMediaInstallOutcome::Installed(media_instance_id) => {
                protocol.mark_ready_to_commit();
                let authorization_outcome = protocol.apply_control(
                    MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
                    || media_instance_id,
                );
                debug_assert_eq!(
                    authorization_outcome,
                    MediaInstallControlOutcome::AuthorizationAccepted
                );
            }
            CompatibilityMediaInstallOutcome::Failed(failure) => {
                protocol.complete_failed(failure);
            }
        }
    }
}
