//! Файл очереди не подготовился до того, как controller принял его план (сессия 07).
//!
//! Controller принимает план только на staging, после успешной подготовки файла. Если файл
//! удалён или повреждён, подготовка падает раньше: strong-open возвращает неиспользованный
//! план (`PlaylistStrongMediaOpenPoll::TargetFailedBeforeAdmission`), и здесь он уходит
//! владельцу очереди. Автоматический пропуск продолжает тот же фиксированный план (C после
//! битого B), ручной выбор сохраняет прежнюю D55-семантику.

use render_wgpu_shell::Renderer;
use tracing::{debug, warn};

use super::playlist_target_failure_summary;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::AppState;
use crate::state::strong_media_open::UnstagedPlaylistMediaOpenError;

impl AppState {
    /// Завершает playlist request, чей target не подготовился до controller admission.
    ///
    /// Порядок проверок тот же, что у обычной ошибки request-а: ожидающая замена и снятие
    /// guard-ом важнее — пользователь уже выбрал другое, план выбрасывается.
    pub(super) fn finish_playlist_target_failed_before_admission(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        active_request_id: MediaOpenRequestId,
        unstaged: UnstagedPlaylistMediaOpenError,
    ) {
        let UnstagedPlaylistMediaOpenError { error, install } = unstaged;
        let failed_request_id = error.terminal_request_id().unwrap_or(active_request_id);
        self.playlist_transport.active_request_id = None;
        self.playlist_transport.active_item_id = None;
        if let Some(queued) = self.playlist_transport.queued_install.take() {
            debug!(error = %error, "Ошибка target-а до admission вытеснена новым playlist plan-ом");
            self.begin_planned_playlist_install(
                playlist_runtime,
                renderer,
                queued.install,
                queued.supersedes,
            );
            return;
        }
        if self.take_guard_released_terminal(failed_request_id) {
            debug!(error = %error, "Playlist request до admission завершён отменой по guard");
            return;
        }
        warn!(error = %error, "Файл очереди не подготовился до admission");
        let continuation = playlist_runtime.report_playlist_target_failure_before_admission(
            failed_request_id,
            *install,
            playlist_target_failure_summary(&error),
        );
        if let Some(next_install) = continuation {
            self.begin_planned_playlist_install(playlist_runtime, renderer, next_install, None);
        }
    }
}
