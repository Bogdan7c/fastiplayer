//! Нейтральная векторная геометрия кнопки выбора аудиодорожки «♪».
//!
//! Значок — нота внутри такой же скруглённой рамки, как у кнопки субтитров «CC», поэтому две
//! кнопки рядом выглядят как пара. Нота рисуется фигурами (головка, штиль, флажок), а не
//! шрифтом: отрисовка не зависит от наличия глифа в шрифте. Стрелка «▾» меню — та же, что у
//! субтитров (`ArtworkPainter::subtitles_menu_arrow`).
//!
//! Состояния и размеры берутся из [`SubtitlesButtonPaintState`] и [`SubtitlesButtonStyle`]:
//! у аудио нет режима «включено/выключено», поэтому `enabled_progress` и
//! `underline_thickness` здесь игнорируются.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use crate::{SubtitlesButtonPaintState, SubtitlesButtonStyle};

/// Пропорция высоты рамки значка к её ширине (как у CC, чтобы рамки совпадали).
const FRAME_HEIGHT_TO_WIDTH: f32 = 0.78;
/// Радиус скругления рамки относительно её высоты (как у CC).
const FRAME_CORNER_RADIUS_TO_HEIGHT: f32 = 0.28;
/// Радиус головки ноты относительно высоты рамки.
const NOTE_HEAD_RADIUS_TO_FRAME_HEIGHT: f32 = 0.15;
/// Центр головки ноты: доля ширины рамки от её левого края.
const NOTE_HEAD_CENTER_X_TO_FRAME_WIDTH: f32 = 0.42;
/// Центр головки ноты: доля высоты рамки от её нижнего края (вверх).
const NOTE_HEAD_CENTER_RISE_TO_FRAME_HEIGHT: f32 = 0.30;
/// Верх штиля: отступ вниз от верхнего края рамки относительно её высоты.
const NOTE_STEM_TOP_INSET_TO_FRAME_HEIGHT: f32 = 0.20;
/// Вынос флажка вправо от штиля относительно ширины рамки.
const NOTE_FLAG_REACH_TO_FRAME_WIDTH: f32 = 0.17;
/// Опускание конца флажка от верха штиля относительно высоты рамки.
const NOTE_FLAG_DROP_TO_FRAME_HEIGHT: f32 = 0.30;

/// Рисует поверхность, focus и значок «♪» внутри стабильной hit-area.
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
    let frame_rect = Rect::from_center_size(rect.center(), vec2(frame_width, frame_height));
    painter.rect_stroke(
        frame_rect,
        frame_height * FRAME_CORNER_RADIUS_TO_HEIGHT,
        stroke,
        egui::StrokeKind::Inside,
    );

    paint_note(painter, frame_rect, state.foreground, stroke);
}

/// Нота: залитая головка, штиль вверх от её правого края и флажок вправо-вниз.
fn paint_note(painter: &Painter, frame_rect: Rect, foreground: Color32, stroke: Stroke) {
    let frame_width = frame_rect.width();
    let frame_height = frame_rect.height();
    let head_radius = frame_height * NOTE_HEAD_RADIUS_TO_FRAME_HEIGHT;
    let head_center = pos2(
        frame_rect.left() + frame_width * NOTE_HEAD_CENTER_X_TO_FRAME_WIDTH,
        frame_rect.bottom() - frame_height * NOTE_HEAD_CENTER_RISE_TO_FRAME_HEIGHT,
    );
    painter.circle_filled(head_center, head_radius, foreground);

    // Штиль растёт от правого края головки к верху рамки.
    let stem_x = head_center.x + head_radius;
    let stem_top = pos2(
        stem_x,
        frame_rect.top() + frame_height * NOTE_STEM_TOP_INSET_TO_FRAME_HEIGHT,
    );
    painter.line_segment([pos2(stem_x, head_center.y), stem_top], stroke);

    // Флажок: короткая линия от верха штиля вправо и вниз.
    painter.add(Shape::line(
        flag_points(
            stem_top,
            frame_width * NOTE_FLAG_REACH_TO_FRAME_WIDTH,
            frame_height * NOTE_FLAG_DROP_TO_FRAME_HEIGHT,
        ),
        stroke,
    ));
}

/// Точки флажка: от верха штиля к концу `reach` правее и `drop` ниже.
fn flag_points(stem_top: Pos2, reach: f32, drop: f32) -> Vec<Pos2> {
    vec![stem_top, pos2(stem_top.x + reach, stem_top.y + drop)]
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
        Rect::from_center_size(pos2(50.0, 50.0), vec2(32.0, 32.0))
    }

    fn painted_shapes(paint: impl FnOnce(ArtworkPainter<'_>)) -> Vec<egui::epaint::ClippedShape> {
        let mut paint = Some(paint);
        crate::test_frame::run_ui_frame(&Context::default(), RawInput::default(), |ui| {
            if let Some(paint) = paint.take() {
                paint(ArtworkPainter::new(ui.painter()));
            }
        })
        .shapes
    }

    #[test]
    fn glyph_is_frame_plus_note_head_stem_and_flag_inside_the_hit_area() {
        let shapes = painted_shapes(|painter| {
            painter.audio_tracks_button(hit_rect(), state(), style());
        });
        // Рамка, головка, штиль, флажок.
        assert_eq!(shapes.len(), 4);
        for shape in shapes {
            assert!(
                hit_rect().contains_rect(shape.shape.visual_bounding_rect()),
                "{:?}",
                shape.shape.visual_bounding_rect()
            );
        }
    }

    #[test]
    fn note_stays_inside_the_frame() {
        let shapes = painted_shapes(|painter| {
            painter.audio_tracks_button(hit_rect(), state(), style());
        });
        let frame = shapes[0].shape.visual_bounding_rect();
        for note_shape in &shapes[1..] {
            let bounds = note_shape.shape.visual_bounding_rect();
            assert!(frame.contains_rect(bounds), "{bounds:?} outside {frame:?}");
        }
    }

    #[test]
    fn frame_matches_the_cc_frame_so_the_two_buttons_pair_up() {
        let audio_shapes = painted_shapes(|painter| {
            painter.audio_tracks_button(hit_rect(), state(), style());
        });
        let cc_shapes = painted_shapes(|painter| {
            painter.subtitles_button(hit_rect(), state(), style());
        });
        // Ширина рамки совпадает; по вертикали CC смещена подчёркиванием, поэтому сравниваем размер.
        assert_eq!(
            audio_shapes[0].shape.visual_bounding_rect().size(),
            cc_shapes[0].shape.visual_bounding_rect().size()
        );
    }

    #[test]
    fn focus_and_hover_surfaces_add_shapes_without_moving_the_glyph() {
        let plain = painted_shapes(|painter| {
            painter.audio_tracks_button(hit_rect(), state(), style());
        });
        let decorated = painted_shapes(|painter| {
            let mut decorated_state = state();
            decorated_state.surface_fill = Color32::from_white_alpha(28);
            decorated_state.focus_visible = true;
            painter.audio_tracks_button(hit_rect(), decorated_state, style());
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
    fn zero_content_scale_collapses_the_glyph() {
        let mut collapsed = state();
        collapsed.content_scale = 0.0;
        let shapes = painted_shapes(|painter| {
            painter.audio_tracks_button(hit_rect(), collapsed, style());
        });
        let frame = shapes[0].shape.visual_bounding_rect();
        assert!(
            frame.width() <= style().glyph_stroke_width + 0.01,
            "{frame:?}"
        );
    }
}
