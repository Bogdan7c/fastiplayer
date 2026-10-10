//! Ввод мышью по свободной области видео (то, что осталось после панелей и сайдбара).
//!
//! Решение владельца (сессия UX 14): двойной клик переключает полноэкранный режим,
//! одиночный клик ничего не делает — без случайных пауз и без задержки на распознавание
//! двойного клика.
//!
//! Решение владельца (UX edge cases 2, B3): колесо мыши над видео меняет громкость.
//! Колесо над плейлистом/сайдбаром остаётся прокруткой, над кнопкой play — скоростью:
//! область видео не включает ни то, ни другое, а `hovered()` ложно, если курсор над
//! оверлеем выше неё.

use egui::{Rect, Sense, Ui};

mod wheel;

use super::player_controls::ControlAction;

/// Действие пользователя над областью видео.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VideoSurfaceAction {
    /// Переключить полноэкранный режим (то же действие, что кнопка в нижней панели).
    ToggleFullscreen,

    /// Сдвинуть громкость на N шагов колесом; положительное число — громче.
    AdjustVolumeSteps(i32),
}

impl From<VideoSurfaceAction> for ControlAction {
    /// Действие над видео — то же намерение, что у кнопок и клавиш controls.
    fn from(action: VideoSurfaceAction) -> Self {
        match action {
            VideoSurfaceAction::ToggleFullscreen => Self::ToggleFullscreen,
            VideoSurfaceAction::AdjustVolumeSteps(step_count) => {
                Self::AdjustVolumeSteps(step_count)
            }
        }
    }
}

/// Регистрирует область видео как интерактивную и возвращает действия пользователя за кадр.
///
/// Вызывать после всех панелей кадра с прямоугольником оставшейся области: оверлеи
/// (уведомления, подтверждения, превью таймлайна) живут на слоях выше и получают клики
/// и колесо раньше этой области.
#[must_use]
pub(crate) fn show(ui: &mut Ui, video_surface_rect: Rect) -> Vec<VideoSurfaceAction> {
    let mut actions = Vec::new();
    if !video_surface_rect.is_positive() {
        return actions;
    }
    let response = ui.interact(
        video_surface_rect,
        ui.id().with("video_surface_input"),
        Sense::click(),
    );
    if response.double_clicked() {
        actions.push(VideoSurfaceAction::ToggleFullscreen);
    }
    let volume_steps = wheel::take_volume_steps(ui.ctx(), &response);
    if volume_steps != 0 {
        actions.push(VideoSurfaceAction::AdjustVolumeSteps(volume_steps));
    }
    actions
}

#[cfg(test)]
mod tests;
