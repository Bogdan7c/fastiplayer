//! Нейтральная рисовка рамки правки субтитров поверх видео: пунктирный прямоугольник
//! и четыре угловых маркера.
//!
//! Crate ничего не знает о мыши, режиме правки и субтитрах: вызывающая сторона передаёт
//! прямоугольник рамки, подсвеченный маркер (под курсором или в перетаскивании) и токены
//! стиля. Геометрия маркеров ([`subtitle_edit_frame_handle_rects`]) принадлежит этому
//! модулю и используется и рисованием, и поиском маркера под курсором на стороне
//! приложения, поэтому картинка и зона нажатия совпадают.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2};

/// Угловой маркер рамки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtitleEditFrameHandle {
    /// Левый верхний угол.
    TopLeft,
    /// Правый верхний угол.
    TopRight,
    /// Правый нижний угол.
    BottomRight,
    /// Левый нижний угол.
    BottomLeft,
}

impl SubtitleEditFrameHandle {
    /// Все маркеры в порядке обхода по часовой стрелке от левого верхнего.
    pub const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::BottomLeft,
    ];

    /// Угол рамки, в котором стоит маркер.
    #[must_use]
    pub fn corner(self, frame: Rect) -> Pos2 {
        match self {
            Self::TopLeft => frame.left_top(),
            Self::TopRight => frame.right_top(),
            Self::BottomRight => frame.right_bottom(),
            Self::BottomLeft => frame.left_bottom(),
        }
    }
}

/// Визуальное состояние рамки в этом кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubtitleEditFramePaintState {
    /// Маркер под курсором или тянущийся сейчас; он рисуется цветом `handle_hover_fill`.
    pub highlighted_handle: Option<SubtitleEditFrameHandle>,
}

/// Токены стиля рамки; цвета и размеры задаёт skin приложения.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubtitleEditFrameStyle {
    /// Линия пунктира.
    pub dash_stroke: Stroke,
    /// Длина штриха пунктира, points.
    pub dash_length: f32,
    /// Длина промежутка пунктира, points.
    pub gap_length: f32,
    /// Сторона нарисованного квадратного маркера, points.
    pub handle_extent: f32,
    /// Заливка маркера в покое.
    pub handle_fill: Color32,
    /// Заливка подсвеченного маркера.
    pub handle_hover_fill: Color32,
    /// Контур маркера.
    pub handle_outline: Stroke,
    /// Радиус скругления маркера, points.
    pub handle_corner_radius: f32,
}

/// Квадраты маркеров стороной `extent` с центрами в углах рамки.
///
/// Сторона передаётся отдельно, чтобы приложение могло взять зону нажатия больше
/// нарисованного маркера (проще попасть мышью), не дублируя расчёт позиций.
#[must_use]
pub fn subtitle_edit_frame_handle_rects(
    frame: Rect,
    extent: f32,
) -> [(SubtitleEditFrameHandle, Rect); 4] {
    SubtitleEditFrameHandle::ALL.map(|handle| {
        (
            handle,
            Rect::from_center_size(handle.corner(frame), vec2(extent, extent)),
        )
    })
}

/// Рисует пунктирную рамку и четыре угловых маркера.
///
/// Вырожденная (нулевая или нечисловая) рамка не рисуется вовсе.
pub(crate) fn paint(
    painter: &Painter,
    frame: Rect,
    state: SubtitleEditFramePaintState,
    style: SubtitleEditFrameStyle,
) {
    if !frame.is_finite() || !frame.is_positive() {
        return;
    }
    // Замкнутый контур: пять точек, последняя повторяет первую.
    let outline: [Pos2; 5] = [
        frame.left_top(),
        frame.right_top(),
        frame.right_bottom(),
        frame.left_bottom(),
        frame.left_top(),
    ];
    painter.extend(Shape::dashed_line(
        &outline,
        style.dash_stroke,
        style.dash_length,
        style.gap_length,
    ));
    for (handle, handle_rect) in subtitle_edit_frame_handle_rects(frame, style.handle_extent) {
        let fill = if state.highlighted_handle == Some(handle) {
            style.handle_hover_fill
        } else {
            style.handle_fill
        };
        painter.rect(
            handle_rect,
            CornerRadius::from(style.handle_corner_radius.clamp(0.0, 255.0).round() as u8),
            fill,
            style.handle_outline,
            StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput, pos2};

    use super::*;
    use crate::ArtworkPainter;

    fn style() -> SubtitleEditFrameStyle {
        SubtitleEditFrameStyle {
            dash_stroke: Stroke::new(1.5, Color32::WHITE),
            dash_length: 6.0,
            gap_length: 4.0,
            handle_extent: 10.0,
            handle_fill: Color32::from_rgb(10, 20, 30),
            handle_hover_fill: Color32::from_rgb(200, 210, 220),
            handle_outline: Stroke::new(1.0, Color32::WHITE),
            handle_corner_radius: 2.0,
        }
    }

    fn frame() -> Rect {
        Rect::from_min_max(pos2(100.0, 200.0), pos2(300.0, 260.0))
    }

    fn painted_shapes(
        frame: Rect,
        state: SubtitleEditFramePaintState,
    ) -> Vec<egui::epaint::ClippedShape> {
        crate::test_frame::run_ui_frame(&Context::default(), RawInput::default(), |ui| {
            ArtworkPainter::new(ui.painter()).subtitle_edit_frame(frame, state, style());
        })
        .shapes
    }

    /// Квадратные маркеры (прямоугольные shape-ы) среди всего, что нарисовано.
    fn handle_fills(shapes: &[egui::epaint::ClippedShape]) -> Vec<Color32> {
        shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                Shape::Rect(rect_shape) => Some(rect_shape.fill),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn handle_rects_are_centered_on_the_four_corners() {
        let rects = subtitle_edit_frame_handle_rects(frame(), 10.0);
        assert_eq!(rects[0].0, SubtitleEditFrameHandle::TopLeft);
        assert_eq!(rects[0].1.center(), pos2(100.0, 200.0));
        assert_eq!(rects[2].0, SubtitleEditFrameHandle::BottomRight);
        assert_eq!(rects[2].1.center(), pos2(300.0, 260.0));
        assert!(
            rects
                .iter()
                .all(|(_, rect)| rect.size() == vec2(10.0, 10.0))
        );
    }

    #[test]
    fn frame_paints_dashes_and_exactly_four_handles() {
        let shapes = painted_shapes(frame(), SubtitleEditFramePaintState::default());
        assert_eq!(handle_fills(&shapes).len(), 4);
        // Периметр 520 points при периоде 10: штрихов много, а не одна сплошная линия.
        assert!(shapes.len() > 4 + 20, "{} shapes", shapes.len());
    }

    #[test]
    fn only_the_highlighted_handle_uses_the_hover_fill() {
        let shapes = painted_shapes(
            frame(),
            SubtitleEditFramePaintState {
                highlighted_handle: Some(SubtitleEditFrameHandle::BottomRight),
            },
        );
        let fills = handle_fills(&shapes);
        assert_eq!(fills.len(), 4);
        let hover_count = fills
            .iter()
            .filter(|fill| **fill == style().handle_hover_fill)
            .count();
        assert_eq!(hover_count, 1);
        // Маркеры идут в порядке ALL, подсвеченный - третий.
        assert_eq!(fills[2], style().handle_hover_fill);
    }

    #[test]
    fn degenerate_frame_paints_nothing() {
        let empty = Rect::from_min_max(pos2(5.0, 5.0), pos2(5.0, 5.0));
        assert!(painted_shapes(empty, SubtitleEditFramePaintState::default()).is_empty());
        let not_a_number = Rect::from_min_max(pos2(f32::NAN, 0.0), pos2(10.0, 10.0));
        assert!(painted_shapes(not_a_number, SubtitleEditFramePaintState::default()).is_empty());
    }
}
