//! Чистая математика жестов: пиксели и точки экрана -> намерения изменения стиля.
//!
//! Без egui-контекста и состояния, поэтому каждая формула проверяется отдельным тестом.
//! Единицы: указатель и рамка - точки egui (points), картинка и слой владельца - физические
//! пиксели; переход - через `pixels_per_point`.

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use render_core::PictureRect;

use crate::state::{AppearanceEdit, PlacedLayerRect};

/// Прокрутка колеса в points на один шаг размера: «щелчок» мыши в egui - 40 points
/// (как шаг громкости колесом в `video_surface_input`).
pub(super) const WHEEL_POINTS_PER_SIZE_STEP: f32 = 40.0;

/// Во сколько раз меняется размер текста за один шаг колеса (+10 %).
pub(super) const WHEEL_SIZE_STEP_FACTOR: f32 = 1.1;

/// Ближе этого расстояния (points) до центра рамки угловой жест не считается: отношение
/// расстояний у самого центра взрывается от шума в доли точки.
const MIN_CORNER_DISTANCE_POINTS: f32 = 4.0;

/// Предел изменения размера за один кадр углового жеста: защита от скачка указателя
/// (потеря фокуса окна, телепорт курсора).
const MAX_FRAME_SCALE_FACTOR: f32 = 2.0;

/// Прямоугольник слоя в points с запасом `padding_points` со всех сторон.
///
/// `None`, если размер экрана нечисловой или нулевой.
pub(super) fn frame_rect_points(
    layer: PlacedLayerRect,
    pixels_per_point: f32,
    padding_points: f32,
) -> Option<Rect> {
    if !(pixels_per_point.is_finite() && pixels_per_point > 0.0) {
        return None;
    }
    let min = pos2(
        layer.x as f32 / pixels_per_point,
        layer.y as f32 / pixels_per_point,
    );
    let size = vec2(
        layer.width as f32 / pixels_per_point,
        layer.height as f32 / pixels_per_point,
    );
    let rect = Rect::from_min_size(min, size).expand(padding_points.max(0.0));
    (rect.is_positive() && rect.is_finite()).then_some(rect)
}

/// Перетаскивание слоя: сдвиг указателя -> `MoveBy` в долях КАРТИНКИ видео.
///
/// Вправо - плюс. Вертикаль владельца растёт ВВЕРХ, а экранная ось растёт вниз, поэтому знак
/// вертикали меняется здесь, на границе UI. `None`, если картинка вырожденная.
pub(super) fn move_edit(
    drag_delta_points: Vec2,
    pixels_per_point: f32,
    picture: PictureRect,
) -> Option<AppearanceEdit> {
    let valid = pixels_per_point.is_finite()
        && pixels_per_point > 0.0
        && picture.width.is_finite()
        && picture.height.is_finite()
        && picture.width > 0.0
        && picture.height > 0.0
        && drag_delta_points.is_finite();
    if !valid || drag_delta_points == Vec2::ZERO {
        return None;
    }
    Some(AppearanceEdit::MoveBy {
        horizontal_fraction: drag_delta_points.x * pixels_per_point / picture.width,
        vertical_fraction: -drag_delta_points.y * pixels_per_point / picture.height,
    })
}

/// Угловой жест: множитель размера пропорционален изменению расстояния от центра рамки.
///
/// Формула: `factor = |текущая точка - центр| / |прошлая точка - центр|`. Тянем угол
/// от центра - текст крупнее, к центру - мельче. Множители соседних кадров перемножаются,
/// так что итог за весь жест равен отношению конечного расстояния к начальному.
/// `None` - указатель не сдвинулся или слишком близко к центру.
pub(super) fn corner_scale_edit(
    frame_center: Pos2,
    previous_pointer: Pos2,
    current_pointer: Pos2,
) -> Option<AppearanceEdit> {
    let previous_distance = previous_pointer.distance(frame_center);
    let current_distance = current_pointer.distance(frame_center);
    let usable = previous_distance.is_finite()
        && current_distance.is_finite()
        && previous_distance >= MIN_CORNER_DISTANCE_POINTS
        && current_distance > 0.0;
    if !usable {
        return None;
    }
    let factor = (current_distance / previous_distance)
        .clamp(1.0 / MAX_FRAME_SCALE_FACTOR, MAX_FRAME_SCALE_FACTOR);
    ((factor - 1.0).abs() > f32::EPSILON).then_some(AppearanceEdit::ScaleTextBy { factor })
}

/// Колесо: прокрутка вверх (положительная) увеличивает текст, вниз - уменьшает.
///
/// `WHEEL_POINTS_PER_SIZE_STEP` points = множитель `WHEEL_SIZE_STEP_FACTOR`; для плавной
/// прокрутки тачпада степень непрерывная, остатка между кадрами копить не нужно.
pub(super) fn wheel_scale_edit(scroll_points: f32) -> Option<AppearanceEdit> {
    if !scroll_points.is_finite() || scroll_points == 0.0 {
        return None;
    }
    let factor = WHEEL_SIZE_STEP_FACTOR.powf(scroll_points / WHEEL_POINTS_PER_SIZE_STEP);
    Some(AppearanceEdit::ScaleTextBy { factor })
}
