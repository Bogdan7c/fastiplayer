//! Ввод мышью по свободной области видео (то, что осталось после панелей и сайдбара).
//!
//! Решение владельца (сессия UX 14): двойной клик переключает полноэкранный режим,
//! одиночный клик ничего не делает — без случайных пауз и без задержки на распознавание
//! двойного клика.

use egui::{Rect, Sense, Ui};

/// Действие пользователя над областью видео.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VideoSurfaceAction {
    /// Переключить полноэкранный режим (то же действие, что кнопка в нижней панели).
    ToggleFullscreen,
}

/// Регистрирует область видео как кликабельную и возвращает действие пользователя.
///
/// Вызывать после всех панелей кадра с прямоугольником оставшейся области: оверлеи
/// (уведомления, подтверждения, превью таймлайна) живут на слоях выше и получают клики
/// раньше этой области.
#[must_use]
pub(crate) fn show(ui: &mut Ui, video_surface_rect: Rect) -> Option<VideoSurfaceAction> {
    if !video_surface_rect.is_positive() {
        return None;
    }
    let response = ui.interact(
        video_surface_rect,
        ui.id().with("video_surface_input"),
        Sense::click(),
    );
    response
        .double_clicked()
        .then_some(VideoSurfaceAction::ToggleFullscreen)
}

#[cfg(test)]
mod tests;
