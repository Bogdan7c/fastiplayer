//! Чистое размещение слоя субтитров: куда сдвинуть готовый overlay на экране.
//!
//! Слой (объединение всех картинок overlay) едет целиком одним сдвигом на GPU, поэтому
//! позиция, подъём над панелью и выход на чёрные полосы НЕ требуют перерастеризации.
//!
//! # Правила (решения владельца, субтитры, сессия 05)
//! Весь расчёт идёт в целых физических пикселях окна (никаких накопленных дробных ошибок):
//! 1. Желаемый сдвиг: `dx = h_frac * ширина_картинки`, `dy = -v_frac * высота_картинки`
//!    (`v_frac` растёт вверх, экранная ось `y` растёт вниз).
//! 2. Горизонталь: слой не выходит за картинку; если он шире картинки - центрируется.
//! 3. Вертикаль: слой остаётся внутри зажатой области видео. Чёрные полосы - её часть,
//!    поэтому слой может стоять под картинкой.
//! 4. Подъём: только если слой мешает панели управления - его низ ниже
//!    `верх_панели - зазор`; тогда слой поднимается ровно на разницу. Высоко стоящий слой
//!    не двигается. Частично выехавшая панель даёт частичный подъём (плавность).
//! 5. Верх не выше области видео (верх важнее подъёма: слой не должен уйти за экран).
//!
//! Стиль по умолчанию без панели даёт сдвиг ровно `(0, 0)`: показ побитно прежний.

use render_core::{PictureRect, RenderViewport};
use render_wgpu_shell::SubtitleLayerOffset;
use subtitle_raster::CanvasRect;

use super::controls_obstruction::BottomControlsObstruction;

/// Всё, что нужно для расчёта размещения одного кадра.
#[derive(Debug, Clone, Copy)]
pub(super) struct LayerPlacementRequest {
    /// Картинка видео без полос, пиксели окна (как её рисует рендер).
    pub(super) picture_rect: PictureRect,
    /// Зажатая область видео, пиксели окна: в ней рисуются субтитры.
    pub(super) viewport: RenderViewport,
    /// Объединение картинок overlay на холсте (холст начинается в углу картинки видео).
    pub(super) content_bounds: CanvasRect,
    /// Сдвиг центра слоя от середины картинки в долях ширины (вправо = плюс).
    pub(super) horizontal_offset_fraction: f32,
    /// Сдвиг низа слоя от места пресета в долях высоты картинки (вверх = плюс).
    pub(super) vertical_offset_fraction: f32,
    /// Что закрывает низ видео в этот момент.
    pub(super) obstruction: BottomControlsObstruction,
}

/// Прямоугольник слоя на экране после размещения, пиксели окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlacedLayerRect {
    /// Левый край.
    pub(crate) x: i32,
    /// Верхний край.
    pub(crate) y: i32,
    /// Ширина.
    pub(crate) width: u32,
    /// Высота.
    pub(crate) height: u32,
}

/// Результат размещения: сдвиг для рендера и итоговый прямоугольник слоя.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LayerPlacement {
    /// Сдвиг относительно стандартного места (пиксели окна, целые значения).
    pub(super) offset: SubtitleLayerOffset,
    /// Где слой окажется на экране.
    pub(super) rect: PlacedLayerRect,
}

/// Рассчитывает размещение; `None`, если геометрия картинки нечисловая или пустая
/// (тогда вызывающий не сдвигает слой).
pub(super) fn place_layer(request: &LayerPlacementRequest) -> Option<LayerPlacement> {
    let picture = IntegerPicture::from_rect(request.picture_rect)?;
    // Стандартное положение слоя: холст лежит от округлённого угла картинки.
    let bounds = request.content_bounds;
    let width = i64::from(bounds.width);
    let height = i64::from(bounds.height);
    let default_left = picture.left + i64::from(bounds.x);
    let default_top = picture.top + i64::from(bounds.y);

    let desired_dx = fraction_to_pixels(request.horizontal_offset_fraction, picture.width);
    // Вверх по доле - это минус по экранной оси.
    let desired_dy = -fraction_to_pixels(request.vertical_offset_fraction, picture.height);

    let left = clamp_horizontally(default_left + desired_dx, width, &picture);
    let mut top = clamp_vertically(default_top + desired_dy, height, request.viewport);

    top = lift_above_controls(top, height, request.obstruction);
    // Верх области видео важнее подъёма: слой не уходит за верхний край.
    top = top.max(i64::from(request.viewport.y));

    let offset = SubtitleLayerOffset {
        x: (left - default_left) as f32,
        y: (top - default_top) as f32,
    };
    let rect = PlacedLayerRect {
        x: i32::try_from(left).ok()?,
        y: i32::try_from(top).ok()?,
        width: bounds.width,
        height: bounds.height,
    };
    Some(LayerPlacement { offset, rect })
}

/// Картинка видео в целых пикселях: угол и размер совпадают с тем, что получает рендер
/// (угол округляется так же, как `placement_origin`, размер - как холст растеризатора).
struct IntegerPicture {
    left: i64,
    top: i64,
    width: i64,
    height: i64,
}

impl IntegerPicture {
    /// `None` для нечисловых или нулевых размеров.
    fn from_rect(rect: PictureRect) -> Option<Self> {
        let all_finite = [rect.x, rect.y, rect.width, rect.height]
            .iter()
            .all(|value| value.is_finite());
        if !all_finite || rect.width.round() < 1.0 || rect.height.round() < 1.0 {
            return None;
        }
        Some(Self {
            left: rect.x.round() as i64,
            top: rect.y.round() as i64,
            width: rect.width.round() as i64,
            height: rect.height.round() as i64,
        })
    }
}

/// Доля размера в целых пикселях; нечисловая доля не двигает слой.
fn fraction_to_pixels(fraction: f32, size: i64) -> i64 {
    if !fraction.is_finite() {
        return 0;
    }
    (f64::from(fraction) * size as f64).round() as i64
}

/// Левый край слоя внутри картинки; слой шире картинки центрируется.
fn clamp_horizontally(desired_left: i64, layer_width: i64, picture: &IntegerPicture) -> i64 {
    if layer_width >= picture.width {
        // Не влезает: центр слоя совпадает с центром картинки (края выходят поровну).
        return picture.left + (picture.width - layer_width).div_euclid(2);
    }
    desired_left.clamp(picture.left, picture.left + picture.width - layer_width)
}

/// Верхний край слоя внутри области видео (чёрные полосы - часть области).
fn clamp_vertically(desired_top: i64, layer_height: i64, viewport: RenderViewport) -> i64 {
    let viewport_top = i64::from(viewport.y);
    let lowest_top = viewport_top + i64::from(viewport.height) - layer_height;
    // Слой выше области: верх прижимается к верху области (нижняя граница `max` не опасна).
    desired_top.min(lowest_top).max(viewport_top)
}

/// Поднимает слой над панелью управления, только если он ей мешает.
fn lift_above_controls(top: i64, layer_height: i64, obstruction: BottomControlsObstruction) -> i64 {
    match obstruction {
        BottomControlsObstruction::Absent => top,
        BottomControlsObstruction::TopEdge {
            top_px,
            clearance_px,
        } => {
            let lowest_allowed_bottom = i64::from(top_px) - i64::from(clearance_px);
            let overlap = top + layer_height - lowest_allowed_bottom;
            // Не мешает - не двигаем; мешает - поднимаем ровно на перекрытие.
            if overlap > 0 { top - overlap } else { top }
        }
    }
}

#[cfg(test)]
mod tests;
