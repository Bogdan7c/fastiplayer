//! Clear-specific controller policy: одна queue mutation и полный exact media reset.

use playlist_core::ClearQueueOutcome;

use super::{
    ControllerDestructiveRemovalOutcome, ControllerRemovalKind, RemovalMutationError,
    RemovedActiveMediaPolicy, SelectionAfterRemoval,
};
use crate::playlist_runtime::controller::PlaylistController;

/// Receipt commit различает настоящий `Stopped` и выигравший новый Installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerClearMediaResetCommit {
    CommittedStopped,
    SupersededByActiveMedia,
}

impl PlaylistController {
    /// Clear очищает queue и сразу отделяет Undo от прежнего playback lifecycle.
    pub(crate) fn clear_queue(&mut self) -> ControllerDestructiveRemovalOutcome {
        // Snapshot exact ID-ов нужен общей removal transaction для одной domain mutation.
        let removed_item_ids = self.queue.iter_playable_ids().collect::<Vec<_>>();
        self.commit_destructive_removal(
            ControllerRemovalKind::Clear,
            removed_item_ids.into(),
            SelectionAfterRemoval::Clear,
            RemovedActiveMediaPolicy::ResetCurrentMedia,
            |controller| match controller.queue.clear() {
                ClearQueueOutcome::Cleared {
                    current_outcome, ..
                } => Ok(current_outcome),
                ClearQueueOutcome::AlreadyEmpty => Err(RemovalMutationError::NoChange),
                ClearQueueOutcome::InstallCommitLinearizing => {
                    Err(RemovalMutationError::InstallCommitLinearizing)
                }
                ClearQueueOutcome::StructuralRevisionExhausted
                | ClearQueueOutcome::TraversalRevisionExhausted => {
                    Err(RemovalMutationError::DomainRevisionExhausted)
                }
            },
        )
    }

    /// Receipt фиксирует `Stopped` только пока Clear identity не заменена новым Installed.
    pub(crate) fn commit_clear_media_reset_stopped(&mut self) -> ControllerClearMediaResetCommit {
        if self.active_media.is_some() {
            return ControllerClearMediaResetCommit::SupersededByActiveMedia;
        }
        self.transport_disposition =
            crate::playlist_runtime::controller::AppTransportDisposition::Stopped;
        ControllerClearMediaResetCommit::CommittedStopped
    }
}
