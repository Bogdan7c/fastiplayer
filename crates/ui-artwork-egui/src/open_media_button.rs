use crate::ButtonVisualState;
use egui::{Color32, Painter, Rect, Shape, Stroke, pos2, vec2};

pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    state: ButtonVisualState,
    stroke: Stroke,
    hover_fill: Color32,
) {
    if state == ButtonVisualState::Hovered {
        painter.rect_filled(rect, 0.0, hover_fill);
    }
    let side = rect.width().min(rect.height()) * 0.64;
    let icon = Rect::from_center_size(rect.center(), vec2(side * 1.12, side));
    let file = Rect::from_min_size(icon.left_top(), vec2(icon.width() * 0.58, icon.height()));
    let fold = file.width() * 0.24;
    let before = pos2(file.right() - fold, file.top());
    let corner = pos2(file.right(), file.top() + fold);
    for points in [
        [file.left_top(), before],
        [before, corner],
        [corner, file.right_bottom()],
        [file.right_bottom(), file.left_bottom()],
        [file.left_bottom(), file.left_top()],
    ] {
        painter.line_segment(points, stroke);
    }
    let center = pos2(
        file.center().x - file.width() * 0.03,
        file.center().y + file.height() * 0.04,
    );
    let hh = file.height() * 0.19;
    let hw = file.width() * 0.17;
    painter.add(Shape::convex_polygon(
        vec![
            pos2(center.x - hw, center.y - hh),
            pos2(center.x - hw, center.y + hh),
            pos2(center.x + hw, center.y),
        ],
        stroke.color,
        Stroke::NONE,
    ));
    let dot = file.right() + icon.width() * 0.12;
    let start = dot + icon.width() * 0.08;
    for y in [
        icon.center().y - icon.height() * 0.24,
        icon.center().y,
        icon.center().y + icon.height() * 0.24,
    ] {
        painter.circle_filled(pos2(dot, y), stroke.width * 0.55, stroke.color);
        painter.line_segment([pos2(start, y), pos2(icon.right(), y)], stroke);
    }
}
