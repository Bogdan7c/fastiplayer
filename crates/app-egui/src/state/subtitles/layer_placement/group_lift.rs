//! Подъём основной группы ASS над панелью управления (режим `main_lines_avoid_controls`).
//!
//! Решение владельца (субтитры, сессия 08): основной диалог (ASS без `\pos`/`\move`, снизу)
//! плавно поднимается над панелью, пока она видна, и возвращается на авторское место при её
//! скрытии. Надписи и верхние реплики не двигаются. Сдвиг пользователя к ASS не применяется.
//!
//! Плавность даёт само препятствие: [`BottomControlsObstruction`] анимируется вместе с
//! панелью, а подъём равен текущему перекрытию, поэтому слой едет синхронно с панелью без
//! собственной анимации и без перерастеризации (меняется только uniform рендера).
//!
//! Правила (целые пиксели окна, как в [`super::place_layer`]):
//! 1. Подъём = перекрытие низа основной группы с панелью (с зазором), как у обычных субтитров.
//! 2. Основная группа поднимается ЦЕЛИКОМ одним сдвигом: взаимное положение строк сохраняется.
//! 3. Верх группы не выше картинки: на чёрные полосы и за экран ничего не выходит.
//! 4. Фиксированная группа не двигается; горизонтальный сдвиг всегда нулевой.

use render_wgpu_shell::{SubtitleLayerOffset, SubtitleLayerOffsets};
use subtitle_raster::CanvasRect;

use super::{
    BottomControlsObstruction, IntegerPicture, PictureRect, PlacedLayerRect, lift_above_controls,
};

/// Вход расчёта подъёма основной группы.
#[derive(Debug, Clone, Copy)]
pub(in crate::state::subtitles) struct MainGroupLiftRequest {
    /// Картинка видео без полос, пиксели окна.
    pub(in crate::state::subtitles) picture_rect: PictureRect,
    /// Границы основной группы на холсте; `None` — основных картинок нет (подъём не нужен).
    pub(in crate::state::subtitles) main_group_bounds: Option<CanvasRect>,
    /// Границы фиксированной группы на холсте; нужны только для итогового прямоугольника.
    pub(in crate::state::subtitles) fixed_group_bounds: Option<CanvasRect>,
    /// Что закрывает низ видео в этот момент.
    pub(in crate::state::subtitles) obstruction: BottomControlsObstruction,
}

/// Результат: сдвиги групп для рендера и итоговый прямоугольник всего слоя на экране.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::state::subtitles) struct MainGroupLift {
    /// Сдвиги групп (основная — вверх на подъём, фиксированная — ноль).
    pub(in crate::state::subtitles) offsets: SubtitleLayerOffsets,
    /// Объединение групп после сдвига, пиксели окна.
    pub(in crate::state::subtitles) rect: PlacedLayerRect,
}

/// Считает подъём основной группы; `None` — геометрия картинки нечисловая либо слой пуст
/// (тогда вызывающий не сдвигает ничего).
pub(in crate::state::subtitles) fn lift_main_group_above_controls(
    request: &MainGroupLiftRequest,
) -> Option<MainGroupLift> {
    let picture = IntegerPicture::from_rect(request.picture_rect)?;
    let lift = request.main_group_bounds.map_or(0, |main| {
        main_group_lift_pixels(main, &picture, request.obstruction)
    });
    let main_screen = request
        .main_group_bounds
        .map(|main| screen_rect(main, &picture, -lift));
    let fixed_screen = request
        .fixed_group_bounds
        .map(|fixed| screen_rect(fixed, &picture, 0));
    let rect = union_rects(main_screen, fixed_screen)?;
    Some(MainGroupLift {
        offsets: SubtitleLayerOffsets {
            main: SubtitleLayerOffset {
                x: 0.0,
                y: -(lift as f32),
            },
            fixed: SubtitleLayerOffset::NONE,
        },
        rect,
    })
}

/// Сколько пикселей поднять основную группу: перекрытие с панелью, но не выше верха картинки.
fn main_group_lift_pixels(
    main: CanvasRect,
    picture: &IntegerPicture,
    obstruction: BottomControlsObstruction,
) -> i64 {
    let default_top = picture.top + i64::from(main.y);
    let lifted_top = lift_above_controls(default_top, i64::from(main.height), obstruction);
    // Верх картинки важнее подъёма: группа не заезжает на верхнюю полосу и за экран.
    let allowed_top = lifted_top.max(picture.top).min(default_top);
    default_top - allowed_top
}

/// Прямоугольник группы на экране: холст лежит от угла картинки, плюс сдвиг по вертикали.
pub(super) fn screen_rect(
    bounds: CanvasRect,
    picture: &IntegerPicture,
    shift_y: i64,
) -> (i64, i64, i64, i64) {
    let left = picture.left + i64::from(bounds.x);
    let top = picture.top + i64::from(bounds.y) + shift_y;
    (
        left,
        top,
        left + i64::from(bounds.width),
        top + i64::from(bounds.height),
    )
}

/// Объединение двух необязательных прямоугольников `(left, top, right, bottom)`.
pub(super) fn union_rects(
    first: Option<(i64, i64, i64, i64)>,
    second: Option<(i64, i64, i64, i64)>,
) -> Option<PlacedLayerRect> {
    let (left, top, right, bottom) = match (first, second) {
        (Some(a), Some(b)) => (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => return None,
    };
    Some(PlacedLayerRect {
        x: i32::try_from(left).ok()?,
        y: i32::try_from(top).ok()?,
        width: u32::try_from(right - left).ok()?,
        height: u32::try_from(bottom - top).ok()?,
    })
}

#[cfg(test)]
mod tests;
