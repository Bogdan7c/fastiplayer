//! Painter-примитивы кнопок секций sidebar.

use crate::ButtonVisualState;
use egui::{Color32, Painter, Pos2, Rect, Stroke, vec2};

/// Иконка, которую нужно нарисовать внутри titlebar-кнопки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarButtonGlyph {
    /// Три строки плейлиста.
    Playlist,
    /// Звено/цепочка URL.
    Url,
    /// Круглая информационная метка.
    Info,
}

pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    glyph: SidebarButtonGlyph,
    state: ButtonVisualState,
    stroke: Stroke,
    hover_fill: Color32,
) {
    if state == ButtonVisualState::Hovered {
        painter.rect_filled(rect, 4.0, hover_fill);
    }

    match glyph {
        SidebarButtonGlyph::Playlist => paint_playlist(painter, rect, stroke),
        SidebarButtonGlyph::Url => paint_url(painter, rect, stroke),
        SidebarButtonGlyph::Info => paint_info(painter, rect, stroke),
    }
}

pub(crate) fn paint_active_background(painter: &Painter, rect: Rect, fill: Color32) {
    painter.rect_filled(rect, 4.0, fill);
}

fn paint_playlist(painter: &Painter, rect: Rect, stroke: Stroke) {
    let center = rect.center();
    for row_offset in [-5.0, 0.0, 5.0] {
        let row_y = center.y + row_offset;
        painter.circle_filled(Pos2::new(center.x - 6.5, row_y), 1.1, stroke.color);
        painter.line_segment(
            [
                Pos2::new(center.x - 3.5, row_y),
                Pos2::new(center.x + 7.0, row_y),
            ],
            stroke,
        );
    }
}

fn paint_url(painter: &Painter, rect: Rect, stroke: Stroke) {
    let center = rect.center();
    let link_size = vec2(10.0, 6.0);
    let left = Rect::from_center_size(center + vec2(-4.0, 2.5), link_size);
    let right = Rect::from_center_size(center + vec2(4.0, -2.5), link_size);
    painter.rect_stroke(left, 3.0, stroke, egui::StrokeKind::Middle);
    painter.rect_stroke(right, 3.0, stroke, egui::StrokeKind::Middle);
}

fn paint_info(painter: &Painter, rect: Rect, stroke: Stroke) {
    let center = rect.center();
    painter.circle_stroke(center, 8.0, stroke);
    painter.circle_filled(center + vec2(0.0, -3.5), 1.2, stroke.color);
    painter.line_segment([center + vec2(0.0, -0.5), center + vec2(0.0, 5.0)], stroke);
}
