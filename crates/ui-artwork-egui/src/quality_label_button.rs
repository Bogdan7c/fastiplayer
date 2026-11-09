//! Кнопка качества с текстовой подписью («1080p») в той же рамке, что у CC и «♪».
//!
//! Значка нет: подпись — сам текст, поэтому рамка шире квадратной hit-area CC и «♪» (ширину
//! задаёт вызывающая сторона прямоугольником). Состояния и размеры берутся из
//! [`SubtitlesButtonPaintState`] и [`SubtitlesButtonStyle`] (`icon_extent` — размер значка CC,
//! от него считается кегль подписи); у кнопки нет режима «включено/выключено», поэтому
//! `enabled_progress` и `underline_thickness` игнорируются.

use egui::{Align2, Color32, FontId, Painter, Rect, Stroke, StrokeKind, vec2};

use crate::{SubtitlesButtonPaintState, SubtitlesButtonStyle};

/// Кегль подписи относительно `icon_extent`.
const LABEL_FONT_TO_ICON_EXTENT: f32 = 0.62;
/// Радиус скругления рамки относительно её высоты (как у CC).
const FRAME_CORNER_RADIUS_TO_HEIGHT: f32 = 0.28;
/// Высота рамки относительно высоты hit-area.
const FRAME_HEIGHT_TO_HIT_AREA: f32 = 0.62;

/// Рисует поверхность, focus и рамку с подписью внутри стабильной hit-area.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    label: &str,
    state: SubtitlesButtonPaintState,
    style: SubtitlesButtonStyle,
) {
    let surface_radius = rect.width().min(rect.height()) * 0.5;
    if state.surface_fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, surface_radius, state.surface_fill);
    }
    if state.focus_visible {
        painter.rect_stroke(
            rect.shrink(style.focus_inset),
            surface_radius,
            style.focus_outline,
            StrokeKind::Inside,
        );
    }

    let content_scale = state.content_scale.clamp(0.0, 1.0);
    let frame_rect = Rect::from_center_size(
        rect.center(),
        vec2(
            (rect.width() - style.focus_inset * 2.0).max(0.0) * content_scale,
            rect.height() * FRAME_HEIGHT_TO_HIT_AREA * content_scale,
        ),
    );
    painter.rect_stroke(
        frame_rect,
        frame_rect.height() * FRAME_CORNER_RADIUS_TO_HEIGHT,
        Stroke::new(style.glyph_stroke_width, state.foreground),
        StrokeKind::Inside,
    );
    // Кегль нуль egui не принимает (паника шрифта): свёрнутая кнопка рисует только рамку.
    if content_scale > 0.0 {
        painter.text(
            frame_rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(style.icon_extent * LABEL_FONT_TO_ICON_EXTENT * content_scale),
            state.foreground,
        );
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput, Shape, pos2};

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

    fn state() -> SubtitlesButtonPaintState {
        SubtitlesButtonPaintState {
            foreground: Color32::WHITE,
            surface_fill: Color32::TRANSPARENT,
            focus_visible: false,
            content_scale: 1.0,
            enabled_progress: 0.0,
        }
    }

    fn hit_rect() -> Rect {
        Rect::from_center_size(pos2(50.0, 50.0), vec2(56.0, 32.0))
    }

    fn painted_shapes(
        label: &str,
        state: SubtitlesButtonPaintState,
    ) -> Vec<egui::epaint::ClippedShape> {
        let mut painted = false;
        crate::test_frame::run_ui_frame(&Context::default(), RawInput::default(), |ui| {
            if !painted {
                painted = true;
                ArtworkPainter::new(ui.painter()).quality_label_button(
                    hit_rect(),
                    label,
                    state,
                    style(),
                );
            }
        })
        .shapes
    }

    fn painted_label(shapes: &[egui::epaint::ClippedShape]) -> Option<String> {
        shapes.iter().find_map(|clipped| match &clipped.shape {
            Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
    }

    #[test]
    fn the_label_is_drawn_inside_a_frame_inside_the_hit_area() {
        let shapes = painted_shapes("1080p", state());
        assert_eq!(shapes.len(), 2, "frame and label");
        assert_eq!(painted_label(&shapes).as_deref(), Some("1080p"));
        for shape in &shapes {
            assert!(
                hit_rect().contains_rect(shape.shape.visual_bounding_rect()),
                "{:?}",
                shape.shape.visual_bounding_rect()
            );
        }
        let frame = shapes[0].shape.visual_bounding_rect();
        assert!(frame.contains_rect(shapes[1].shape.visual_bounding_rect()));
    }

    #[test]
    fn focus_and_hover_surfaces_add_shapes_without_moving_the_label() {
        let plain = painted_shapes("720p", state());
        let mut decorated_state = state();
        decorated_state.surface_fill = Color32::from_white_alpha(28);
        decorated_state.focus_visible = true;
        let decorated = painted_shapes("720p", decorated_state);
        assert_eq!(decorated.len(), plain.len() + 2);
        for (plain_shape, decorated_shape) in plain.iter().zip(decorated.iter().skip(2)) {
            assert_eq!(
                plain_shape.shape.visual_bounding_rect(),
                decorated_shape.shape.visual_bounding_rect()
            );
        }
    }

    #[test]
    fn zero_content_scale_collapses_the_frame() {
        let mut collapsed = state();
        collapsed.content_scale = 0.0;
        let shapes = painted_shapes("480p", collapsed);
        assert_eq!(painted_label(&shapes), None, "no label at zero scale");
        let frame = shapes[0].shape.visual_bounding_rect();
        assert!(
            frame.width() <= style().glyph_stroke_width + 0.01,
            "{frame:?}"
        );
    }
}
