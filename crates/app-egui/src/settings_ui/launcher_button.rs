//! Floating launcher button для visual settings UI.

use egui::{Align2, Area, Context, Id};

use super::SettingsUiAction;

/// Stable egui id для bottom-right launcher-а.
const LAUNCHER_AREA_ID: &str = "rustiplayer_settings_launcher";

/// Отступ launcher-а от правого нижнего угла viewport-а.
const LAUNCHER_OFFSET: egui::Vec2 = egui::Vec2::new(-16.0, -16.0);

/// Рисует кнопку настроек в правом нижнем углу: открывает панель и закрывает её.
pub fn show(ctx: &Context, actions: &mut Vec<SettingsUiAction>) {
    Area::new(Id::new(LAUNCHER_AREA_ID))
        .anchor(Align2::RIGHT_BOTTOM, LAUNCHER_OFFSET)
        .show(ctx, |ui| {
            if let Some(action) = launcher_action_for_button(ui.button("Настройки").clicked())
            {
                actions.push(action);
            }
        });
}

/// Pure mapping для тестов: click на launcher — это toggle intent.
/// Открыто или закрыто сейчас — знает и решает только runtime owner.
#[must_use]
pub(crate) fn launcher_action_for_button(clicked: bool) -> Option<SettingsUiAction> {
    clicked.then_some(SettingsUiAction::ToggleOpen)
}
