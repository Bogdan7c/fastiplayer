use crate::ButtonVisualState;
use egui::{Color32, Painter, Rect, Shape, Stroke, pos2};
/// Состояние glyph-а громкости.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeGlyph {
    Audible,
    Muted,
}
pub(crate) fn paint(
    p: &Painter,
    r: Rect,
    g: VolumeGlyph,
    s: ButtonVisualState,
    stroke: Stroke,
    hover: Color32,
) {
    if s == ButtonVisualState::Hovered {
        p.rect_filled(r, 0.0, hover);
    }
    let side = r.width().min(r.height()) * 0.68;
    let ir = Rect::from_center_size(r.center(), egui::Vec2::splat(side));
    let c = ir.center();
    p.add(Shape::line(
        vec![
            pos2(ir.left(), c.y - side * 0.14),
            pos2(c.x - side * 0.18, c.y - side * 0.14),
            pos2(c.x + side * 0.06, ir.top()),
            pos2(c.x + side * 0.06, ir.bottom()),
            pos2(c.x - side * 0.18, c.y + side * 0.14),
            pos2(ir.left(), c.y + side * 0.14),
            pos2(ir.left(), c.y - side * 0.14),
        ],
        stroke,
    ));
    match g {
        VolumeGlyph::Muted => {
            p.line_segment(
                [
                    pos2(ir.right(), ir.top() + side * 0.08),
                    pos2(ir.left() + side * 0.08, ir.bottom()),
                ],
                stroke,
            );
        }
        VolumeGlyph::Audible => {
            for radius in [side * 0.25, side * 0.39] {
                let origin = pos2(c.x + side * 0.05, c.y);
                let points = (0..=8)
                    .map(|i| {
                        let angle = -0.72 + i as f32 / 8.0 * 1.44;
                        pos2(
                            origin.x + radius * angle.cos(),
                            origin.y + radius * angle.sin(),
                        )
                    })
                    .collect();
                p.add(Shape::line(points, stroke));
            }
        }
    }
}
