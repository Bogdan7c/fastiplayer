use crate::ButtonVisualState;
use egui::{Color32, Painter, Rect, Stroke, StrokeKind, Vec2, pos2};
/// Вариант системной кнопки окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControlGlyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}
/// Стиль системной кнопки окна.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowControlStyle {
    pub fill: Color32,
    pub stroke: Stroke,
    pub hover_fill: Color32,
}
pub(crate) fn paint(
    p: &Painter,
    r: Rect,
    g: WindowControlGlyph,
    s: ButtonVisualState,
    style: WindowControlStyle,
) {
    if s == ButtonVisualState::Hovered {
        p.rect_filled(r, 0.0, style.hover_fill);
    }
    let c = r.center();
    match g {
        WindowControlGlyph::Minimize => {
            p.line_segment(
                [pos2(c.x - 6.0, c.y + 5.0), pos2(c.x + 6.0, c.y + 5.0)],
                style.stroke,
            );
        }
        WindowControlGlyph::Maximize => {
            p.rect_stroke(
                Rect::from_center_size(c, Vec2::new(12.0, 10.0)),
                0.0,
                style.stroke,
                StrokeKind::Inside,
            );
        }
        WindowControlGlyph::Restore => {
            let back = Rect::from_center_size(pos2(c.x + 2.0, c.y - 2.0), Vec2::new(10.0, 8.0));
            let front = Rect::from_center_size(pos2(c.x - 2.0, c.y + 2.0), Vec2::new(10.0, 8.0));
            p.rect_stroke(back, 0.0, style.stroke, StrokeKind::Inside);
            p.rect_filled(front.expand(1.0), 0.0, style.fill);
            p.rect_stroke(front, 0.0, style.stroke, StrokeKind::Inside);
        }
        WindowControlGlyph::Close => {
            p.line_segment(
                [pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)],
                style.stroke,
            );
            p.line_segment(
                [pos2(c.x + 5.0, c.y - 5.0), pos2(c.x - 5.0, c.y + 5.0)],
                style.stroke,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use egui::{Context, RawInput, epaint::ClippedShape};

    use super::*;

    /// Hit area кнопки окна для headless-рисования.
    fn hit_rect() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), Vec2::splat(40.0))
    }

    /// Рисует одну кнопку окна в headless egui и возвращает получившиеся фигуры.
    fn painted_shapes(glyph: WindowControlGlyph, state: ButtonVisualState) -> Vec<ClippedShape> {
        let style = WindowControlStyle {
            fill: Color32::BLACK,
            stroke: Stroke::new(1.0, Color32::WHITE),
            hover_fill: Color32::GRAY,
        };
        let output = Context::default().run_ui(RawInput::default(), |ui| {
            paint(ui.painter(), hit_rect(), glyph, state, style);
        });
        output.shapes
    }

    /// Каждая кнопка окна рисует свой, отличимый от других глиф внутри своей
    /// hit-area, а hover добавляет ровно одну подложку поверх idle-рисунка.
    #[test]
    fn every_window_control_has_distinct_glyph_and_hover_surface() {
        let glyphs = [
            WindowControlGlyph::Minimize,
            WindowControlGlyph::Maximize,
            WindowControlGlyph::Restore,
            WindowControlGlyph::Close,
        ];
        let mut idle_fingerprints = HashSet::new();
        for glyph in glyphs {
            let idle_shapes = painted_shapes(glyph, ButtonVisualState::Idle);
            let hovered_shapes = painted_shapes(glyph, ButtonVisualState::Hovered);

            assert!(!idle_shapes.is_empty(), "{glyph:?} must paint a glyph");
            assert_eq!(
                hovered_shapes.len(),
                idle_shapes.len() + 1,
                "{glyph:?} hover must add exactly one surface"
            );
            for shape in &idle_shapes {
                assert!(
                    hit_rect().contains_rect(shape.shape.visual_bounding_rect()),
                    "{glyph:?} glyph must stay inside its hit area"
                );
            }
            idle_fingerprints.insert(format!("{idle_shapes:?}"));
        }
        // Если два глифа рисуются одинаково (например, «Закрыть» как «Свернуть»),
        // пользователь их не различит — множество отпечатков станет меньше.
        assert_eq!(idle_fingerprints.len(), glyphs.len());
    }
}
