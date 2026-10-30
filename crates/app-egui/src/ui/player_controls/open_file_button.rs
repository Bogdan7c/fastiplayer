//! Кнопка Open file нижней панели: открыть своё медиа.
//!
//! Есть не в каждом виде окна: окно браузерного режима её не рисует
//! (`WindowViewProfile::offers_media_opening`). Место кнопки (`open_file_button_anchor_rect`)
//! остаётся осью раскладки строки в любом виде: от него считаются зона громкости и
//! соседи, и раскладка обычного окна не меняется.

use egui::{Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType};
use ui_artwork_egui::ArtworkPainter;

use super::button_visual_state;
use crate::ui::skin::PlayerSkin;

/// Рисует open-file кнопку в заранее рассчитанном anchored rect.
pub(super) fn render_open_file_button_at(
    ui: &mut Ui,
    button_rect: Rect,
    skin: &impl PlayerSkin,
) -> egui::Response {
    let accessible_label = "Open file";
    let button_response = ui.allocate_rect(button_rect, Sense::click());
    let button_response = button_response.on_hover_text(accessible_label);

    button_response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), accessible_label));

    paint_open_file_button(ui, button_rect, skin, &button_response);

    button_response
}

fn paint_open_file_button(
    ui: &Ui,
    button_rect: Rect,
    skin: &impl PlayerSkin,
    button_response: &egui::Response,
) {
    let style = skin.controls_style();
    ArtworkPainter::new(ui.painter()).open_media_button(
        button_rect,
        button_visual_state(button_response.hovered()),
        Stroke::new(style.playback_button_stroke_width, style.text_color),
        style.playback_button_hover_fill,
    );
}
