//! Единственный владелец ввода по свободной области видео: либо обычный режим просмотра,
//! либо правка внешнего вида субтитров.
//!
//! Два режима взаимоисключающие по построению: в каждом кадре вызывается ровно ОДИН из
//! `video_surface_input::show` и `subtitle_appearance_edit::show`. Поэтому в режиме правки
//! двойной клик не может переключить полноэкранный режим, а колесо - громкость, и
//! отдельных проверок «не в режиме ли правки» в обработчиках нет.

use egui::{Rect, Ui};

use super::player_controls::ControlAction;
use super::subtitle_appearance_edit::{
    self, SubtitleAppearanceEditAction, SubtitleAppearanceEditInput,
};
use super::video_surface_input;

/// Какой режим ввода сейчас у области видео.
pub(crate) enum VideoAreaMode<'a> {
    /// Обычный просмотр: двойной клик - полный экран, колесо - громкость.
    Playback,
    /// Правка субтитров на видео: жесты и панель.
    SubtitleAppearanceEdit(SubtitleAppearanceEditInput<'a>),
}

/// Намерения пользователя над областью видео за кадр.
#[derive(Debug, Default)]
pub(crate) struct VideoAreaOutput {
    /// Обычные действия плеера (идут в общий список `ControlAction`).
    pub(crate) control_actions: Vec<ControlAction>,
    /// Действия режима правки субтитров.
    pub(crate) subtitle_edit_actions: Vec<SubtitleAppearanceEditAction>,
}

/// Показывает область видео в выбранном режиме и возвращает намерения.
#[must_use]
pub(crate) fn show(ui: &mut Ui, video_rect: Rect, mode: VideoAreaMode<'_>) -> VideoAreaOutput {
    match mode {
        VideoAreaMode::Playback => VideoAreaOutput {
            control_actions: video_surface_input::show(ui, video_rect)
                .into_iter()
                .map(ControlAction::from)
                .collect(),
            subtitle_edit_actions: Vec::new(),
        },
        VideoAreaMode::SubtitleAppearanceEdit(input) => VideoAreaOutput {
            control_actions: Vec::new(),
            subtitle_edit_actions: subtitle_appearance_edit::show(ui, input),
        },
    }
}

#[cfg(test)]
mod tests;
