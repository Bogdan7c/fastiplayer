//! Нейтральная векторная геометрия кнопки субтитров «CC» и стрелки меню «▾».
//!
//! Буквы «CC» рисуются двумя открытыми дугами, поэтому отрисовка не зависит от шрифтов.
//! Подчёркивание под значком показывает, что субтитры включены: его ширина и прозрачность
//! растут вместе с `enabled_progress`, который анимирует вызывающая сторона.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

/// Уже разрешённое вызывающей стороной визуальное состояние кнопки CC.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubtitlesButtonPaintState {
    /// Итоговый цвет значка после hover/disabled переходов (и общей прозрачности появления).
    pub foreground: Color32,

    /// Круглая поверхность; прозрачный цвет означает отсутствие shape.
    pub surface_fill: Color32,

    /// Видим ли keyboard-focus outline.
    pub focus_visible: bool,

    /// Масштабируется только значок, поэтому hit-area и layout остаются неизменными.
    pub content_scale: f32,

    /// Прогресс «субтитры включены» `0.0..=1.0`: управляет подчёркиванием.
    pub enabled_progress: f32,
}

/// Размеры и outline-токены кнопки CC и стрелки меню.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubtitlesButtonStyle {
    /// Ширина значка «CC» (прямоугольной рамки); высота выводится из неё.
    pub icon_extent: f32,

    /// Толщина всех линий значка.
    pub glyph_stroke_width: f32,

    /// Keyboard-focus outline, уже окрашенный skin-ом.
    pub focus_outline: Stroke,

    /// Inset outline от границы hit-area.
    pub focus_inset: f32,

    /// Толщина подчёркивания включённых субтитров.
    pub underline_thickness: f32,
}

/// Пропорция высоты рамки значка к её ширине (как у знака CC на телевизорах).
const FRAME_HEIGHT_TO_WIDTH: f32 = 0.78;
/// Радиус скругления рамки относительно её высоты.
const FRAME_CORNER_RADIUS_TO_HEIGHT: f32 = 0.28;
/// Радиус каждой буквы C относительно высоты рамки.
///
/// Центры букв разнесены на 0.38 ширины рамки, а высота рамки — 0.78 ширины: диаметр буквы
/// 2 × 0.2 × 0.78 ≈ 0.31 ширины, остаётся просвет ~0.07 ширины. Больший радиус (0.27 давал
/// диаметр 0.42 > 0.4) склеивал буквы в одну фигуру.
const LETTER_RADIUS_TO_FRAME_HEIGHT: f32 = 0.2;
/// Угол раскрытия буквы C: дуга не рисуется в секторе ±этот угол справа от центра.
const LETTER_OPENING_HALF_ANGLE: f32 = 0.95;
/// Центры букв: доли ширины рамки от её левого края (симметрично относительно середины).
const LETTER_CENTERS_TO_FRAME_WIDTH: [f32; 2] = [0.31, 0.69];
/// Число отрезков, которыми аппроксимируется дуга буквы.
const LETTER_ARC_SEGMENTS: usize = 10;
/// Зазор между рамкой значка и подчёркиванием относительно высоты рамки.
const UNDERLINE_GAP_TO_FRAME_HEIGHT: f32 = 0.22;

/// Рисует поверхность, focus и значок «CC» с подчёркиванием внутри стабильной hit-area.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    state: SubtitlesButtonPaintState,
    style: SubtitlesButtonStyle,
) {
    let surface_radius = rect.width().min(rect.height()) * 0.5;
    if state.surface_fill != Color32::TRANSPARENT {
        painter.circle_filled(rect.center(), surface_radius, state.surface_fill);
    }
    if state.focus_visible {
        painter.circle_stroke(
            rect.center(),
            (surface_radius - style.focus_inset).max(0.0),
            style.focus_outline,
        );
    }

    let content_scale = state.content_scale.clamp(0.0, 1.0);
    let frame_width = style.icon_extent * content_scale;
    let frame_height = frame_width * FRAME_HEIGHT_TO_WIDTH;
    let stroke = Stroke::new(style.glyph_stroke_width, state.foreground);

    // Значок и подчёркивание вместе центрируются по вертикали в hit-area: подчёркивание
    // занимает место под рамкой, поэтому саму рамку сдвигаем чуть выше центра.
    let underline_offset = frame_height * UNDERLINE_GAP_TO_FRAME_HEIGHT + style.underline_thickness;
    let frame_center = pos2(rect.center().x, rect.center().y - underline_offset * 0.5);
    let frame_rect = Rect::from_center_size(frame_center, vec2(frame_width, frame_height));
    painter.rect_stroke(
        frame_rect,
        frame_height * FRAME_CORNER_RADIUS_TO_HEIGHT,
        stroke,
        egui::StrokeKind::Inside,
    );

    // Две буквы C, разнесённые так, чтобы между ними оставался просвет.
    let letter_radius = frame_height * LETTER_RADIUS_TO_FRAME_HEIGHT;
    for letter_center_fraction in LETTER_CENTERS_TO_FRAME_WIDTH {
        let letter_center_x = frame_rect.left() + frame_width * letter_center_fraction;
        painter.add(Shape::line(
            letter_arc_points(pos2(letter_center_x, frame_center.y), letter_radius),
            stroke,
        ));
    }

    paint_underline(painter, frame_rect, frame_height, state, style);
}

/// Подчёркивание под рамкой: растёт от центра и проявляется вместе с `enabled_progress`.
fn paint_underline(
    painter: &Painter,
    frame_rect: Rect,
    frame_height: f32,
    state: SubtitlesButtonPaintState,
    style: SubtitlesButtonStyle,
) {
    let progress = state.enabled_progress.clamp(0.0, 1.0);
    if progress <= 0.0 {
        return;
    }
    let y = frame_rect.bottom()
        + frame_height * UNDERLINE_GAP_TO_FRAME_HEIGHT
        + style.underline_thickness * 0.5;
    let half_width = frame_rect.width() * 0.5 * progress;
    let color = state.foreground.gamma_multiply(progress);
    painter.line_segment(
        [
            pos2(frame_rect.center().x - half_width, y),
            pos2(frame_rect.center().x + half_width, y),
        ],
        Stroke::new(style.underline_thickness, color),
    );
}

/// Точки дуги буквы C: окружность без сектора справа (буква «открыта» вправо).
fn letter_arc_points(center: Pos2, radius: f32) -> Vec<Pos2> {
    // Дуга идёт от верхнего края раскрытия против часовой стрелки до нижнего.
    let start_angle = LETTER_OPENING_HALF_ANGLE;
    let end_angle = std::f32::consts::TAU - LETTER_OPENING_HALF_ANGLE;
    (0..=LETTER_ARC_SEGMENTS)
        .map(|segment_index| {
            let progress = segment_index as f32 / LETTER_ARC_SEGMENTS as f32;
            let angle = start_angle + (end_angle - start_angle) * progress;
            // В egui ось Y направлена вниз; знак минус делает обход визуально против часовой.
            pos2(
                center.x + radius * angle.cos(),
                center.y - radius * angle.sin(),
            )
        })
        .collect()
}

/// Рисует стрелку «▾» меню субтитров внутри своей hit-area (ширина меньше кнопки CC).
pub(crate) fn paint_menu_arrow(
    painter: &Painter,
    rect: Rect,
    state: SubtitlesButtonPaintState,
    style: SubtitlesButtonStyle,
) {
    if state.surface_fill != Color32::TRANSPARENT {
        painter.rect_filled(
            rect.shrink2(vec2(0.0, 4.0)),
            rect.width() * 0.5,
            state.surface_fill,
        );
    }
    if state.focus_visible {
        painter.rect_stroke(
            rect.shrink(style.focus_inset),
            rect.width() * 0.5,
            style.focus_outline,
            egui::StrokeKind::Inside,
        );
    }
    let content_scale = state.content_scale.clamp(0.0, 1.0);
    let half_width = rect.width() * 0.22 * content_scale;
    let half_height = half_width * 0.6;
    let center = rect.center();
    painter.add(Shape::line(
        vec![
            pos2(center.x - half_width, center.y - half_height),
            pos2(center.x, center.y + half_height),
            pos2(center.x + half_width, center.y - half_height),
        ],
        Stroke::new(style.glyph_stroke_width, state.foreground),
    ));
}

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput};

    use super::*;
    use crate::ArtworkPainter;

    fn style() -> SubtitlesButtonStyle {
        SubtitlesButtonStyle {
            icon_extent: 18.0,
            glyph_stroke_width: 1.6,
            focus_outline: Stroke::new(1.5_f32, Color32::WHITE),
            focus_inset: 1.5,
            underline_thickness: 1.6,
        }
    }

    fn state(enabled_progress: f32) -> SubtitlesButtonPaintState {
        SubtitlesButtonPaintState {
            foreground: Color32::WHITE,
            surface_fill: Color32::TRANSPARENT,
            focus_visible: false,
            content_scale: 1.0,
            enabled_progress,
        }
    }

    fn hit_rect() -> Rect {
        Rect::from_center_size(pos2(50.0, 50.0), vec2(32.0, 32.0))
    }

    fn painted_shapes(paint: impl FnMut(ArtworkPainter<'_>)) -> Vec<egui::epaint::ClippedShape> {
        let mut paint = paint;
        crate::test_frame::run_ui_frame(&Context::default(), RawInput::default(), |ui| {
            paint(ArtworkPainter::new(ui.painter()));
        })
        .shapes
    }

    #[test]
    fn disabled_subtitles_have_no_underline_and_enabled_add_exactly_one_line() {
        let off = painted_shapes(|painter| {
            painter.subtitles_button(hit_rect(), state(0.0), style());
        });
        let on = painted_shapes(|painter| {
            painter.subtitles_button(hit_rect(), state(1.0), style());
        });
        // Рамка + две буквы; подчёркивание добавляет ровно один shape.
        assert_eq!(off.len(), 3);
        assert_eq!(on.len(), 4);
    }

    #[test]
    fn underline_grows_from_the_center_with_progress() {
        let half = painted_shapes(|painter| {
            painter.subtitles_button(hit_rect(), state(0.5), style());
        });
        let full = painted_shapes(|painter| {
            painter.subtitles_button(hit_rect(), state(1.0), style());
        });
        let half_width = half[3].shape.visual_bounding_rect().width();
        let full_width = full[3].shape.visual_bounding_rect().width();
        assert!(half_width < full_width, "{half_width} !< {full_width}");
        assert!(
            (half[3].shape.visual_bounding_rect().center().x - hit_rect().center().x).abs() < 0.01
        );
    }

    #[test]
    fn glyph_and_underline_stay_inside_the_hit_area_at_every_progress() {
        for progress in [0.0, 0.5, 1.0] {
            let shapes = painted_shapes(|painter| {
                painter.subtitles_button(hit_rect(), state(progress), style());
            });
            assert!(!shapes.is_empty());
            for shape in shapes {
                assert!(
                    hit_rect().contains_rect(shape.shape.visual_bounding_rect()),
                    "{:?}",
                    shape.shape.visual_bounding_rect()
                );
            }
        }
    }

    #[test]
    fn focus_and_hover_surfaces_add_shapes_without_moving_the_glyph() {
        let plain = painted_shapes(|painter| {
            painter.subtitles_button(hit_rect(), state(0.0), style());
        });
        let decorated = painted_shapes(|painter| {
            let mut decorated_state = state(0.0);
            decorated_state.surface_fill = Color32::from_white_alpha(28);
            decorated_state.focus_visible = true;
            painter.subtitles_button(hit_rect(), decorated_state, style());
        });
        assert_eq!(decorated.len(), plain.len() + 2);
        for (plain_shape, decorated_shape) in plain.iter().zip(decorated.iter().skip(2)) {
            assert_eq!(
                plain_shape.shape.visual_bounding_rect(),
                decorated_shape.shape.visual_bounding_rect()
            );
        }
    }

    #[test]
    fn menu_arrow_is_a_single_chevron_inside_its_narrow_hit_area() {
        let arrow_rect = Rect::from_center_size(pos2(80.0, 50.0), vec2(14.0, 32.0));
        let shapes = painted_shapes(|painter| {
            painter.subtitles_menu_arrow(arrow_rect, state(1.0), style());
        });
        assert_eq!(shapes.len(), 1);
        assert!(arrow_rect.contains_rect(shapes[0].shape.visual_bounding_rect()));
    }
}
