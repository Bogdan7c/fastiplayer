use egui::{Color32, Painter, Rect};
pub(crate) fn paint(p: &Painter, r: Rect, c: Color32) {
    p.rect_filled(r, 0.5, c);
}
