//! Настоящий порт повтора: передаёт вызовы существующим владельцам путей открытия.

use std::path::PathBuf;
use std::time::Duration;

use playlist_core::PlaylistItemId;
use render_wgpu_shell::Renderer;
use tracing::debug;

use super::{
    LocalOpenSlot, MediaRetryPort, RETRY_TARGET_GONE_MESSAGE, RetryStart, execute_media_retry,
};
use crate::external_open::request::DroppedWebUrl;
use crate::playlist_runtime::{PlaylistRuntime, RuntimeRowPlayOutcome};
use crate::state::AppState;
use crate::state::media_retry::ReopenPosition;

/// Порт на один кадр: `AppState` + очередь + renderer, как у остальных действий кадра.
struct AppRetryPort<'a> {
    app_state: &'a mut AppState,
    playlist_runtime: &'a mut PlaylistRuntime,
    renderer: &'a Renderer,
}

impl AppRetryPort<'_> {
    /// Исполняет Play строки; `None` (нет элемента) и отказ запуска дают `NothingToOpen`.
    fn start_row_play(&mut self, outcome: Option<RuntimeRowPlayOutcome>) -> RetryStart {
        let started = outcome.is_some_and(|outcome| {
            crate::transport_runtime::apply_playlist_row_play(
                self.app_state,
                self.playlist_runtime,
                self.renderer,
                outcome,
            )
        });
        if started {
            RetryStart::Started
        } else {
            RetryStart::NothingToOpen
        }
    }
}

impl MediaRetryPort for AppRetryPort<'_> {
    fn local_open_slot(&self) -> LocalOpenSlot {
        if self.app_state.has_pending_local_file_open() {
            LocalOpenSlot::Busy
        } else {
            LocalOpenSlot::Free
        }
    }

    fn report_still_opening(&mut self) {
        self.app_state.notify_open_still_in_progress();
    }

    fn clear_failure(&mut self) {
        self.app_state.resolve_media_failure_for_retry();
    }

    fn open_local_file(&mut self, path: PathBuf) {
        self.app_state
            .open_selected_local_file(path, self.playlist_runtime, self.renderer);
    }

    fn open_web_url(&mut self, url: &DroppedWebUrl) {
        self.app_state
            .request_queue_replacement_with_dropped_web_url(
                url,
                self.playlist_runtime,
                self.renderer,
            );
    }

    fn play_queue_item(&mut self, item_id: PlaylistItemId) -> RetryStart {
        let outcome = self.playlist_runtime.play_playlist_row(item_id);
        self.start_row_play(Some(outcome))
    }

    fn resume_blocked_queue(&mut self) -> RetryStart {
        let outcome = self.playlist_runtime.play_queue_blocked_item();
        self.start_row_play(outcome)
    }

    fn reopen_failed_playback(&mut self, position: ReopenPosition) -> RetryStart {
        let from = match position {
            ReopenPosition::FromStart => Duration::ZERO,
            ReopenPosition::At(position) => position,
        };
        let outcome = self.playlist_runtime.reopen_failed_active_item_from(from);
        self.start_row_play(outcome)
    }

    fn report_nothing_to_retry(&mut self) {
        self.app_state.notify_transient(RETRY_TARGET_GONE_MESSAGE);
    }
}

impl AppState {
    /// Исполняет принятое нажатие «Повторить» (если оно было в этом кадре).
    pub(crate) fn apply_requested_media_retry(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) {
        let Some(target) = self.notifications.take_retry_request() else {
            return;
        };
        debug!(?target, "Повтор открытия по кнопке «Повторить»");
        execute_media_retry(
            &mut AppRetryPort {
                app_state: self,
                playlist_runtime,
                renderer,
            },
            target,
        );
        self.mark_pending_worker_redraw();
    }

    /// Снимает ошибку открытия, потому что повтор стартовал.
    fn resolve_media_failure_for_retry(&mut self) {
        self.notifications.resolve_media_failure();
    }
}
