use crate::ButtonVisualState;
use egui::{Color32, Painter, Rect, Stroke, Vec2, pos2};
/// Направление fullscreen glyph-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenGlyph {
    Enter,
    Exit,
}
/// Стиль fullscreen-кнопки.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FullscreenStyle {
    pub icon_extent: f32,
    pub stroke: Stroke,
    pub hover_fill: Color32,
}
pub(crate) fn paint(
    p: &Painter,
    r: Rect,
    g: FullscreenGlyph,
    s: ButtonVisualState,
    style: FullscreenStyle,
) {
    if s == ButtonVisualState::Hovered {
        p.rect_filled(r, 0.0, style.hover_fill);
    }
    let ir = Rect::from_center_size(r.center(), Vec2::splat(style.icon_extent));
    let l = style.icon_extent * 0.38;
    let (a, b, c, d) = (ir.left(), ir.right(), ir.top(), ir.bottom());
    let triples = match g {
        FullscreenGlyph::Enter => [
            (pos2(a, c), pos2(a + l, c), pos2(a, c + l)),
            (pos2(b, c), pos2(b - l, c), pos2(b, c + l)),
            (pos2(a, d), pos2(a + l, d), pos2(a, d - l)),
            (pos2(b, d), pos2(b - l, d), pos2(b, d - l)),
        ],
        FullscreenGlyph::Exit => [
            (pos2(a + l, c + l), pos2(a, c + l), pos2(a + l, c)),
            (pos2(b - l, c + l), pos2(b, c + l), pos2(b - l, c)),
            (pos2(a + l, d - l), pos2(a, d - l), pos2(a + l, d)),
            (pos2(b - l, d - l), pos2(b, d - l), pos2(b - l, d)),
        ],
    };
    for (corner, h, v) in triples {
        p.line_segment([corner, h], style.stroke);
        p.line_segment([corner, v], style.stroke);
    }
}
