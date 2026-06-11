//! Resizable settings window shell.

use egui::{Context, Id, Window};

use super::{SettingsUiAction, SettingsUiModel, SettingsUiState, layout};

/// Stable egui id окна настроек.
const SETTINGS_WINDOW_ID: &str = "rustiplayer_settings_window";

/// Default размер окна: достаточно широкий для списка sections и редакторов.
const DEFAULT_WINDOW_SIZE: egui::Vec2 = egui::Vec2::new(760.0, 560.0);

/// Рисует resizable settings window и возвращает только visual actions.
pub fn show(
    ctx: &Context,
    model: &SettingsUiModel,
    state: &mut SettingsUiState,
    actions: &mut Vec<SettingsUiAction>,
) {
    if !model.is_open {
        return;
    }

    let mut open_after_egui = true;

    Window::new("Настройки")
        .id(Id::new(SETTINGS_WINDOW_ID))
        .open(&mut open_after_egui)
        .resizable(true)
        .collapsible(false)
        .default_size(DEFAULT_WINDOW_SIZE)
        .show(ctx, |ui| {
            layout::show(ui, model, state, actions);
        });

    if let Some(action) = close_action_from_open_flag(model.is_open, open_after_egui) {
        actions.push(action);
    }
}

/// Превращает egui close flag в cancel action, не меняя runtime state внутри UI.
#[must_use]
pub(crate) fn close_action_from_open_flag(
    was_open_before_frame: bool,
    is_open_after_frame: bool,
) -> Option<SettingsUiAction> {
    (was_open_before_frame && !is_open_after_frame).then_some(SettingsUiAction::Cancel)
}
