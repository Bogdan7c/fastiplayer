//! Clear-specific controller policy: одна queue mutation и полный exact media reset.

use playlist_core::ClearQueueOutcome;

use super::{
    ControllerDestructiveRemovalOutcome, ControllerRemovalKind, PlaybackProgressAtClear,
    RemovalMutationError, RemovedActiveMediaPolicy, SelectionAfterRemoval,
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
    ///
    /// `progress` — позиция из player-а: вместе с тем, что controller знает сам (элемент и
    /// Play/Pause), она попадает в `ClearPlaybackResume`, чтобы Undo вернул воспроизведение.
    pub(crate) fn clear_queue(
        &mut self,
        progress: PlaybackProgressAtClear,
    ) -> ControllerDestructiveRemovalOutcome {
        // Снимок обязан предшествовать мутации: Clear сбрасывает active identity и pending.
        let playback_resume = self.capture_playback_resume_for_clear(progress);
        let outcome = self.commit_clear_queue();
        match outcome {
            ControllerDestructiveRemovalOutcome::Removed(mut removal) => {
                removal.playback_resume = playback_resume;
                ControllerDestructiveRemovalOutcome::Removed(removal)
            }
            other => other,
        }
    }

    fn commit_clear_queue(&mut self) -> ControllerDestructiveRemovalOutcome {
        // Snapshot exact ID-ов нужен общей removal transaction для одной domain mutation.
        let removed_entry_ids = self.queue.iter_top_level_entry_ids().collect::<Vec<_>>();
        self.commit_destructive_removal(
            ControllerRemovalKind::Clear,
            removed_entry_ids.into(),
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
