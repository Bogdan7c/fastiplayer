use egui::{Color32, Painter, Rect, Shape, Stroke, pos2};

/// Визуальное состояние интерактивного элемента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVisualState {
    Idle,
    Hovered,
}

/// Glyph центральной кнопки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackGlyph {
    Play,
    Pause,
}

/// Визуальные параметры центральной кнопки.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackStyle {
    pub diameter: f32,
    pub icon_extent: f32,
    pub stroke_width: f32,
    pub color: Color32,
    pub hover_fill: Color32,
}

pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    glyph: PlaybackGlyph,
    state: ButtonVisualState,
    style: PlaybackStyle,
) {
    let center = rect.center();
    let radius = style.diameter * 0.5 - style.stroke_width * 0.5;
    let stroke = Stroke::new(style.stroke_width, style.color);
    if state == ButtonVisualState::Hovered {
        painter.circle_filled(center, radius, style.hover_fill);
    }
    painter.circle_stroke(center, radius, stroke);
    let half = style.icon_extent * 0.5;
    match glyph {
        PlaybackGlyph::Play => {
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(center.x - half * 0.45, center.y - half),
                    pos2(center.x - half * 0.45, center.y + half),
                    pos2(center.x + half * 0.75, center.y),
                ],
                style.color,
                Stroke::NONE,
            ));
        }
        PlaybackGlyph::Pause => {
            for x in [
                center.x - (style.icon_extent * 0.16 + style.stroke_width),
                center.x + (style.icon_extent * 0.16 + style.stroke_width),
            ] {
                painter.line_segment([pos2(x, center.y - half), pos2(x, center.y + half)], stroke);
            }
        }
    }
}
