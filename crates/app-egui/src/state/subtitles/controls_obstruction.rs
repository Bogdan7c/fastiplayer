//! Что сейчас закрывает низ видео: верхняя граница видимой нижней панели управления.
//!
//! Нижняя панель рисуется поверх видео (видео занимает всё окно под панелями), поэтому
//! субтитры, стоящие низко, могли бы оказаться под кнопками. Владелец субтитров получает
//! от UI типизированное наблюдение «панели нет» либо «её видимая верхняя граница в
//! физических пикселях» и поднимает слой только тогда, когда он реально мешает
//! ([`super::layer_placement`]).
//!
//! Наблюдение приходит КАЖДЫЙ кадр UI: при выезде/уезде панели в фуллскрине её видимый
//! прямоугольник меняется плавно, и подъём субтитров повторяет это движение без отдельной
//! анимации.

/// Зазор между низом субтитров и верхом панели управления, в точках egui (не пикселях).
///
/// Это визуальное «дыхание», чтобы текст не прилипал к панели: 8 pt - стандартная базовая
/// единица отступов интерфейса. В пиксели переводится масштабом UI (`pixels_per_point`),
/// чтобы на HiDPI зазор выглядел так же.
pub(crate) const CONTROLS_CLEARANCE_POINTS: f32 = 8.0;

/// Наблюдение за нижней панелью управления в физических пикселях окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BottomControlsObstruction {
    /// Панели нет (полностью скрыта) либо её геометрия непригодна: ничего не мешает.
    #[default]
    Absent,
    /// Панель видна от `top_px` вниз; слой должен оставаться выше `top_px - clearance_px`.
    TopEdge {
        /// Верхняя граница видимой части панели, пиксели окна сверху вниз.
        top_px: i32,
        /// Требуемый зазор между низом слоя и панелью, пиксели.
        clearance_px: i32,
    },
}

impl BottomControlsObstruction {
    /// Строит наблюдение из видимого прямоугольника панели (точки egui, координаты окна).
    ///
    /// `None` (панель полностью скрыта), пустой или нечисловой прямоугольник дают
    /// [`Self::Absent`]. Верхняя граница переводится с округлением вниз - так же, как верх
    /// области видео (`video_viewport_from_ui_rect`), поэтому панель и видео делят одну
    /// пиксельную сетку; зазор округляется вверх, чтобы не стать меньше задуманного.
    pub(crate) fn from_visible_rect(
        visible_rect: Option<egui::Rect>,
        pixels_per_point: f32,
    ) -> Self {
        let Some(rect) = visible_rect else {
            return Self::Absent;
        };
        let scale = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
            pixels_per_point
        } else {
            1.0
        };
        let top = (rect.min.y.max(0.0) * scale).floor();
        // Сравнение в форме `>=` сознательно: NaN его не проходит. Верх панели не может
        // уходить за диапазон i32 - иначе наблюдение бессмысленно.
        if rect.is_negative() || !rect.is_finite() || !(top >= 0.0 && top <= i32::MAX as f32) {
            return Self::Absent;
        }
        let clearance = (CONTROLS_CLEARANCE_POINTS * scale).ceil();
        Self::TopEdge {
            top_px: top as i32,
            clearance_px: clearance as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(top: f32) -> Option<egui::Rect> {
        Some(egui::Rect::from_min_max(
            egui::pos2(0.0, top),
            egui::pos2(800.0, top + 60.0),
        ))
    }

    #[test]
    fn hidden_panel_is_absent() {
        assert_eq!(
            BottomControlsObstruction::from_visible_rect(None, 2.0),
            BottomControlsObstruction::Absent
        );
    }

    #[test]
    fn visible_panel_converts_points_to_pixels_and_scales_the_clearance() {
        assert_eq!(
            BottomControlsObstruction::from_visible_rect(panel(100.25), 2.0),
            BottomControlsObstruction::TopEdge {
                top_px: 200,
                clearance_px: 16
            }
        );
        assert_eq!(
            BottomControlsObstruction::from_visible_rect(panel(100.0), 1.0),
            BottomControlsObstruction::TopEdge {
                top_px: 100,
                clearance_px: 8
            }
        );
    }

    #[test]
    fn broken_scale_or_rect_never_produces_garbage() {
        // Нечисловой масштаб заменяется единицей.
        assert_eq!(
            BottomControlsObstruction::from_visible_rect(panel(50.0), f32::NAN),
            BottomControlsObstruction::TopEdge {
                top_px: 50,
                clearance_px: 8
            }
        );
        assert_eq!(
            BottomControlsObstruction::from_visible_rect(panel(f32::NAN), 1.0),
            BottomControlsObstruction::Absent
        );
        assert_eq!(
            BottomControlsObstruction::from_visible_rect(panel(f32::INFINITY), 1.0),
            BottomControlsObstruction::Absent
        );
    }
}
