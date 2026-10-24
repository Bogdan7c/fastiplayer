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
//!
//! # Слой ASS (решения владельца, субтитры, сессия 08)
//! Расчёт выбирается явным [`LayerPlacementPolicy`]:
//! - обычные субтитры (SRT/WebVTT) - пользовательское размещение с границей
//!   [`LayerBounds::PictureAndBars`] (правила 1-5 выше, поведение не менялось);
//! - ASS в режиме `whole_layer` - то же пользовательское размещение, но
//!   [`LayerBounds::PictureOnly`]: слой не выходит за картинку по ОБЕИМ осям;
//! - ASS в режиме `author` - [`LayerPlacementPolicy::AuthorPlaced`]: слой остаётся ровно там,
//!   где его нарисовал автор (нулевой сдвиг, без подъёма над панелью и сдвига пользователя);
//! - ASS в режиме `main_lines_avoid_controls` - [`LayerPlacementPolicy::LiftMainGroupOnly`]:
//!   над панелью поднимается только основная группа (диалог), считает [`group_lift`];
//!   `place_layer` для такого слоя возвращает нулевой сдвиг целого слоя;
//! - PGS - [`LayerPlacementPolicy::MainGroupUserOffsetInsidePicture`] (решение владельца,
//!   сессия 11): нижние картинки следуют вертикальному сдвигу пользователя и поднимаются над
//!   панелью, но не выходят за картинку видео; верхние/средние стоят на месте (см.
//!   [`picture_group`]).

use render_core::{PictureRect, RenderViewport};
use render_shell_api::SubtitleLayerOffset;
use subtitle_raster::CanvasRect;

use super::controls_obstruction::BottomControlsObstruction;

pub(super) mod group_lift;
pub(super) mod picture_group;

/// Где разрешено стоять слою, который двигает пользователь.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayerBounds {
    /// Слой остаётся в зажатой области видео: может стоять и на чёрной полосе (SRT/WebVTT).
    PictureAndBars,
    /// Слой остаётся строго внутри картинки видео по обеим осям (ASS, режим `whole_layer`).
    PictureOnly,
}

/// Кто решает, где стоит слой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayerPlacementPolicy {
    /// Слой двигает пользователь (позиция из стиля + подъём над панелью) в пределах `bounds`.
    UserPlaced { bounds: LayerBounds },
    /// Слой не двигается: стоит там, где его нарисовал автор (нулевой сдвиг).
    AuthorPlaced,
    /// Над панелью поднимается только основная группа (ASS-диалог), остальное стоит на месте;
    /// сдвиг пользователя не применяется. Группы считает [`group_lift`], а не `place_layer`.
    LiftMainGroupOnly,
    /// Растровые субтитры (PGS): основная группа (нижние картинки) следует вертикальному сдвигу
    /// пользователя внутри картинки видео и поднимается над панелью, фиксированная группа
    /// (верхние/средние надписи) стоит на авторском месте, горизонтальный сдвиг и масштаб не
    /// применяются. Группы считает [`picture_group`], а не `place_layer`.
    MainGroupUserOffsetInsidePicture,
}

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
    /// Кто и в каких границах двигает слой.
    pub(super) policy: LayerPlacementPolicy,
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
    let default_left = picture.left + i64::from(bounds.x);
    let default_top = picture.top + i64::from(bounds.y);

    let (left, top) = match request.policy {
        // Автор решил, где слой: ни пользователь, ни панель его не двигают.
        LayerPlacementPolicy::AuthorPlaced => (default_left, default_top),
        // Целый слой не двигается; подъём группы считает `group_lift` (см. владельца).
        LayerPlacementPolicy::LiftMainGroupOnly
        | LayerPlacementPolicy::MainGroupUserOffsetInsidePicture => (default_left, default_top),
        LayerPlacementPolicy::UserPlaced {
            bounds: layer_bounds,
        } => user_placed_corner(request, &picture, layer_bounds, default_left, default_top),
    };

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

/// Левый верхний угол слоя, который двигает пользователь (сдвиг из стиля, затем подъём над
/// панелью управления) в границах `bounds`.
fn user_placed_corner(
    request: &LayerPlacementRequest,
    picture: &IntegerPicture,
    bounds: LayerBounds,
    default_left: i64,
    default_top: i64,
) -> (i64, i64) {
    let width = i64::from(request.content_bounds.width);
    let height = i64::from(request.content_bounds.height);
    let desired_dx = fraction_to_pixels(request.horizontal_offset_fraction, picture.width);
    // Вверх по доле - это минус по экранной оси.
    let desired_dy = -fraction_to_pixels(request.vertical_offset_fraction, picture.height);

    let left = clamp_horizontally(default_left + desired_dx, width, picture);
    let desired_top = default_top + desired_dy;
    // Верхняя граница, ниже которой слой не поднимается, и способ зажать вертикаль.
    let (mut top, highest_top) = match bounds {
        LayerBounds::PictureAndBars => (
            clamp_vertically(desired_top, height, request.viewport),
            i64::from(request.viewport.y),
        ),
        LayerBounds::PictureOnly => (
            clamp_vertically_inside_picture(desired_top, height, picture),
            picture.top,
        ),
    };

    top = lift_above_controls(top, height, request.obstruction);
    // Верх допустимой области важнее подъёма: слой не уходит за верхний край.
    (left, top.max(highest_top))
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

/// Верхний край слоя строго внутри картинки; слой выше картинки центрируется (как по горизонтали).
fn clamp_vertically_inside_picture(
    desired_top: i64,
    layer_height: i64,
    picture: &IntegerPicture,
) -> i64 {
    if layer_height >= picture.height {
        return picture.top + (picture.height - layer_height).div_euclid(2);
    }
    desired_top.clamp(picture.top, picture.top + picture.height - layer_height)
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
