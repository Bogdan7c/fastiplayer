//! App-level sidebar host без доступа к runtime/config/render backend-ам.

use egui::{Button, RichText, Ui};

use crate::settings_ui::{SettingsUiAction, SettingsUiModel, layout};

/// Контент app sidebar-а. V1 содержит только настройки, но enum оставляет
/// понятную точку расширения для будущих panels без изменения layout owner-а.
pub(crate) enum AppSidebarContent<'model> {
    /// Visual settings UI поверх уже собранной runtime-owned модели.
    Settings {
        /// Read-only visual model; open-state остаётся у settings runtime owner-а.
        model: &'model SettingsUiModel,
    },
}

/// Ширина sidebar-а по умолчанию в egui points.
const DEFAULT_SIDEBAR_WIDTH: f32 = 420.0;

/// Минимальная ширина оставляет поля настроек читаемыми.
const MIN_SIDEBAR_WIDTH: f32 = 320.0;

/// Максимальная ширина не даёт sidebar-у съесть весь video viewport на обычных окнах.
const MAX_SIDEBAR_WIDTH: f32 = 560.0;

/// Рисует левый app sidebar и возвращает только visual actions.
pub(crate) fn show(
    ui: &mut Ui,
    content: AppSidebarContent<'_>,
    actions: &mut Vec<SettingsUiAction>,
) {
    match content {
        AppSidebarContent::Settings { model } => show_settings_sidebar(ui, model, actions),
    }
}

/// Рисует settings sidebar только когда settings runtime считает его открытым.
fn show_settings_sidebar(
    ui: &mut Ui,
    model: &SettingsUiModel,
    actions: &mut Vec<SettingsUiAction>,
) {
    if !model.is_open {
        return;
    }

    egui::Panel::left("app_sidebar_settings")
        .resizable(true)
        .default_size(DEFAULT_SIDEBAR_WIDTH)
        .size_range(MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH)
        .frame(egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(18, 18, 18, 230)))
        .show_inside(ui, |ui| {
            render_settings_header(ui, actions);
            ui.separator();
            layout::show(ui, model, actions);
        });
}

/// Рисует заголовок sidebar-а и мапит close button в settings cancel action.
fn render_settings_header(ui: &mut Ui, actions: &mut Vec<SettingsUiAction>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Настройки").heading());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(Button::new(RichText::new("×").strong()))
                .on_hover_text("Закрыть настройки")
                .clicked()
            {
                actions.push(settings_sidebar_close_action());
            }
        });
    });
}

/// Close sidebar означает тот же rollback/cancel intent, что и закрытие старого окна.
#[must_use]
fn settings_sidebar_close_action() -> SettingsUiAction {
    SettingsUiAction::Cancel
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn settings_sidebar_close_maps_to_cancel() {
        assert_eq!(settings_sidebar_close_action(), SettingsUiAction::Cancel);
    }

    #[test]
    fn source_guardrail_sidebar_stays_visual_only() {
        let sidebar_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/sidebar.rs");
        let source = std::fs::read_to_string(sidebar_path).expect("sidebar source is readable");
        let normalized_source = source.to_lowercase();
        let forbidden_patterns = [
            concat!("settings_", "runtime").to_string(),
            concat!("rustiplayer_", "config").to_string(),
            format!("{}{}", "app", "config"),
            concat!("render_", "wg", "pu").to_string(),
            concat!("render-", "wg", "pu").to_string(),
            concat!("wg", "pu").to_string(),
        ];

        for forbidden_pattern in forbidden_patterns {
            assert!(
                !normalized_source.contains(&forbidden_pattern),
                "ui/sidebar.rs must not reference `{forbidden_pattern}`"
            );
        }
    }
}
