//! Exact fake-able adapter существующего ordered `player-core` control stream-а.

use player_core::{
    AuthorizeInstallCommit, CancelMediaInstall, MediaInstallCancellationCause,
    MediaInstallCompletion, MediaInstallControlOutcome, MediaInstallPhase, MediaInstallRequestId,
    MediaInstallVideoResourcePort, PlaybackIntentUpdate, PlaybackIntentUpdateReceipt,
    PlayerCommandSender, PlayerWorkerSendError,
};

use super::{MediaOpenInstallIntent, PlayerDispatchRejection};

pub(super) trait InstallReceiptPort: Send {
    fn take_ready(&self) -> Option<MediaInstallPhase>;
    fn take_completion(&self) -> Option<MediaInstallCompletion>;
}

impl InstallReceiptPort for player_core::MediaInstallReceipt {
    fn take_ready(&self) -> Option<MediaInstallPhase> {
        self.try_take_ready_to_commit()
    }

    fn take_completion(&self) -> Option<MediaInstallCompletion> {
        self.try_take_completion()
    }
}

pub(super) trait ControlReceiptPort: Send {
    fn take_outcome(&self) -> Result<Option<MediaInstallControlOutcome>, ()>;
}

impl ControlReceiptPort for player_core::MediaInstallControlReceipt {
    fn take_outcome(&self) -> Result<Option<MediaInstallControlOutcome>, ()> {
        self.try_take_outcome().map_err(|_| ())
    }
}

/// Production adapter делегирует ports без coordinator-owned queue/token semantics.
pub(super) trait MediaOpenPlayerPort: Send + Sync {
    fn stage(
        &self,
        request_id: MediaInstallRequestId,
        prepared_media: player_core::PreparedMedia,
        intent: MediaOpenInstallIntent,
        video_resource_port: MediaInstallVideoResourcePort,
    ) -> Result<Box<dyn InstallReceiptPort>, PlayerDispatchRejection>;
    fn authorize(
        &self,
        request_id: MediaInstallRequestId,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection>;
    fn cancel(
        &self,
        request_id: MediaInstallRequestId,
        cause: MediaInstallCancellationCause,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection>;
    fn update_intent(
        &self,
        update: PlaybackIntentUpdate,
    ) -> Result<PlaybackIntentUpdateReceipt, PlayerDispatchRejection>;
}

impl MediaOpenPlayerPort for PlayerCommandSender {
    fn stage(
        &self,
        request_id: MediaInstallRequestId,
        prepared_media: player_core::PreparedMedia,
        intent: MediaOpenInstallIntent,
        video_resource_port: MediaInstallVideoResourcePort,
    ) -> Result<Box<dyn InstallReceiptPort>, PlayerDispatchRejection> {
        self.stage_prepared_media_install(
            request_id,
            prepared_media,
            intent.intent,
            intent.revision,
            video_resource_port,
        )
        .map(|receipt| Box::new(receipt) as Box<dyn InstallReceiptPort>)
        .map_err(map_player_send_error)
    }

    fn authorize(
        &self,
        request_id: MediaInstallRequestId,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
        self.authorize_install_commit(AuthorizeInstallCommit { request_id })
            .map(|receipt| Box::new(receipt) as Box<dyn ControlReceiptPort>)
            .map_err(map_player_send_error)
    }

    fn cancel(
        &self,
        request_id: MediaInstallRequestId,
        cause: MediaInstallCancellationCause,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
        self.cancel_media_install(CancelMediaInstall { request_id, cause })
            .map(|receipt| Box::new(receipt) as Box<dyn ControlReceiptPort>)
            .map_err(map_player_send_error)
    }

    fn update_intent(
        &self,
        update: PlaybackIntentUpdate,
    ) -> Result<PlaybackIntentUpdateReceipt, PlayerDispatchRejection> {
        self.update_playback_intent(update)
            .map_err(map_player_send_error)
    }
}

fn map_player_send_error(error: PlayerWorkerSendError) -> PlayerDispatchRejection {
    match error {
        PlayerWorkerSendError::Full => PlayerDispatchRejection::Backpressure,
        PlayerWorkerSendError::Disconnected => PlayerDispatchRejection::Disconnected,
    }
}
