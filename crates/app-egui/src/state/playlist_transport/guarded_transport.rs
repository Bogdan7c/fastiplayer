//! App-side поддержка transport-команд, которые остановил playlist install guard.
//!
//! Главная задача — освобождение pending media open, который controller guard снял ради
//! новой команды.
//!
//! Controller уже забыл старый install (`CancelPendingThenExecute`/exact abort), но
//! coordinator request ещё жив. Если его не отменить, он дойдёт до Installed без playlist
//! binding-а и проиграется как «посторонний» файл. Отмену выполняет тот, кто владеет
//! terminal-ом request-а:
//! - playlist transport (`active_request_id`) — lossless cancel здесь;
//! - startup orchestration (`pending_strong_media_open`) — через её штатный supersede, чтобы
//!   её terminal policy трактовала отмену как cancel-win, а не как ошибку restore.
//!
//! Новый install, запланированный за чужим request-ом, ждёт освобождения strong-open слота.

use tracing::{debug, warn};

use super::QueuedPlaylistInstall;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::{PlannedPlaylistInstall, PlaylistRuntime, ReleasedPendingRequest};
use crate::state::AppState;

/// Кто отвечает за terminal освобождаемого request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupersededRequestOwner {
    /// Request запущен playlist transport-ом; его terminal забирает `poll_playlist_transport`.
    PlaylistTransport,
    /// Request запущен startup orchestration; её poll сам отменит и заберёт terminal.
    StartupOrchestration,
    /// Request уже завершён и забран — отменять нечего.
    AlreadyFinished,
}

impl AppState {
    /// Отменяет pending request, который controller guard уже снял со своей стороны.
    pub(crate) fn release_superseded_playlist_request(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        released: ReleasedPendingRequest,
    ) {
        match self.superseded_request_owner(released.request_id) {
            SupersededRequestOwner::PlaylistTransport => {
                self.playlist_transport.released_by_guard_request = Some(released.request_id);
                if let Err(error) =
                    playlist_runtime.cancel_media_open_lossless(released.request_id, released.cause)
                {
                    warn!(error = %error, "Не удалось отменить playlist request, снятый guard-ом");
                }
            }
            SupersededRequestOwner::StartupOrchestration => {
                // Startup poll увидит supersede, отменит свой request и применит cancel-win.
                playlist_runtime.supersede_startup_media_apply();
                self.playlist_transport.queued_behind_foreign_request = Some(released.request_id);
            }
            SupersededRequestOwner::AlreadyFinished => {
                debug!(
                    request_id = ?released.request_id,
                    "Request, снятый guard-ом, уже завершён"
                );
            }
        }
        self.mark_pending_worker_redraw();
    }

    /// Позиция последнего player snapshot-а для D17 при отложенном исполнении команды.
    pub(crate) fn last_known_player_position(&self) -> std::time::Duration {
        self.last_player_snapshot.current_position
    }

    /// Strong-open слот свободен: отложенная команда может стартовать новый request.
    pub(crate) fn strong_media_open_slot_is_idle(&self) -> bool {
        self.playlist_transport.active_request_id.is_none()
            && self.pending_strong_media_open.is_none()
    }

    /// Install за чужим request-ом ставится в очередь вместо немедленного `Busy`.
    ///
    /// Возвращает install обратно, если ждать нечего и его можно запускать сразу.
    pub(super) fn queue_install_behind_foreign_request(
        &mut self,
        install: PlannedPlaylistInstall,
    ) -> Option<PlannedPlaylistInstall> {
        let Some(foreign_request_id) = self.playlist_transport.queued_behind_foreign_request else {
            return Some(install);
        };
        if !self.foreign_request_is_pending(foreign_request_id) {
            self.playlist_transport.queued_behind_foreign_request = None;
            return Some(install);
        }
        self.playlist_transport.queued_install = Some(QueuedPlaylistInstall {
            install,
            supersedes: None,
        });
        self.mark_pending_worker_redraw();
        None
    }

    /// Когда чужой request завершился, забирает install, ждавший освобождения слота.
    pub(super) fn take_install_released_by_foreign_request(
        &mut self,
    ) -> Option<PlannedPlaylistInstall> {
        let foreign_request_id = self.playlist_transport.queued_behind_foreign_request?;
        if self.foreign_request_is_pending(foreign_request_id) {
            return None;
        }
        self.playlist_transport.queued_behind_foreign_request = None;
        self.playlist_transport
            .queued_install
            .take()
            .map(|queued| queued.install)
    }

    /// Terminal request-а, отменённого guard-ом, не должен становиться D55 failure target-а.
    ///
    /// Возвращает `true` ровно один раз для совпадающего request-а и снимает отметку.
    pub(super) fn take_guard_released_terminal(&mut self, request_id: MediaOpenRequestId) -> bool {
        if self.playlist_transport.released_by_guard_request == Some(request_id) {
            self.playlist_transport.released_by_guard_request = None;
            return true;
        }
        false
    }

    fn superseded_request_owner(&self, request_id: MediaOpenRequestId) -> SupersededRequestOwner {
        if self.playlist_transport.active_request_id == Some(request_id) {
            SupersededRequestOwner::PlaylistTransport
        } else if self.foreign_request_is_pending(request_id) {
            SupersededRequestOwner::StartupOrchestration
        } else {
            SupersededRequestOwner::AlreadyFinished
        }
    }

    fn foreign_request_is_pending(&self, request_id: MediaOpenRequestId) -> bool {
        self.pending_strong_media_open
            .as_ref()
            .is_some_and(|pending| pending.request_id() == request_id)
    }
}
