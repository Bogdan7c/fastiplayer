use crate::ButtonVisualState;
use egui::{Color32, Painter, Rect, Stroke, StrokeKind, Vec2, pos2};
/// Вариант системной кнопки окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControlGlyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}
/// Стиль системной кнопки окна.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowControlStyle {
    pub fill: Color32,
    pub stroke: Stroke,
    pub hover_fill: Color32,
}
pub(crate) fn paint(
    p: &Painter,
    r: Rect,
    g: WindowControlGlyph,
    s: ButtonVisualState,
    style: WindowControlStyle,
) {
    if s == ButtonVisualState::Hovered {
        p.rect_filled(r, 0.0, style.hover_fill);
    }
    let c = r.center();
    match g {
        WindowControlGlyph::Minimize => {
            p.line_segment(
                [pos2(c.x - 6.0, c.y + 5.0), pos2(c.x + 6.0, c.y + 5.0)],
                style.stroke,
            );
        }
        WindowControlGlyph::Maximize => {
            p.rect_stroke(
                Rect::from_center_size(c, Vec2::new(12.0, 10.0)),
                0.0,
                style.stroke,
                StrokeKind::Inside,
            );
        }
        WindowControlGlyph::Restore => {
            let back = Rect::from_center_size(pos2(c.x + 2.0, c.y - 2.0), Vec2::new(10.0, 8.0));
            let front = Rect::from_center_size(pos2(c.x - 2.0, c.y + 2.0), Vec2::new(10.0, 8.0));
            p.rect_stroke(back, 0.0, style.stroke, StrokeKind::Inside);
            p.rect_filled(front.expand(1.0), 0.0, style.fill);
            p.rect_stroke(front, 0.0, style.stroke, StrokeKind::Inside);
        }
        WindowControlGlyph::Close => {
            p.line_segment(
                [pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)],
                style.stroke,
            );
            p.line_segment(
                [pos2(c.x + 5.0, c.y - 5.0), pos2(c.x - 5.0, c.y + 5.0)],
                style.stroke,
            );
        }
    }
}
