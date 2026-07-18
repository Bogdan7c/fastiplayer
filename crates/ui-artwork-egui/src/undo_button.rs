//! Нейтральная векторная отрисовка анимированной кнопки Undo.
//!
//! Модуль знает только готовую hit-area, визуальное состояние и геометрию.
//! Interaction, accessibility, countdown и playlist intent остаются в `app-egui`.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

/// Количество прямых сегментов, аппроксимирующих плавную Undo-дугу.
const ARC_SEGMENT_COUNT: usize = 12;
/// Горизонтальное смещение центра дуги в нормализованных координатах glyph.
const ARC_CENTER_X: f32 = 0.05;
/// Вертикальное смещение центра дуги в нормализованных координатах glyph.
const ARC_CENTER_Y: f32 = 0.07;
/// Радиус дуги в нормализованных координатах glyph.
const ARC_RADIUS: f32 = 0.36;
/// Начальный угол оставляет дугу открытой снизу справа.
const ARC_START_RADIANS: f32 = -25.0_f32.to_radians();
/// Конечный угол направляет стрелку против часовой стрелки.
const ARC_END_RADIANS: f32 = 160.0_f32.to_radians();
/// Усиленная длина открытого наконечника относительно design extent.
const ARROWHEAD_LENGTH: f32 = 0.24;
/// Усиленная половина ширины основания наконечника.
const ARROWHEAD_HALF_WIDTH: f32 = 0.13;

/// Уже разрешённое вызывающей стороной визуальное состояние Undo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UndoButtonPaintState {
    /// Итоговый цвет glyph с учётом hover и enabled-state.
    pub foreground: Color32,
    /// Hover/pressed surface; прозрачный цвет не создаёт постоянную подложку.
    pub surface_fill: Color32,
    /// Нужен ли видимый keyboard-focus outline.
    pub focus_visible: bool,
    /// Множитель прозрачности всей кнопки в диапазоне `0.0..=1.0`.
    pub opacity: f32,
    /// Масштаб glyph вокруг неизменного центра hit-area.
    pub content_scale: f32,
}

/// Нейтральная геометрия Undo внутри готовой app-owned hit-area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UndoButtonStyle {
    /// Итоговая видимая высота glyph вместе со stroke.
    pub glyph_height: f32,
    /// Толщина дуги и наконечника.
    pub glyph_stroke_width: f32,
    /// Радиус hover/pressed surface.
    pub surface_corner_radius: f32,
    /// Контур keyboard focus.
    pub focus_outline: Stroke,
    /// Отступ focus outline внутрь hit-area.
    pub focus_inset: f32,
}

/// Опорные точки открытой Undo-стрелки после масштабирования.
struct UndoGeometry {
    /// Точки дуги, включая оба конца.
    arc_points: Vec<Pos2>,
    /// Начало дуги для круглого открытого конца.
    arc_start: Pos2,
    /// Остриё наконечника и конец дуги.
    arrow_tip: Pos2,
    /// Первая сторона основания наконечника.
    arrow_base_first: Pos2,
    /// Вторая сторона основания наконечника.
    arrow_base_second: Pos2,
}

/// Рисует surface, focus и Undo glyph внутри готовой hit-area.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    state: UndoButtonPaintState,
    style: UndoButtonStyle,
) {
    // Artwork повторно нормализует animation boundary на случай NaN/overshoot.
    let opacity = normalized_unit_interval(state.opacity);
    // Отрицательный либо некорректный scale не отражает и не раздувает glyph.
    let content_scale = normalized_unit_interval(state.content_scale);
    // Все resolved цвета получают общую opacity, чтобы fade оставался согласованным.
    let foreground = state.foreground.gamma_multiply(opacity);
    let surface_fill = state.surface_fill.gamma_multiply(opacity);
    let focus_outline = Stroke::new(
        style.focus_outline.width.max(0.0),
        style.focus_outline.color.gamma_multiply(opacity),
    );

    // В покое surface прозрачна; shape появляется только для hover/press.
    if surface_fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, style.surface_corner_radius.max(0.0), surface_fill);
    }

    // Focus не меняет glyph geometry и остаётся внутри полной hit-area.
    if state.focus_visible {
        painter.rect_stroke(
            rect.shrink(style.focus_inset.max(0.0)),
            style.surface_corner_radius.max(0.0),
            focus_outline,
            StrokeKind::Inside,
        );
    }

    // Видимая высота ограничена hit-area и масштабируется вокруг её центра.
    let visible_height = style.glyph_height.clamp(0.0, rect.height().max(0.0)) * content_scale;
    // Stroke также ограничен текущей высотой для безопасных узких rect.
    let stroke_width = style.glyph_stroke_width.max(0.0).min(visible_height);
    let stroke = Stroke::new(stroke_width, foreground);
    let geometry = undo_geometry(rect, visible_height, stroke_width);

    // Одна открытая polyline сохраняет единый визуальный вес дуги.
    painter.add(Shape::line(geometry.arc_points, stroke));
    // Две стороны образуют выраженный, но незалитый наконечник.
    painter.line_segment([geometry.arrow_tip, geometry.arrow_base_first], stroke);
    painter.line_segment([geometry.arrow_tip, geometry.arrow_base_second], stroke);

    // Малые круги превращают butt-caps egui в визуально круглые концы.
    let cap_radius = stroke_width * 0.5;
    for cap_center in [
        geometry.arc_start,
        geometry.arrow_tip,
        geometry.arrow_base_first,
        geometry.arrow_base_second,
    ] {
        painter.circle_filled(cap_center, cap_radius, foreground);
    }
}

/// Строит glyph так, чтобы итоговый visual bounds имел заданную высоту.
fn undo_geometry(rect: Rect, visible_height: f32, stroke_width: f32) -> UndoGeometry {
    // Сначала строим unit geometry, чтобы точно измерить её centerline bounds.
    let normalized_arc_points = (0..=ARC_SEGMENT_COUNT)
        .map(|segment_index| {
            let progress = segment_index as f32 / ARC_SEGMENT_COUNT as f32;
            let angle = ARC_START_RADIANS + (ARC_END_RADIANS - ARC_START_RADIANS) * progress;
            normalized_arc_point(angle)
        })
        .collect::<Vec<_>>();
    let normalized_arc_start = normalized_arc_point(ARC_START_RADIANS);
    let normalized_arrow_tip = normalized_arc_point(ARC_END_RADIANS);
    // Экранная касательная учитывает инвертированную относительно математики ось Y.
    let arrow_direction = vec2(-ARC_END_RADIANS.sin(), -ARC_END_RADIANS.cos());
    // Перпендикуляр раскрывает симметричное основание наконечника.
    let arrow_normal = vec2(-arrow_direction.y, arrow_direction.x);
    let arrow_base_center = normalized_arrow_tip - arrow_direction * ARROWHEAD_LENGTH;
    let normalized_arrow_base_first = arrow_base_center + arrow_normal * ARROWHEAD_HALF_WIDTH;
    let normalized_arrow_base_second = arrow_base_center - arrow_normal * ARROWHEAD_HALF_WIDTH;

    // Общий centerline bounds включает дугу и обе стороны наконечника.
    let normalized_bounds = normalized_arc_points
        .iter()
        .copied()
        .chain([normalized_arrow_base_first, normalized_arrow_base_second])
        .fold(Rect::NOTHING, |bounds, point| {
            bounds.union(Rect::from_min_max(point, point))
        });
    // Stroke добавляет половину ширины сверху и снизу, поэтому centerline получает остаток.
    let centerline_height = (visible_height - stroke_width).max(0.0);
    let geometry_scale = if normalized_bounds.height() > f32::EPSILON {
        centerline_height / normalized_bounds.height()
    } else {
        0.0
    };
    // Центрируем фактический bounds, а не условный design square.
    let normalized_center = normalized_bounds.center();
    let map_point = |point: Pos2| rect.center() + (point - normalized_center) * geometry_scale;

    UndoGeometry {
        arc_points: normalized_arc_points.into_iter().map(map_point).collect(),
        arc_start: map_point(normalized_arc_start),
        arrow_tip: map_point(normalized_arrow_tip),
        arrow_base_first: map_point(normalized_arrow_base_first),
        arrow_base_second: map_point(normalized_arrow_base_second),
    }
}

/// Возвращает точку дуги в нормализованной экранной системе координат.
fn normalized_arc_point(angle: f32) -> Pos2 {
    pos2(
        ARC_CENTER_X + ARC_RADIUS * angle.cos(),
        ARC_CENTER_Y - ARC_RADIUS * angle.sin(),
    )
}

/// Безопасно приводит внешний animation sample к диапазону `0.0..=1.0`.
fn normalized_unit_interval(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput, Vec2};

    use super::*;

    /// Production-like 32-point hit-area на fractional coordinates.
    fn hit_rect() -> Rect {
        Rect::from_min_size(pos2(10.25, 20.75), Vec2::splat(32.0))
    }

    /// Heading-height fixture повторяет app-side `TextStyle::Heading`.
    fn style() -> UndoButtonStyle {
        UndoButtonStyle {
            glyph_height: 18.0,
            glyph_stroke_width: 2.0,
            surface_corner_radius: 4.0,
            focus_outline: Stroke::new(1.5, Color32::WHITE),
            focus_inset: 1.5,
        }
    }

    /// Idle state не создаёт постоянную surface.
    fn state() -> UndoButtonPaintState {
        UndoButtonPaintState {
            foreground: Color32::from_gray(230),
            surface_fill: Color32::TRANSPARENT,
            focus_visible: false,
            opacity: 1.0,
            content_scale: 1.0,
        }
    }

    /// Возвращает объединённый visual bounds всех shapes.
    fn painted_bounds(
        rect: Rect,
        paint_state: UndoButtonPaintState,
        button_style: UndoButtonStyle,
    ) -> Rect {
        let output = Context::default().run_ui(RawInput::default(), |ui| {
            paint(ui.painter(), rect, paint_state, button_style);
        });
        output.shapes.iter().fold(Rect::NOTHING, |bounds, shape| {
            bounds.union(shape.shape.visual_bounding_rect())
        })
    }

    #[test]
    fn arrowhead_uses_strengthened_open_proportions() {
        let arrow_tip = normalized_arc_point(ARC_END_RADIANS);
        let arrow_direction = vec2(-ARC_END_RADIANS.sin(), -ARC_END_RADIANS.cos());
        let arrow_normal = vec2(-arrow_direction.y, arrow_direction.x);
        let arrow_base_center = arrow_tip - arrow_direction * ARROWHEAD_LENGTH;
        let arrow_base_first = arrow_base_center + arrow_normal * ARROWHEAD_HALF_WIDTH;

        assert!(((arrow_tip - arrow_base_center).length() - 0.24).abs() < f32::EPSILON);
        assert!(((arrow_base_first - arrow_base_center).length() - 0.13).abs() < f32::EPSILON);
    }

    #[test]
    fn final_visible_height_matches_requested_heading_height_and_stroke() {
        let bounds = painted_bounds(hit_rect(), state(), style());

        assert!((bounds.height() - style().glyph_height).abs() < 0.05);
        assert_eq!(style().glyph_stroke_width, 2.0);
    }

    #[test]
    fn glyph_bounds_stay_inside_hit_area() {
        let bounds = painted_bounds(hit_rect(), state(), style());

        assert!(hit_rect().contains_rect(bounds));
    }

    #[test]
    fn idle_has_no_surface_while_hover_and_focus_keep_their_decorations() {
        let idle_output = Context::default().run_ui(RawInput::default(), |ui| {
            paint(ui.painter(), hit_rect(), state(), style());
        });
        let decorated_output = Context::default().run_ui(RawInput::default(), |ui| {
            paint(
                ui.painter(),
                hit_rect(),
                UndoButtonPaintState {
                    surface_fill: Color32::from_white_alpha(28),
                    focus_visible: true,
                    ..state()
                },
                style(),
            );
        });

        // Hover surface и focus outline добавляются, idle glyph geometry не меняется.
        assert_eq!(decorated_output.shapes.len(), idle_output.shapes.len() + 2);
        assert!(
            idle_output
                .shapes
                .iter()
                .all(|shape| { !matches!(shape.shape, Shape::Rect(_)) })
        );
    }

    #[test]
    fn opacity_and_scale_preserve_hit_area_center() {
        let full_bounds = painted_bounds(hit_rect(), state(), style());
        let animated_bounds = painted_bounds(
            hit_rect(),
            UndoButtonPaintState {
                opacity: 0.5,
                content_scale: 0.8,
                ..state()
            },
            style(),
        );

        assert!((full_bounds.center().x - hit_rect().center().x).abs() < 0.05);
        assert!((full_bounds.center().y - hit_rect().center().y).abs() < 0.05);
        assert!((animated_bounds.center().x - hit_rect().center().x).abs() < 0.05);
        assert!((animated_bounds.center().y - hit_rect().center().y).abs() < 0.05);
        assert!(animated_bounds.height() < full_bounds.height());
    }

    #[test]
    fn fractional_hidpi_coordinates_keep_bounds_stable() {
        for pixels_per_point in [1.25, 1.5, 2.0, 2.5] {
            let context = Context::default();
            context.set_pixels_per_point(pixels_per_point);
            let output = context.run_ui(RawInput::default(), |ui| {
                paint(ui.painter(), hit_rect(), state(), style());
            });

            assert!(
                output
                    .shapes
                    .iter()
                    .all(|shape| { hit_rect().contains_rect(shape.shape.visual_bounding_rect()) })
            );
        }
    }
}
