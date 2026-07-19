//! Нейтральная отрисовка background/strokes и physical-pixel separator строки.

use egui::layers::ShapeIdx;
use egui::{Color32, Painter, Rect, Shape, Stroke, StrokeKind};

/// Резервирует background shape до текста, чтобы interaction можно было вычислить после layout.
pub(crate) fn reserve_background(painter: &Painter) -> ShapeIdx {
    painter.add(Shape::Noop)
}

/// Заполняет ранее зарезервированный background slot, не перекрывая row content.
pub(crate) fn paint_background(
    painter: &Painter,
    shape_index: ShapeIdx,
    rect: Rect,
    fill: Color32,
    stroke: Stroke,
) {
    painter.set(
        shape_index,
        Shape::Vec(vec![
            Shape::rect_filled(rect, 0.0, fill),
            Shape::rect_stroke(rect, 0.0, stroke, StrokeKind::Inside),
        ]),
    );
}

/// Рисует отдельный overlay-контур после content без дублирования fill.
pub(crate) fn paint_outline(painter: &Painter, rect: Rect, stroke: Stroke) {
    // Пустой stroke не добавляет декоративную shape и не засоряет paint list.
    if stroke == Stroke::NONE {
        return;
    }
    // Внутренний stroke не выходит за row rect и предсказуемо клипуется viewport-ом.
    painter.rect_stroke(rect, 0.0, stroke, StrokeKind::Inside);
}

/// Рисует separator поверх fill/content ровно одним physical pixel.
pub(crate) fn paint_separator(
    painter: &Painter,
    rect: Rect,
    color: Color32,
    pixels_per_point: f32,
) {
    let Some((separator_y, stroke_width)) = separator_geometry(rect, pixels_per_point) else {
        return;
    };
    painter.hline(
        rect.x_range(),
        separator_y,
        Stroke::new(stroke_width, color),
    );
}

/// Центрирует stroke внутри последнего physical pixel строки.
pub(crate) fn separator_geometry(rect: Rect, pixels_per_point: f32) -> Option<(f32, f32)> {
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 || !rect.is_positive() {
        return None;
    }
    let stroke_width = 1.0 / pixels_per_point;
    let bottom_physical_pixel = (rect.bottom() * pixels_per_point).round();
    let separator_y = (bottom_physical_pixel - 0.5) / pixels_per_point;
    Some((separator_y, stroke_width))
}
