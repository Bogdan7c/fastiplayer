use crate::ButtonVisualState;
use egui::{Color32, Painter, Rect, Stroke, pos2};
/// Высота видимой дорожки громкости.
pub const TRACK_HEIGHT: f32 = 3.0;
/// Базовый радиус бегунка громкости.
pub const THUMB_RADIUS: f32 = 5.0;
pub(crate) fn paint(
    p: &Painter,
    r: Rect,
    v: f32,
    s: ButtonVisualState,
    color: Color32,
    stroke_width: f32,
) {
    let v = v.clamp(0.0, 1.0);
    let x = r.left() + r.width() * v;
    let active = Rect::from_min_max(r.left_top(), pos2(x.max(r.left()), r.bottom()));
    p.rect_filled(
        r,
        TRACK_HEIGHT * 0.5,
        Color32::from_rgba_unmultiplied(255, 255, 255, 52),
    );
    if active.width() > f32::EPSILON {
        p.rect_filled(active, TRACK_HEIGHT * 0.5, color.gamma_multiply(0.85));
    }
    let radius = THUMB_RADIUS
        + if s == ButtonVisualState::Hovered {
            1.0
        } else {
            0.0
        };
    p.circle_filled(pos2(x, r.center().y), radius, color);
    p.circle_stroke(
        pos2(x, r.center().y),
        radius,
        Stroke::new(stroke_width, Color32::from_rgba_unmultiplied(0, 0, 0, 130)),
    );
}
