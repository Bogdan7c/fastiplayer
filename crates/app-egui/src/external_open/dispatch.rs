//! Исполнение внешнего запроса открытия через существующие границы владельцев.
//!
//! Модуль ничего не знает про `AppState`/`PlaylistRuntime`: он работает через узкий
//! контракт [`ExternalOpenHost`], где каждый метод назван по намерению. Реализация хоста
//! (`state::external_open`) обязана вести каждый шаг по уже существующим путям —
//! coordinator, подтверждение замены очереди, Manual Add — и не вводить обход.

use std::path::PathBuf;

use fastiplayer_config::DroppedPlaylistFileAction;

use super::classify::classify_request;
use super::playlist_intent::playlist_import_intent;
use super::request::DroppedWebUrl;
use super::request::{DropTarget, ExternalOpenRequest};
use super::route::{OpenStep, route_plan};
use crate::playlist_runtime::{DroppedCollectionEntry, PlaylistImportIntent};

/// Что хост-приложение умеет сделать по запросу открытия.
pub(crate) trait ExternalOpenHost {
    /// Идёт ли открытие/импорт, из-за которого новый бросок надо отклонить.
    fn open_in_progress(&self) -> bool;
    /// Сообщить «Файл ещё открывается».
    fn notify_open_still_in_progress(&mut self);
    /// Показать пользователю информационное сообщение.
    fn notify_info(&mut self, message: String);
    /// Открыть один файл тем же путём, что кнопка Open.
    fn open_single_media_file_like_open_button(&mut self, path: PathBuf);
    /// Заменить очередь файлами (с подтверждением, если очередь не пуста) и играть первый.
    fn replace_queue_with_media_files(&mut self, paths: Vec<PathBuf>);
    /// Добавить файлы в конец очереди.
    fn append_media_files_to_queue(&mut self, paths: Vec<PathBuf>);
    /// Запустить фоновый обход набора с папками. Результат придёт позже отдельным путём:
    /// пустой результат — уведомление, иначе замена очереди (видео) или добавление (панель).
    fn start_dropped_collection_walk(
        &mut self,
        entries: Vec<DroppedCollectionEntry>,
        target: DropTarget,
    );
    /// Настройка «что делать с брошенным файлом плейлиста» из committed config.
    fn dropped_playlist_file_action(&self) -> DroppedPlaylistFileAction;
    /// Запустить импорт плейлиста по пути (дальше — обычный предпросмотр и подтверждение).
    fn import_playlist_file(&mut self, path: PathBuf, intent: PlaylistImportIntent);
    /// Ссылка на панель плейлиста: ровно как кнопка «Добавить URL» (прогресс, причины отказа,
    /// подтверждения сохранения).
    fn add_web_url_like_add_url_button(&mut self, url: DroppedWebUrl);
    /// Ссылка на видео: новая очередь из неё (с подтверждением, если очередь не пуста) и play.
    fn replace_queue_with_web_url(&mut self, url: DroppedWebUrl);
}

/// Итог обработки запроса: отличает «отклонено занятостью» от «исполнено».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalOpenDispatchOutcome {
    /// Идёт другое открытие: бросок проигнорирован, пользователь предупреждён.
    IgnoredBecauseBusy,
    /// План построен и все его шаги переданы хосту.
    Dispatched,
}

/// Классифицирует запрос, строит шаги и исполняет их через хост.
pub(crate) fn dispatch_external_open_request(
    host: &mut impl ExternalOpenHost,
    request: ExternalOpenRequest,
) -> ExternalOpenDispatchOutcome {
    // Занятость проверяется до любой работы: бросок во время открытия не должен ни
    // менять очередь, ни ставить в очередь второе открытие (решение владельца).
    if host.open_in_progress() {
        host.notify_open_still_in_progress();
        return ExternalOpenDispatchOutcome::IgnoredBecauseBusy;
    }
    for step in route_plan(classify_request(request)) {
        execute_step(host, step);
    }
    ExternalOpenDispatchOutcome::Dispatched
}

/// Исполняет один шаг через хост.
fn execute_step(host: &mut impl ExternalOpenHost, step: OpenStep) {
    match step {
        OpenStep::OpenSingleMediaFile(path) => host.open_single_media_file_like_open_button(path),
        OpenStep::ReplaceQueueWithMediaFiles(paths) => host.replace_queue_with_media_files(paths),
        OpenStep::AppendMediaFiles(paths) => host.append_media_files_to_queue(paths),
        OpenStep::CollectDroppedItems { target, entries } => {
            host.start_dropped_collection_walk(entries, target);
        }
        OpenStep::ImportPlaylistFile { path, target } => {
            let intent = playlist_import_intent(host.dropped_playlist_file_action(), target);
            host.import_playlist_file(path, intent);
        }
        OpenStep::Notify(notice) => host.notify_info(notice.text()),
        OpenStep::OpenWebUrl { url, target } => match target {
            DropTarget::Playlist => host.add_web_url_like_add_url_button(url),
            DropTarget::Video => host.replace_queue_with_web_url(url),
        },
    }
}

#[cfg(test)]
mod tests;
