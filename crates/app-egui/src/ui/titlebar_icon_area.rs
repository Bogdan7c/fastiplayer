//! Левая icon-area кастомного titlebar.
//!
//! Модуль остаётся visual-only boundary: он владеет геометрией кнопки,
//! hover-отрисовкой и glyph-ом, а наружу отдаёт только нейтральный intent.

use egui::{Color32, Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType, vec2};

/// Stable id кнопки, чтобы egui не смешивал её состояние с другими controls.
const SETTINGS_BUTTON_ID: &str = "titlebar_icon_area_settings";

/// Количество зубцов у hand-drawn gear icon.
const GEAR_TOOTH_COUNT: usize = 8;

/// Действие, которое левая icon-area отдаёт владельцу runtime semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TitlebarIconAreaAction {
    /// Переключить settings sidebar: closed -> open, open -> cancel/close.
    ToggleSettingsSidebar,
}

/// Визуальный стиль icon-area, полученный от текущего titlebar skin-а.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TitlebarIconAreaStyle {
    /// Stroke иконки, совпадает с системными titlebar icon-ами.
    pub(crate) icon_stroke: Stroke,

    /// Hover-заливка, совпадает с обычными кнопками titlebar.
    pub(crate) button_hover_fill: Color32,
}

/// Результат отрисовки icon-area за один egui frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TitlebarIconAreaOutput {
    /// Rect, который должен быть исключён из drag/resize hit-zones.
    pub(crate) reserved_rect: Rect,

    /// Нейтральные действия, собранные с кнопок icon-area.
    pub(crate) actions: Vec<TitlebarIconAreaAction>,
}

/// Рисует левую settings-кнопку и возвращает занятый rect вместе с actions.
#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    titlebar_rect: Rect,
    style: TitlebarIconAreaStyle,
) -> TitlebarIconAreaOutput {
    let button_rect = settings_button_rect(titlebar_rect);
    let response = ui
        .interact(
            button_rect,
            ui.id().with(SETTINGS_BUTTON_ID),
            Sense::click(),
        )
        .on_hover_text("Настройки");

    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), "Настройки"));
    paint_settings_button(ui, button_rect, style, response.hovered());

    TitlebarIconAreaOutput {
        reserved_rect: button_rect,
        actions: actions_for_settings_button(response.clicked()),
    }
}

/// Возвращает квадратную settings-кнопку, привязанную к левому краю titlebar.
#[must_use]
pub(crate) fn settings_button_rect(titlebar_rect: Rect) -> Rect {
    Rect::from_min_size(
        titlebar_rect.min,
        vec2(titlebar_rect.height(), titlebar_rect.height()),
    )
}

/// Pure mapping для тестов: click на gear остаётся toggle intent-ом.
#[must_use]
pub(crate) fn actions_for_settings_button(clicked: bool) -> Vec<TitlebarIconAreaAction> {
    clicked
        .then_some(TitlebarIconAreaAction::ToggleSettingsSidebar)
        .into_iter()
        .collect()
}

/// Рисует hover background и hand-drawn gear icon без новой icon dependency.
fn paint_settings_button(ui: &Ui, button_rect: Rect, style: TitlebarIconAreaStyle, hovered: bool) {
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(button_rect, 0.0, style.button_hover_fill);
    }

    paint_gear_icon(painter, button_rect, style.icon_stroke);
}

/// Рисует gear из окружностей и коротких radial strokes.
fn paint_gear_icon(painter: &egui::Painter, button_rect: Rect, stroke: Stroke) {
    let center = button_rect.center();
    let icon_radius = (button_rect.width().min(button_rect.height()) * 0.22).max(5.0);
    let tooth_inner_radius = icon_radius * 1.12;
    let tooth_outer_radius = icon_radius * 1.42;

    painter.circle_stroke(center, icon_radius, stroke);
    painter.circle_stroke(center, icon_radius * 0.38, stroke);

    for tooth_index in 0..GEAR_TOOTH_COUNT {
        let angle = std::f32::consts::TAU * tooth_index as f32 / GEAR_TOOTH_COUNT as f32;
        let direction = vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [
                center + direction * tooth_inner_radius,
                center + direction * tooth_outer_radius,
            ],
            stroke,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Pos2;

    #[test]
    fn button_rect_is_square_and_starts_at_left_edge() {
        let titlebar_rect = Rect::from_min_size(Pos2::new(12.0, 4.0), vec2(900.0, 40.0));
        let button_rect = settings_button_rect(titlebar_rect);

        assert_eq!(button_rect.left(), titlebar_rect.left());
        assert_eq!(button_rect.top(), titlebar_rect.top());
        assert_eq!(button_rect.width(), titlebar_rect.height());
        assert_eq!(button_rect.height(), titlebar_rect.height());
    }

    #[test]
    fn click_maps_to_toggle_settings_sidebar() {
        assert_eq!(
            actions_for_settings_button(true),
            vec![TitlebarIconAreaAction::ToggleSettingsSidebar]
        );
    }

    #[test]
    fn no_click_emits_no_action() {
        assert!(actions_for_settings_button(false).is_empty());
    }
}
