use egui::{Align2, Color32, FontId, Painter, Rect};
pub(crate) fn paint(p: &Painter, r: Rect, title: &str, color: Color32) {
    p.with_clip_rect(r).text(
        r.center(),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(15.0),
        color,
    );
}
