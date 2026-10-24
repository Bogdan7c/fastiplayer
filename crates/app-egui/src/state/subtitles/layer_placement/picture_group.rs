//! Размещение растровых субтитров (PGS): нижние картинки следуют сдвигу пользователя и
//! поднимаются над панелью, остальные стоят там, где их поставил автор.
//!
//! Решение владельца (субтитры, сессия 11): масштаб пользователя к PGS не применяется
//! (растеризатор его игнорирует), горизонтальный сдвиг не применяется. Картинки основной
//! группы (верх картинки на отметке не выше половины опорной высоты - см.
//! `OverlayPlacementGroup::Main`) двигаются вертикально по сдвигу пользователя и поднимаются
//! над панелью управления так же плавно, как остальные субтитры. Картинки фиксированной группы
//! (надписи сверху и посередине) не двигаются никогда. Решение 12 README: PGS всегда привязан
//! к кадру - ни при каком сдвиге группа не выходит за картинку видео на чёрные полосы.
//!
//! Правила (целые пиксели окна, как у [`super::place_layer`]):
//! 1. Желаемый верх основной группы = авторский верх + сдвиг пользователя (вверх = плюс).
//! 2. Группа зажимается строго внутри картинки; выше картинки - центрируется по вертикали.
//! 3. Затем поднимается над панелью, если мешает (подъём - ровно перекрытие, плавность даёт
//!    анимация препятствия).
//! 4. Верх картинки важнее подъёма: группа не уходит выше картинки.
//! 5. Фиксированная группа и горизонталь: сдвиг ноль.

use render_shell_api::{SubtitleLayerOffset, SubtitleLayerOffsets};
use subtitle_raster::CanvasRect;

use super::group_lift::{MainGroupLift, screen_rect, union_rects};
use super::{
    BottomControlsObstruction, IntegerPicture, PictureRect, clamp_vertically_inside_picture,
    fraction_to_pixels, lift_above_controls,
};

/// Вход расчёта размещения PGS-слоя.
#[derive(Debug, Clone, Copy)]
pub(in crate::state::subtitles) struct PictureGroupPlacementRequest {
    /// Картинка видео без полос, пиксели окна.
    pub(in crate::state::subtitles) picture_rect: PictureRect,
    /// Границы основной группы на холсте; `None` - нижних картинок нет.
    pub(in crate::state::subtitles) main_group_bounds: Option<CanvasRect>,
    /// Границы фиксированной группы на холсте; нужны для итогового прямоугольника слоя.
    pub(in crate::state::subtitles) fixed_group_bounds: Option<CanvasRect>,
    /// Вертикальный сдвиг пользователя от места пресета в долях высоты картинки (вверх = плюс).
    pub(in crate::state::subtitles) vertical_offset_fraction: f32,
    /// Что закрывает низ видео в этот момент.
    pub(in crate::state::subtitles) obstruction: BottomControlsObstruction,
}

/// Считает сдвиги групп PGS-слоя; `None` - геометрия картинки нечисловая либо слой пуст
/// (вызывающий тогда не сдвигает ничего).
pub(in crate::state::subtitles) fn place_picture_groups(
    request: &PictureGroupPlacementRequest,
) -> Option<MainGroupLift> {
    let picture = IntegerPicture::from_rect(request.picture_rect)?;
    let main_shift = request
        .main_group_bounds
        .map_or(0, |main| main_group_shift_pixels(main, request, &picture));
    let main_screen = request
        .main_group_bounds
        .map(|main| screen_rect(main, &picture, main_shift));
    let fixed_screen = request
        .fixed_group_bounds
        .map(|fixed| screen_rect(fixed, &picture, 0));
    let rect = union_rects(main_screen, fixed_screen)?;
    Some(MainGroupLift {
        offsets: SubtitleLayerOffsets {
            main: SubtitleLayerOffset {
                x: 0.0,
                y: main_shift as f32,
            },
            fixed: SubtitleLayerOffset::NONE,
        },
        rect,
    })
}

/// Вертикальный сдвиг основной группы в пикселях (минус = вверх).
fn main_group_shift_pixels(
    main: CanvasRect,
    request: &PictureGroupPlacementRequest,
    picture: &IntegerPicture,
) -> i64 {
    let height = i64::from(main.height);
    let default_top = picture.top + i64::from(main.y);
    // Вверх по доле - это минус по экранной оси.
    let user_dy = -fraction_to_pixels(request.vertical_offset_fraction, picture.height);
    let inside_picture = clamp_vertically_inside_picture(default_top + user_dy, height, picture);
    let lifted = lift_above_controls(inside_picture, height, request.obstruction);
    // Верх картинки важнее подъёма: на чёрную полосу и за экран группа не заезжает.
    lifted.max(picture.top) - default_top
}

#[cfg(test)]
mod tests;
