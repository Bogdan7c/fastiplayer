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

/// Egui temp-memory ключ последней пользовательской ширины открытого sidebar-а.
/// Нужен анимации, чтобы выезжать к той же ширине, к которой панель открыта.
fn sidebar_open_width_memory_id() -> egui::Id {
    egui::Id::new("app_sidebar_settings_open_width")
}

/// Непрозрачная заливка: видео не рисуется под панелью (viewport сжат),
/// полупрозрачность давала бы только лишний blending по всей площади панели.
const SIDEBAR_FILL: egui::Color32 = egui::Color32::from_rgb(18, 18, 18);

/// Рисует левый app sidebar и возвращает его фактическую область, если он виден.
///
/// `slide_progress` — сглаженный прогресс анимации выезда `0.0..=1.0`:
/// `0.0` — панель скрыта, `1.0` — полностью открыта, промежуточные значения —
/// панель в движении (открывается или закрывается).
#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    content: AppSidebarContent<'_>,
    slide_progress: f32,
    actions: &mut Vec<SettingsUiAction>,
) -> Option<egui::Rect> {
    match content {
        AppSidebarContent::Settings { model } => {
            show_settings_sidebar(ui, model, slide_progress, actions)
        }
    }
}

/// Рисует settings sidebar, пока он визуально виден (открыт или анимируется).
#[must_use]
fn show_settings_sidebar(
    ui: &mut Ui,
    model: &SettingsUiModel,
    slide_progress: f32,
    actions: &mut Vec<SettingsUiAction>,
) -> Option<egui::Rect> {
    if slide_progress <= 0.0 {
        return None;
    }

    if slide_progress >= 1.0 {
        let sidebar_response = egui::Panel::left("app_sidebar_settings")
            .resizable(true)
            .default_size(DEFAULT_SIDEBAR_WIDTH)
            .size_range(MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH)
            .frame(egui::Frame::NONE.fill(SIDEBAR_FILL))
            .show_inside(ui, |ui| {
                render_settings_header(ui, actions);
                ui.separator();
                layout::show(ui, model, actions);
            });

        let sidebar_rect = sidebar_response.response.rect;
        // Запоминаем пользовательскую ширину: к ней панель будет выезжать/уезжать.
        ui.ctx().data_mut(|data| {
            data.insert_temp(sidebar_open_width_memory_id(), sidebar_rect.width());
        });
        return Some(sidebar_rect);
    }

    show_animating_settings_sidebar(ui, model, slide_progress, actions)
}

/// Рисует панель в промежуточном состоянии анимации выезда/заезда.
///
/// Контент НЕ переверстывается под промежуточную ширину: он раскладывается на
/// полную целевую ширину и прижимается к правому краю панели, поэтому левая
/// часть уходит за край окна — эффект выезда слева без re-wrap текста.
#[must_use]
fn show_animating_settings_sidebar(
    ui: &mut Ui,
    model: &SettingsUiModel,
    slide_progress: f32,
    actions: &mut Vec<SettingsUiAction>,
) -> Option<egui::Rect> {
    let target_width = ui
        .ctx()
        .data(|data| data.get_temp::<f32>(sidebar_open_width_memory_id()))
        .unwrap_or(DEFAULT_SIDEBAR_WIDTH)
        .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
    // Минимум 1pt: панель нулевой ширины egui рисовать не должен.
    let animated_width = (slide_progress * target_width).max(1.0);

    let sidebar_response = egui::Panel::left("app_sidebar_settings_animating")
        .resizable(false)
        .exact_size(animated_width)
        .frame(egui::Frame::NONE.fill(SIDEBAR_FILL))
        .show_inside(ui, |ui| {
            let panel_rect = ui.max_rect();
            // Контент фиксированной целевой ширины, прижатый к правому краю панели.
            let content_rect = egui::Rect::from_min_max(
                egui::pos2(panel_rect.right() - target_width, panel_rect.top()),
                panel_rect.max,
            );
            let mut content_ui = ui.new_child(egui::UiBuilder::new().max_rect(content_rect));
            // Клип по панели: выехавшая часть контента не рисуется за её пределами.
            content_ui.set_clip_rect(panel_rect.intersect(ui.clip_rect()));
            render_settings_header(&mut content_ui, actions);
            content_ui.separator();
            layout::show(&mut content_ui, model, actions);
        });

    Some(sidebar_response.response.rect)
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
