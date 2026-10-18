//! Геометрия авторских картинок: перевод координат автора в пиксели холста.
//!
//! Автор (PGS) задаёт позиции в своей системе координат ([`PictureReferenceSize`], например
//! 1920x1080), а холст - это картинка видео на экране (другого размера). Пользовательский
//! масштаб и сдвиг сюда не входят: картинка занимает то место, которое выбрал автор.

use subtitle_core::{PicturePosition, PictureReferenceSize};

use crate::overlay::OverlayPlacementGroup;

/// Прямоугольник в пикселях холста (правый и нижний края не включены). Может выходить за
/// холст: обрезку делает [`VisibleWindow::within_canvas`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DestinationRect {
    pub(crate) left: i64,
    pub(crate) top: i64,
    pub(crate) right: i64,
    pub(crate) bottom: i64,
}

impl DestinationRect {
    /// Ширина в пикселях холста.
    pub(crate) fn width(&self) -> i64 {
        self.right - self.left
    }

    /// Высота в пикселях холста.
    pub(crate) fn height(&self) -> i64 {
        self.bottom - self.top
    }
}

/// Размер стороны холста в пикселях по осям.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CanvasSides {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Как система координат автора вписывается в холст: ОДИН масштаб на обе оси + центрирование.
///
/// Почему не отдельные масштабы по осям: холст автора PGS почти всегда 1920x1080 (кадр
/// Blu-ray вместе с чёрными полосами), а в рипах полосы обычно срезаны (видео 1920x800) или
/// видео вообще другого размера. Растяжение по осям порознь сплющило бы буквы. Поэтому, как
/// mpv по умолчанию, холст автора вписывается в картинку видео с сохранением пропорций
/// («letterbox» внутри кадра): масштаб = min(canvas_w / ref_w, canvas_h / ref_h), остаток по
/// второй оси делится поровну с двух сторон. Картинка при этом никогда не выходит из кадра.
///
/// Масштаб хранится дробью `numerator / denominator` (стороны холста и автора по
/// ограничивающей оси), чтобы считать края точно в целых числах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PictureFit {
    numerator: u64,
    denominator: u64,
    canvas: CanvasSides,
    reference_width: u64,
    reference_height: u64,
}

impl PictureFit {
    /// Вписывание холста автора `reference` в холст `canvas`.
    pub(crate) fn new(reference: PictureReferenceSize, canvas: CanvasSides) -> Self {
        let reference_width = u64::from(reference.width().get());
        let reference_height = u64::from(reference.height().get());
        let canvas_width = u64::from(canvas.width);
        let canvas_height = u64::from(canvas.height);
        // Сравнение дробей canvas_w/ref_w <= canvas_h/ref_h без деления.
        let width_limits = canvas_width * reference_height <= canvas_height * reference_width;
        let (numerator, denominator) = if width_limits {
            (canvas_width, reference_width)
        } else {
            (canvas_height, reference_height)
        };
        Self {
            numerator,
            denominator,
            canvas,
            reference_width,
            reference_height,
        }
    }

    /// Горизонтальный край из системы автора в пиксели холста.
    pub(crate) fn horizontal_edge(&self, edge: u64) -> i64 {
        self.scale_edge(edge, u64::from(self.canvas.width), self.reference_width)
    }

    /// Вертикальный край из системы автора в пиксели холста.
    pub(crate) fn vertical_edge(&self, edge: u64) -> i64 {
        self.scale_edge(edge, u64::from(self.canvas.height), self.reference_height)
    }

    /// Переводит КРАЙ (координату границы между пикселями) по одной оси:
    /// `round_half_up(edge * s + (canvas_side - reference_side * s) / 2)`, где `s` — общий
    /// масштаб. В целых числах: `floor((2*edge*n + canvas_side*d - reference_side*n + d) / (2*d))`.
    ///
    /// Правило округления одно на все края: правый край объекта и левый край соседнего объекта,
    /// стоящего вплотную в системе автора, попадают в ту же точку холста. Поэтому соседние
    /// объекты не расходятся щелью и не перекрываются: ошибка каждого края не больше половины
    /// пикселя, а ширина объекта (разность краёв) отличается от идеальной меньше чем на 1 пиксель.
    fn scale_edge(&self, edge: u64, canvas_side: u64, reference_side: u64) -> i64 {
        let numerator = u128::from(self.numerator);
        let denominator = u128::from(self.denominator);
        // Свободное место по оси (в единицах 1/d пикселя); по ограничивающей оси оно 0, по
        // второй — неотрицательно по построению масштаба. `saturating_sub` — страховка.
        let free_space = (u128::from(canvas_side) * denominator)
            .saturating_sub(u128::from(reference_side) * numerator);
        let scaled =
            (2 * u128::from(edge) * numerator + free_space + denominator) / (2 * denominator);
        i64::try_from(scaled).unwrap_or(i64::MAX)
    }
}

/// Куда на холсте попадает объект: левый верхний угол `origin` и размер `width x height`
/// заданы в системе координат автора, все четыре края считаются через [`PictureFit`].
pub(crate) fn destination_rect(
    origin: PicturePosition,
    width: u32,
    height: u32,
    reference: PictureReferenceSize,
    canvas: CanvasSides,
) -> DestinationRect {
    let fit = PictureFit::new(reference, canvas);
    let right_edge = u64::from(origin.x) + u64::from(width);
    let bottom_edge = u64::from(origin.y) + u64::from(height);
    DestinationRect {
        left: fit.horizontal_edge(u64::from(origin.x)),
        top: fit.vertical_edge(u64::from(origin.y)),
        right: fit.horizontal_edge(right_edge),
        bottom: fit.vertical_edge(bottom_edge),
    }
}

/// Часть [`DestinationRect`], которая лежит внутри холста.
///
/// `skip_*` - сколько пикселей прямоугольника слева и сверху обрезано (их не нужно считать),
/// `width/height` - размер видимой части, `canvas_*` - её положение на холсте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleWindow {
    pub(crate) skip_x: u32,
    pub(crate) skip_y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) canvas_x: u32,
    pub(crate) canvas_y: u32,
}

impl VisibleWindow {
    /// Пересечение прямоугольника с холстом; `None`, если пересечение пусто.
    pub(crate) fn within_canvas(rect: DestinationRect, canvas: CanvasSides) -> Option<Self> {
        let left = rect.left.max(0);
        let top = rect.top.max(0);
        let right = rect.right.min(i64::from(canvas.width));
        let bottom = rect.bottom.min(i64::from(canvas.height));
        if left >= right || top >= bottom {
            return None;
        }
        Some(Self {
            skip_x: u32::try_from(left - rect.left).ok()?,
            skip_y: u32::try_from(top - rect.top).ok()?,
            width: u32::try_from(right - left).ok()?,
            height: u32::try_from(bottom - top).ok()?,
            canvas_x: u32::try_from(left).ok()?,
            canvas_y: u32::try_from(top).ok()?,
        })
    }
}

/// Группа размещения картинки по её положению в системе автора.
///
/// Правило: если верхний край картинки лежит на половине высоты кадра автора или ниже
/// (`2 * origin_y >= reference_height`), это диалог внизу кадра - [`OverlayPlacementGroup::Main`]
/// (показ поднимает его над панелью управления и двигает по вертикальному сдвигу
/// пользователя). Остальное (надписи сверху и посередине) привязано к месту кадра -
/// [`OverlayPlacementGroup::Fixed`].
pub(crate) fn picture_placement_group(
    origin_y: u32,
    reference: PictureReferenceSize,
) -> OverlayPlacementGroup {
    if u64::from(origin_y) * 2 >= u64::from(reference.height().get()) {
        OverlayPlacementGroup::Main
    } else {
        OverlayPlacementGroup::Fixed
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn reference(width: u32, height: u32) -> PictureReferenceSize {
        PictureReferenceSize::new(
            NonZeroU32::new(width).expect("test width is non-zero"),
            NonZeroU32::new(height).expect("test height is non-zero"),
        )
    }

    fn sides(width: u32, height: u32) -> CanvasSides {
        CanvasSides { width, height }
    }

    #[test]
    fn edges_round_half_up_and_are_identity_at_scale_one() {
        let identity = PictureFit::new(reference(1920, 1080), sides(1920, 1080));
        assert_eq!(identity.horizontal_edge(7), 7);
        assert_eq!(identity.vertical_edge(1079), 1079);
        // Масштаб 1/2: 3 * 1/2 = 1.5 -> 2 (половина округляется вверх).
        let half = PictureFit::new(reference(2, 2), sides(1, 1));
        assert_eq!(half.horizontal_edge(3), 2);
        assert_eq!(half.horizontal_edge(1), 1);
    }

    #[test]
    fn cropped_video_keeps_aspect_and_centres_the_author_frame() {
        // Рип Blu-ray со срезанными полосами: PGS 1920x1080, видео 1920x800. Масштаб
        // ограничен высотой: 800/1080; ширина вписанного кадра 1422.2, по бокам по 248.9.
        let fit = PictureFit::new(reference(1920, 1080), sides(1920, 800));
        assert_eq!(fit.vertical_edge(0), 0);
        assert_eq!(fit.vertical_edge(1080), 800);
        assert_eq!(fit.horizontal_edge(0), 249);
        assert_eq!(fit.horizontal_edge(1920), 1671);
        // Квадрат автора остаётся квадратом (буквы не сплющены).
        let square = destination_rect(
            PicturePosition { x: 960, y: 540 },
            108,
            108,
            reference(1920, 1080),
            sides(1920, 800),
        );
        assert_eq!(square.width(), square.height());
        assert_eq!(square.width(), 80);
    }

    #[test]
    fn wider_reference_is_centred_vertically() {
        // Холст автора шире видео по пропорциям: ограничивает ширина, остаток по высоте делится
        // поровну. 1920x800 в 1280x720: масштаб 2/3, высота 533.3, сверху и снизу по 93.3.
        let fit = PictureFit::new(reference(1920, 800), sides(1280, 720));
        assert_eq!(fit.horizontal_edge(1920), 1280);
        assert_eq!(fit.vertical_edge(0), 93);
        assert_eq!(fit.vertical_edge(800), 627);
    }

    #[test]
    fn adjacent_objects_share_the_edge_without_gap_or_overlap() {
        let canvas = CanvasSides {
            width: 1000,
            height: 563,
        };
        let reference = reference(1920, 1080);
        // Два объекта вплотную в системе автора: [100, 333) и [333, 700).
        let first = destination_rect(PicturePosition { x: 100, y: 0 }, 233, 10, reference, canvas);
        let second = destination_rect(PicturePosition { x: 333, y: 0 }, 367, 10, reference, canvas);
        assert_eq!(first.right, second.left);
    }

    #[test]
    fn destination_halves_position_and_size_when_canvas_is_half() {
        let rect = destination_rect(
            PicturePosition { x: 400, y: 800 },
            600,
            100,
            reference(1920, 1080),
            CanvasSides {
                width: 960,
                height: 540,
            },
        );
        assert_eq!(
            rect,
            DestinationRect {
                left: 200,
                top: 400,
                right: 500,
                bottom: 450
            }
        );
    }

    #[test]
    fn visible_window_clips_all_sides_and_reports_skipped_pixels() {
        let canvas = CanvasSides {
            width: 100,
            height: 50,
        };
        let rect = DestinationRect {
            left: -10,
            top: -5,
            right: 120,
            bottom: 20,
        };
        assert_eq!(
            VisibleWindow::within_canvas(rect, canvas),
            Some(VisibleWindow {
                skip_x: 10,
                skip_y: 5,
                width: 100,
                height: 20,
                canvas_x: 0,
                canvas_y: 0,
            })
        );
        let outside = DestinationRect {
            left: 100,
            top: 0,
            right: 110,
            bottom: 10,
        };
        assert_eq!(VisibleWindow::within_canvas(outside, canvas), None);
    }

    #[test]
    fn placement_group_splits_at_half_of_the_reference_height() {
        let reference = reference(1920, 1080);
        assert_eq!(
            picture_placement_group(540, reference),
            OverlayPlacementGroup::Main
        );
        assert_eq!(
            picture_placement_group(539, reference),
            OverlayPlacementGroup::Fixed
        );
        assert_eq!(
            picture_placement_group(0, reference),
            OverlayPlacementGroup::Fixed
        );
        assert_eq!(
            picture_placement_group(900, reference),
            OverlayPlacementGroup::Main
        );
    }
}
