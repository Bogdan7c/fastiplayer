//! Исполнение «Повторить»: цель повтора превращается в то же действие, что и исходное.
//!
//! Решение «что вызвать» (`execute_media_retry`) отделено от самих вызовов (`MediaRetryPort`):
//! правило «занято», выбор пути под цель и реакция на «повторять нечего» проверяются
//! тестами с фальшивым портом, а настоящий порт (`app_port`) только передаёт вызовы
//! существующим владельцам — кнопке Open, ссылке на видео, Play строки.
//!
//! Ни одна ветка не открывает media напрямую. Поэтому «занято» (сессия 18), подтверждение
//! замены очереди, строка прогресса и **повторный отказ** (новая ошибка в центре с той же
//! кнопкой) работают так же, как у первой попытки.

use std::path::PathBuf;

use super::{MediaRetryTarget, ReopenPosition};
use crate::external_open::request::DroppedWebUrl;
use playlist_core::PlaylistItemId;

mod app_port;

#[cfg(test)]
mod tests;

/// Повторять нечего: элемент очереди удалён или очередь уже не в том состоянии.
pub(crate) const RETRY_TARGET_GONE_MESSAGE: &str =
    "I couldn’t repeat: the element is no longer in line.";

/// Занят ли общий слот открытия файла из Open (подготовка или установка).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalOpenSlot {
    /// Слот свободен: можно начинать новое открытие.
    Free,
    /// Идёт открытие из Open: второе открытие не стартует (сессия 18, «Файл ещё открывается»).
    Busy,
}

/// Начался ли повтор через Play строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryStart {
    /// Открытие запущено существующим путём; его исход покажет обычный механизм ошибок.
    Started,
    /// Открывать нечего: элемента нет в очереди или очередь не была остановлена.
    NothingToOpen,
}

/// Что умеет приложение для повтора: по одному вызову на каждый существующий путь открытия.
pub(crate) trait MediaRetryPort {
    /// Состояние общего слота открытия из Open.
    fn local_open_slot(&self) -> LocalOpenSlot;
    /// Сообщает «Файл ещё открывается»; ошибка при этом остаётся на экране.
    fn report_still_opening(&mut self);
    /// Снимает ошибку: повтор стартовал, исход покажет новое открытие.
    fn clear_failure(&mut self);
    /// Путь кнопки Open для локального файла.
    fn open_local_file(&mut self, path: PathBuf);
    /// Путь ссылки, брошенной на видео.
    fn open_web_url(&mut self, url: &DroppedWebUrl);
    /// Play строки очереди.
    fn play_queue_item(&mut self, item_id: PlaylistItemId) -> RetryStart;
    /// Play на остановленном (B8) элементе очереди.
    fn resume_blocked_queue(&mut self) -> RetryStart;
    /// Переоткрытие оборвавшегося воспроизведения с позиции обрыва.
    fn reopen_failed_playback(&mut self, position: ReopenPosition) -> RetryStart;
    /// Сообщает, что повторять нечего.
    fn report_nothing_to_retry(&mut self);
}

/// Исполняет повтор: сначала правило «занято», затем вызов пути, который породил ошибку.
///
/// Если слот открытия занят, повтор не стартует и ошибка **остаётся** с кнопкой — иначе
/// пользователь потерял бы и ошибку, и возможность повторить позже. Если повтор стартует,
/// ошибка снимается сразу: следующее состояние покажет уже новое открытие.
pub(crate) fn execute_media_retry(port: &mut impl MediaRetryPort, target: MediaRetryTarget) {
    if port.local_open_slot() == LocalOpenSlot::Busy {
        port.report_still_opening();
        return;
    }
    port.clear_failure();
    let start = match target {
        MediaRetryTarget::LocalFile(path) => {
            port.open_local_file(path);
            RetryStart::Started
        }
        MediaRetryTarget::WebUrl(url) => {
            port.open_web_url(&url);
            RetryStart::Started
        }
        MediaRetryTarget::QueueItem(item_id) => port.play_queue_item(item_id),
        MediaRetryTarget::ResumeBlockedQueue => port.resume_blocked_queue(),
        MediaRetryTarget::ReopenFailedPlayback { position } => {
            port.reopen_failed_playback(position)
        }
    };
    if start == RetryStart::NothingToOpen {
        port.report_nothing_to_retry();
    }
}
