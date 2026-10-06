//! Индикатор «получаем данные по ссылке» с кнопкой «Отменить» (UX сессия 09).
//!
//! Только отрисовка: факт и домен приходят из read-only
//! `PlaylistInteractionModel::url_import_progress`, нажатие публикует typed
//! `PlaylistAction::CancelUrlImport`. Решение об отмене и её последствиях принимает
//! `PlaylistRuntime`.
//!
//! Строка статична, без `egui::Spinner`: тот просит перерисовку каждый кадр, пока виден,
//! а yt-dlp может работать до таймаута (по умолчанию 30 с) — окно крутилось бы
//! вхолостую. Результат и так будит окно через owner mailbox.

use crate::playlist_runtime::PlaylistUrlImportProgress;

use super::super::PlaylistUiOutput;
use super::super::actions::PlaylistAction;

/// Подпись кнопки отмены (она же — имя для экранного диктора).
const CANCEL_BUTTON_LABEL: &str = "Отменить";

/// Подсказка: что именно отменяется и что не пострадает.
const CANCEL_BUTTON_TOOLTIP: &str = "Остановить получение данных по ссылке; очередь не изменится";

/// Рисует строку индикатора под toolbar.
pub(super) fn show(
    ui: &mut egui::Ui,
    progress: &PlaylistUrlImportProgress,
    output: &mut PlaylistUiOutput,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(progress_text(progress)).weak());
        let cancel_response = ui
            .button(CANCEL_BUTTON_LABEL)
            .on_hover_text(CANCEL_BUTTON_TOOLTIP);
        if cancel_response.clicked() {
            output.push_action(PlaylistAction::CancelUrlImport);
        }
    });
}

/// «Получаем данные по ссылке (youtube.com)…» — только домен, без пути и query.
fn progress_text(progress: &PlaylistUrlImportProgress) -> String {
    match progress.display_host.as_deref() {
        Some(host) => format!("Получаем данные по ссылке ({host})…"),
        None => "Получаем данные по ссылке…".to_owned(),
    }
}

#[cfg(test)]
#[path = "url_import_progress/tests.rs"]
mod tests;
