//! App-facing boundary для классификации результата главного local file picker-а.

use std::path::Path;

use super::PlaylistRuntime;
use super::controller::{LocalFileQueueReplacementReason, LocalFileSelectionDisposition};

impl PlaylistRuntime {
    /// Решает, можно ли открыть exact committed row без destructive queue replacement.
    pub(crate) fn classify_in_app_local_file_selection(
        &mut self,
        selected_path: &Path,
    ) -> LocalFileSelectionDisposition {
        self.discovery.cancel_initial_queue_playback();
        self.controller
            .as_ref()
            .map(|controller| controller.classify_local_file_selection(selected_path))
            .unwrap_or(LocalFileSelectionDisposition::ReplaceQueue {
                reason: LocalFileQueueReplacementReason::NoCurrentLocalDirectory,
            })
    }
}
