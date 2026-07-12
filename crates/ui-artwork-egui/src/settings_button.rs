use crate::ButtonVisualState;
use egui::{Color32, Painter, Rect, Stroke, vec2};
pub(crate) fn paint(p: &Painter, r: Rect, s: ButtonVisualState, stroke: Stroke, hover: Color32) {
    if s == ButtonVisualState::Hovered {
        p.rect_filled(r, 0.0, hover);
    }
    let c = r.center();
    let radius = (r.width().min(r.height()) * 0.22).max(5.0);
    p.circle_stroke(c, radius, stroke);
    p.circle_stroke(c, radius * 0.38, stroke);
    for i in 0..8 {
        let a = std::f32::consts::TAU * i as f32 / 8.0;
        let d = vec2(a.cos(), a.sin());
        p.line_segment([c + d * radius * 1.12, c + d * radius * 1.42], stroke);
    }
}
