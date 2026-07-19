//! Нейтральная отрисовка background/strokes и physical-pixel separator строки.

use egui::layers::ShapeIdx;
use egui::{Color32, Painter, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

/// Нейтральные визуальные параметры вертикального маркера внутри строки.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaylistRowMarkerStyle {
    /// Ширина маркера в logical points.
    pub width: f32,
    /// Симметричный отступ от верхнего и нижнего краёв строки.
    pub vertical_inset: f32,
    /// Радиус скругления, ограничиваемый фактической геометрией маркера.
    pub corner_radius: f32,
    /// Цвет заливки маркера.
    pub fill: Color32,
}

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

/// Рисует нейтральный вертикальный маркер внутри левого края строки.
pub(crate) fn paint_marker(painter: &Painter, row_rect: Rect, style: PlaylistRowMarkerStyle) {
    // Невалидная или вырожденная геометрия не должна создавать мусорную shape.
    let Some(marker_rect) = marker_rect(row_rect, style) else {
        return;
    };
    // Полностью прозрачный маркер не занимает paint list.
    if style.fill == Color32::TRANSPARENT {
        return;
    }
    // Скругление не может превышать половину меньшей стороны готового маркера.
    let corner_radius = style
        .corner_radius
        .max(0.0)
        .min(marker_rect.width() * 0.5)
        .min(marker_rect.height() * 0.5);
    // Painter наследует ScrollArea clip rect и не влияет на layout или interaction.
    painter.rect_filled(marker_rect, corner_radius, style.fill);
}

/// Возвращает безопасную геометрию маркера, полностью ограниченную строкой.
pub(crate) fn marker_rect(row_rect: Rect, style: PlaylistRowMarkerStyle) -> Option<Rect> {
    // NaN/Infinity нельзя передавать в Painter geometry.
    if !row_rect.is_positive()
        || !style.width.is_finite()
        || !style.vertical_inset.is_finite()
        || !style.corner_radius.is_finite()
    {
        return None;
    }
    // Ширина не выходит за пределы даже очень узкой строки.
    let marker_width = style.width.clamp(0.0, row_rect.width());
    // Вертикальный отступ не может перевернуть итоговый прямоугольник.
    let vertical_inset = style.vertical_inset.clamp(0.0, row_rect.height() * 0.5);
    // Высота симметрично вычитается из полного row rect.
    let marker_height = row_rect.height() - vertical_inset * 2.0;
    // Нулевая ширина или высота означает отсутствие видимой геометрии.
    if marker_width <= f32::EPSILON || marker_height <= f32::EPSILON {
        return None;
    }
    // Маркер начинается от левого края и не меняет layout содержимого строки.
    Some(Rect::from_min_size(
        pos2(row_rect.left(), row_rect.top() + vertical_inset),
        vec2(marker_width, marker_height),
    ))
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
