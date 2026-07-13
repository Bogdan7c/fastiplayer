use super::*;
use crate::{
    AuthorizeInstallCommit, MediaInstallControl, MediaInstallControlOutcome, PlaybackIntent,
    PlaybackIntentRevision,
};

impl PlayerWorkerRuntime {
    /// Тонко адаптирует startup/settings callsites к единому strong player install algorithm.
    ///
    /// Facade создаёт typed initial intent и auto-authorize-ит только matching ready request.
    /// До Session 10D app ещё не передаёт detached candidate port, поэтому video backend
    /// поднимается существующим post-install app adapter-ом; destructive player path удалён.
    pub(super) fn handle_compatibility_media_install(
        &mut self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        autoplay: bool,
        install_port: Arc<dyn MediaInstallPhaseCompletionPort>,
    ) {
        self.session.stage_prepared_media_install_compatibility(
            request_id,
            prepared_media,
            PlaybackIntent::from_autoplay(autoplay),
            PlaybackIntentRevision::INITIAL,
            install_port,
        );

        if self.session.has_staged_media_install() {
            let authorization_outcome =
                self.session
                    .apply_staged_media_install_control(MediaInstallControl::Authorize(
                        AuthorizeInstallCommit { request_id },
                    ));
            debug_assert_eq!(
                authorization_outcome,
                MediaInstallControlOutcome::AuthorizationAccepted
            );
        }
    }
}
