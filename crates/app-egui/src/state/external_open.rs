//! Склейка `AppState` с модулем `external_open`: исполнение брошенных в окно файлов.
//!
//! Здесь живёт реализация контракта [`ExternalOpenHost`]: каждый шаг идёт по уже
//! существующему пути владельца (Open как у кнопки, подтверждение замены очереди,
//! Manual Add), собственных обходов coordinator-а нет.

use std::path::PathBuf;

use fastiplayer_config::DroppedPlaylistFileAction;
use tracing::{info, warn};

use super::AppState;
use crate::external_open::request::DroppedWebUrl;
use crate::external_open::{
    DropGestureEvent, DropNotice, DropTarget, ExternalOpenHost, dispatch_external_open_request,
};
use crate::playlist_runtime::{
    DroppedCollectionDestination, DroppedCollectionEntry, DroppedCollectionTruncation,
    DroppedCollectionWalkStartError, InAppQueueReplacementIntent, LocalFilesReplacementOutcome,
    PlaylistImportIntent, PlaylistPathImportStart, PlaylistRuntime,
};

mod dropped_collection;
mod resolved_url_collection;
mod web_url;

impl AppState {
    /// Применяет событие жеста; на броске исполняет ровно один запрос открытия.
    ///
    /// Возвращает `true`, если окну нужна перерисовка (подсветка, уведомление, смена очереди).
    pub(crate) fn handle_external_drop_gesture(
        &mut self,
        event: DropGestureEvent,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let pixels_per_point = self.egui_ctx.pixels_per_point();
        if let Some(request) = self
            .external_open
            .apply_gesture_event(event, pixels_per_point)
        {
            let mut host = AppStateExternalOpenHost {
                app_state: self,
                playlist_runtime,
                renderer,
            };
            let outcome = dispatch_external_open_request(&mut host, request);
            info!(?outcome, "Брошенные в окно данные обработаны");
        }
        // Любое событие жеста меняет подсветку, поэтому кадр нужен всегда.
        true
    }

    /// Принимает устаревшее оконное событие файла (платформы без внешнего канала drag).
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn ingest_legacy_file_drop_event(
        &mut self,
        event: &winit::event::WindowEvent,
    ) -> Option<DropGestureEvent> {
        self.external_open.ingest_legacy_window_event(event)
    }

    /// Забирает бросок устаревшего источника, накопленный за проход event loop.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn take_legacy_file_drop(&mut self) -> Option<DropGestureEvent> {
        self.external_open.take_legacy_completed_drop()
    }

    /// Группа файлов после подтверждения (или сразу при пустой очереди): новая очередь + play.
    pub(crate) fn replace_queue_with_admitted_local_files(
        &mut self,
        paths: Vec<PathBuf>,
        truncation: Option<DroppedCollectionTruncation>,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        match playlist_runtime.replace_queue_with_local_files(paths) {
            LocalFilesReplacementOutcome::Replaced {
                first_item,
                file_count,
            } => {
                info!(file_count, "Очередь заменена брошенными файлами");
                // Файлы реально добавлены (пустая очередь или Confirm): только теперь
                // честно сообщаем, что набор усечён лимитом обхода.
                if let Some(notice) = dropped_collection::truncation_notice(truncation, file_count)
                {
                    self.notify_info(notice.text());
                }
                let row_play = playlist_runtime.play_playlist_row(first_item);
                if !crate::transport_runtime::apply_playlist_row_play(
                    self,
                    playlist_runtime,
                    renderer,
                    row_play,
                ) {
                    self.set_startup_error("Не удалось начать воспроизведение файлов".to_string());
                }
                self.mark_pending_worker_redraw();
            }
            LocalFilesReplacementOutcome::NoFilesProvided => {
                self.notify_info(DropNotice::NothingToOpen.text());
            }
            LocalFilesReplacementOutcome::TooManyFiles => {
                self.notify_info(DropNotice::TooManyFiles.text());
            }
            LocalFilesReplacementOutcome::LoadDecisionPending => {
                self.notify_info(DropNotice::PlaylistStillLoading.text());
            }
            LocalFilesReplacementOutcome::InstallInProgress => self.notify_open_still_in_progress(),
            LocalFilesReplacementOutcome::Rejected => {
                warn!("Замена очереди брошенными файлами отклонена владельцем очереди");
                self.notify_info("Не удалось открыть файлы".to_string());
            }
        }
    }

    /// Несколько файлов на видео: admission замены очереди (подтверждение, если очередь не пуста).
    fn request_queue_replacement_with_dropped_files(
        &mut self,
        paths: Vec<PathBuf>,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        let intent = InAppQueueReplacementIntent::local_files(paths);
        self.request_queue_replacement_with_intent(intent, playlist_runtime, renderer);
    }

    /// Общая admission замены очереди набором файлов: подпись intent-а выбирает вызывающий.
    fn request_queue_replacement_with_intent(
        &mut self,
        intent: InAppQueueReplacementIntent,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        // До load decision очереди ещё нет: admission записал бы startup-замену, а менять
        // было бы нечего. Отказываем заранее и ничего не трогаем.
        if playlist_runtime.queue_load_decision_is_pending() {
            self.notify_info(DropNotice::PlaylistStillLoading.text());
            return;
        }
        let admission = playlist_runtime.admit_in_app_queue_replacement(intent);
        self.apply_in_app_queue_replacement_admission(admission, playlist_runtime, renderer);
    }
}

impl AppState {
    /// Добавляет файлы в конец очереди (Manual Add) без подтверждения: очередь не заменяется.
    pub(super) fn append_dropped_media_files(
        &mut self,
        paths: Vec<PathBuf>,
        playlist_runtime: &mut PlaylistRuntime,
    ) {
        if playlist_runtime.queue_load_decision_is_pending() {
            self.notify_info(DropNotice::PlaylistStillLoading.text());
            return;
        }
        // Порядок броска/обхода папок сохраняем (решение владельца 10), не сортируем.
        match playlist_runtime.start_manual_file_add_in_given_order(paths) {
            Ok(_job_id) => self.mark_pending_worker_redraw(),
            Err(error) => {
                warn!(?error, "Manual Add из брошенных файлов не стартовал");
                self.notify_info("Не удалось добавить файлы".to_string());
            }
        }
    }
}

/// Прямоугольник панели плейлиста для hit-test: только если в sidebar открыт именно плейлист.
///
/// Настройки, URL и инфо-панель не принимают файлы как «добавить в плейлист», поэтому
/// бросок на них идёт на видео.
pub(super) fn playlist_panel_rect(
    sidebar_output: Option<&crate::ui::sidebar::SidebarOutput>,
    displayed_section: Option<super::SidebarSection>,
) -> Option<egui::Rect> {
    let sidebar_output = sidebar_output?;
    (displayed_section == Some(super::SidebarSection::Playlist)).then_some(sidebar_output.rect)
}

/// Хост исполнения запроса: временно соединяет `AppState`, runtime плейлиста и renderer.
struct AppStateExternalOpenHost<'host> {
    app_state: &'host mut AppState,
    playlist_runtime: &'host mut PlaylistRuntime,
    renderer: &'host render_wgpu_shell::Renderer,
}

impl ExternalOpenHost for AppStateExternalOpenHost<'_> {
    fn open_in_progress(&self) -> bool {
        self.app_state.has_pending_local_file_open()
            || self.playlist_runtime.has_playlist_import_in_flight()
            || self
                .playlist_runtime
                .has_dropped_collection_walk_in_flight()
            // Идущий импорт ссылки («Получаем данные по ссылке…») — тоже открытие в процессе.
            || self.playlist_runtime.playlist_url_import_progress().is_some()
    }

    fn notify_open_still_in_progress(&mut self) {
        self.app_state.notify_open_still_in_progress();
    }

    fn notify_info(&mut self, message: String) {
        self.app_state.notify_info(message);
    }

    fn open_single_media_file_like_open_button(&mut self, path: PathBuf) {
        self.app_state
            .open_selected_local_file(path, self.playlist_runtime, self.renderer);
    }

    fn replace_queue_with_media_files(&mut self, paths: Vec<PathBuf>) {
        self.app_state.request_queue_replacement_with_dropped_files(
            paths,
            self.playlist_runtime,
            self.renderer,
        );
    }

    fn append_media_files_to_queue(&mut self, paths: Vec<PathBuf>) {
        self.app_state
            .append_dropped_media_files(paths, self.playlist_runtime);
    }

    fn start_dropped_collection_walk(
        &mut self,
        entries: Vec<DroppedCollectionEntry>,
        target: DropTarget,
    ) {
        if self.playlist_runtime.queue_load_decision_is_pending() {
            self.app_state
                .notify_info(DropNotice::PlaylistStillLoading.text());
            return;
        }
        let destination = match target {
            DropTarget::Video => DroppedCollectionDestination::ReplaceQueue,
            DropTarget::Playlist => DroppedCollectionDestination::AppendToQueue,
        };
        match self
            .playlist_runtime
            .start_dropped_collection_walk(entries, destination)
        {
            Ok(()) => {}
            Err(DroppedCollectionWalkStartError::AlreadyInFlight) => {
                self.app_state.notify_open_still_in_progress();
            }
            Err(error @ DroppedCollectionWalkStartError::RuntimeClosed) => {
                warn!(
                    ?error,
                    "Обход брошенных папок не принят: runtime закрывается"
                );
            }
            Err(DroppedCollectionWalkStartError::SpawnFailed) => {
                self.app_state
                    .notify_info(DropNotice::FolderUnreadable.text());
            }
        }
    }

    fn dropped_playlist_file_action(&self) -> DroppedPlaylistFileAction {
        self.playlist_runtime.dropped_playlist_file_action()
    }

    fn import_playlist_file(&mut self, path: PathBuf, intent: PlaylistImportIntent) {
        if self.playlist_runtime.queue_load_decision_is_pending() {
            self.app_state
                .notify_info(DropNotice::PlaylistStillLoading.text());
            return;
        }
        match self
            .playlist_runtime
            .start_playlist_import_path(path, intent)
        {
            PlaylistPathImportStart::Started => self.app_state.mark_pending_worker_redraw(),
            PlaylistPathImportStart::ImportAlreadyRunning => {
                self.app_state.notify_open_still_in_progress();
            }
            PlaylistPathImportStart::RuntimeClosed | PlaylistPathImportStart::SpawnFailed => {
                self.app_state
                    .notify_info("Не удалось открыть плейлист".to_string());
            }
        }
    }

    fn add_web_url_like_add_url_button(&mut self, url: DroppedWebUrl) {
        self.app_state
            .add_dropped_web_url_to_playlist(&url, self.playlist_runtime);
    }

    fn replace_queue_with_web_url(&mut self, url: DroppedWebUrl) {
        self.app_state
            .request_queue_replacement_with_dropped_web_url(
                &url,
                self.playlist_runtime,
                self.renderer,
            );
    }
}
