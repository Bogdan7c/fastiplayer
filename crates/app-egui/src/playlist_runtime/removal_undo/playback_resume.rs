//! Undo Clear возвращает не только строки, но и воспроизведение (решение владельца B7).
//!
//! Slot Undo уже владеет снимком Clear, в котором controller сохранил «что играло и где»
//! (`ClearPlaybackResume`). Здесь runtime связывает успешный restore очереди с запуском этого
//! воспроизведения через обычный путь Play row, а не отдельным обходом.

use std::time::Instant;

use super::RemovalUndoOutcome;
use crate::playlist_runtime::PlaylistRuntime;
use crate::playlist_runtime::controller::ControllerPlayItemOutcome;

/// Итог Undo: что сделано с очередью и (если было что возвращать) план запуска воспроизведения.
///
/// `playback` — тот же `ControllerPlayItemOutcome`, который app исполняет для клика по строке
/// (`apply_playlist_row_play`); `None` — возвращать воспроизведение не нужно.
pub(crate) struct RemovalUndoWithPlayback {
    pub(crate) outcome: RemovalUndoOutcome,
    pub(crate) playback: Option<ControllerPlayItemOutcome>,
}

impl PlaylistRuntime {
    /// Undo последнего удаления; для Clear с играющим элементом дополнительно планирует его
    /// повторное открытие с прежней позицией и Play/Pause.
    ///
    /// Воспроизведение планируется только при `RemovalUndoOutcome::Restored`: просроченный,
    /// инвалидированный или отклонённый Undo ничего не открывает и не трогает.
    pub(crate) fn undo_last_removal_resuming_playback(
        &mut self,
        now: Instant,
    ) -> RemovalUndoWithPlayback {
        // Slot освобождается внутри undo_last_removal, поэтому снимок читаем заранее.
        let playback_resume = self
            .removal_undo
            .as_ref()
            .and_then(|undo| undo.removal.playback_resume);
        let outcome = self.undo_last_removal(now);
        let playback = match (&outcome, playback_resume) {
            (RemovalUndoOutcome::Restored { .. }, Some(resume)) => {
                self.supersede_competing_openers_for_row_play();
                self.controller
                    .as_mut()
                    .map(|controller| controller.resume_playback_after_clear_undo(resume))
            }
            _ => None,
        };
        RemovalUndoWithPlayback { outcome, playback }
    }
}
