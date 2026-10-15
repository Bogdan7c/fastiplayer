//! Тесты обводчика: форма результата проверяется по растеризованной маске.

use super::{StrokeError, StrokeParameters, stroke_outline};
use crate::bitmap::{CoverageBitmap, outline_to_bitmap};
use crate::outline::{OUTLINE_MAX, Outline, OutlineError, Point, SegmentKind};
use crate::raster::Rasterizer;

/// Допустимая ошибка обводки в единицах контура (libass: `STROKER_PRECISION`).
const PRECISION: i32 = 16;

fn d6(pixels: f64) -> i32 {
    (pixels * 64.0).round() as i32
}

fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Outline {
    let mut outline = Outline::new();
    outline.add_rect(d6(x0), d6(y0), d6(x1), d6(y1)).unwrap();
    outline
}

/// Круг радиуса `r` четырьмя кубическими дугами.
fn circle(cx: f64, cy: f64, r: f64) -> Outline {
    let k = 0.552_284_7 * r;
    let points = [
        (cx + r, cy),
        (cx + r, cy + k),
        (cx + k, cy + r),
        (cx, cy + r),
        (cx - k, cy + r),
        (cx - r, cy + k),
        (cx - r, cy),
        (cx - r, cy - k),
        (cx - k, cy - r),
        (cx, cy - r),
        (cx + k, cy - r),
        (cx + r, cy - k),
    ];
    let mut outline = Outline::new();
    for chunk in points.chunks(3) {
        for (x, y) in chunk {
            outline.push_point(Point::new(d6(*x), d6(*y))).unwrap();
        }
        outline.push_segment(SegmentKind::Cubic);
    }
    assert!(outline.close_contour());
    outline
}

fn params(border_x_px: f64, border_y_px: f64) -> StrokeParameters {
    StrokeParameters {
        border_x: d6(border_x_px),
        border_y: d6(border_y_px),
        precision: PRECISION,
    }
}

/// Растеризует обе границы обводки вместе (как libass: основная и дополнительная группа).
fn stroke_mask(path: &Outline, parameters: StrokeParameters) -> CoverageBitmap {
    let stroked = stroke_outline(path, parameters).expect("stroke must succeed");
    let mut rasterizer = Rasterizer::new(16);
    outline_to_bitmap(
        &mut rasterizer,
        Some(&stroked.positive),
        Some(&stroked.negative),
    )
    .expect("rasterization must succeed")
    .expect("stroke is not empty")
}

/// Пиксель равен ожидаемому с допуском `tolerance` (см. ниже: растеризатор libass на границах
/// вертикальных/горизонтальных рёбер даёт 252..254 вместо 255, это поведение оригинала).
fn assert_near(actual: u8, expected: u8, what: &str) {
    assert!(
        actual.abs_diff(expected) <= 6,
        "{what}: expected about {expected}, got {actual}"
    );
}

fn value_at(bitmap: &CoverageBitmap, x: i32, y: i32) -> u8 {
    // Вне битмапа покрытия нет.
    let column = usize::try_from(x - bitmap.left).ok();
    bitmap
        .row(y - bitmap.top)
        .and_then(|row| column.and_then(|column| row.get(column)))
        .copied()
        .unwrap_or(0)
}

/// Площадь маски в пикселях.
fn area(bitmap: &CoverageBitmap) -> f64 {
    bitmap
        .buffer
        .iter()
        .map(|&value| f64::from(value))
        .sum::<f64>()
        / 255.0
}

#[test]
fn stroking_a_square_grows_it_by_the_border_width_on_every_side() {
    let path = square(20.0, 20.0, 60.0, 50.0);
    let mask = stroke_mask(&path, params(3.0, 3.0));
    // Середины сторон: ровно b пикселей наружу заполнены, дальше пусто (допуск ±6 из-за
    // неточности заливки на целых рёбрах в libass).
    assert_near(value_at(&mask, 17, 35), 255, "3 px left of the left side");
    assert_near(value_at(&mask, 16, 35), 0, "4 px left of the left side");
    assert_near(value_at(&mask, 62, 35), 255, "3 px right of the right side");
    assert_near(value_at(&mask, 63, 35), 0, "pixel (63, 35)");
    assert_near(value_at(&mask, 40, 17), 255, "3 px above the top side");
    assert_near(value_at(&mask, 40, 16), 0, "pixel (40, 16)");
    assert_near(value_at(&mask, 40, 52), 255, "3 px below the bottom side");
    assert_near(value_at(&mask, 40, 53), 0, "pixel (40, 53)");
    // Внутри квадрата всё залито (граница обеих групп покрывает исходную фигуру).
    assert_near(value_at(&mask, 40, 35), 255, "pixel (40, 35)");
}

#[test]
fn stroking_a_square_rounds_the_outer_corners() {
    let path = square(20.0, 20.0, 60.0, 50.0);
    let mask = stroke_mask(&path, params(4.0, 4.0));
    // Угловая клетка квадрата-расширения лежит вне круга радиуса 4 вокруг угла.
    assert!(
        value_at(&mask, 16, 16) < 40,
        "corner cell is not a square corner: {}",
        value_at(&mask, 16, 16)
    );
    // А клетка в 1 px от угла по диагонали уже внутри круга.
    assert_near(value_at(&mask, 18, 18), 255, "pixel (18, 18)");
    // Площадь = исходная + периметр * b + круг: 1200 + 140 * 4 + pi * 16.
    let expected = 40.0 * 30.0 + 140.0 * 4.0 + std::f64::consts::PI * 16.0;
    assert!(
        (area(&mask) - expected).abs() / expected < 0.01,
        "{} vs {expected}",
        area(&mask)
    );
}

#[test]
fn elliptic_border_uses_independent_horizontal_and_vertical_widths() {
    let path = square(30.0, 30.0, 70.0, 60.0);
    let mask = stroke_mask(&path, params(2.0, 6.0));
    assert_near(value_at(&mask, 28, 45), 255, "pixel (28, 45)");
    assert_near(value_at(&mask, 27, 45), 0, "horizontal reach is 2 px");
    assert_near(value_at(&mask, 50, 24), 255, "pixel (50, 24)");
    assert_near(value_at(&mask, 50, 23), 0, "vertical reach is 6 px");
}

#[test]
fn stroking_a_cubic_circle_gives_the_enlarged_disk() {
    let path = circle(48.0, 48.0, 20.0);
    let mask = stroke_mask(&path, params(3.0, 3.0));
    let expected = std::f64::consts::PI * 23.0 * 23.0;
    assert!(
        (area(&mask) - expected).abs() / expected < 0.01,
        "{} vs {expected}",
        area(&mask)
    );
    assert_near(value_at(&mask, 48, 48), 255, "pixel (48, 48)");
    assert_near(
        value_at(&mask, 69, 48),
        255,
        "21.5 px from the center is inside radius 23",
    );
    assert_near(
        value_at(&mask, 72, 48),
        0,
        "24 px from the center is outside radius 23",
    );
}

#[test]
fn a_single_point_contour_becomes_a_round_dot() {
    let mut path = Outline::new();
    for _ in 0..3 {
        path.push_point(Point::new(d6(30.0), d6(30.0))).unwrap();
        path.push_segment(SegmentKind::Line);
    }
    assert!(path.close_contour());
    let mask = stroke_mask(&path, params(5.0, 5.0));
    let expected = std::f64::consts::PI * 25.0;
    assert!(
        (area(&mask) - expected).abs() / expected < 0.03,
        "{} vs {expected}",
        area(&mask)
    );
    assert_near(value_at(&mask, 30, 30), 255, "pixel (30, 30)");
    assert_near(value_at(&mask, 36, 30), 0, "pixel (36, 30)");
}

#[test]
fn stroke_borders_are_valid_closed_outlines_and_the_source_is_untouched() {
    let path = circle(48.0, 48.0, 20.0);
    let snapshot = path.clone();
    let stroked = stroke_outline(&path, params(3.0, 3.0)).unwrap();
    assert_eq!(path, snapshot);
    stroked.positive.validate().unwrap();
    stroked.negative.validate().unwrap();
    assert!(stroked.positive.is_closed() && stroked.negative.is_closed());
    assert!(!stroked.positive.is_empty() && !stroked.negative.is_empty());
}

#[test]
fn empty_path_gives_empty_borders() {
    let stroked = stroke_outline(&Outline::new(), params(3.0, 3.0)).unwrap();
    assert!(stroked.positive.is_empty() && stroked.negative.is_empty());
}

#[test]
fn invalid_parameters_and_malformed_paths_are_errors() {
    let path = square(0.0, 0.0, 10.0, 10.0);
    let with = |border_x, border_y, precision| {
        stroke_outline(
            &path,
            StrokeParameters {
                border_x,
                border_y,
                precision,
            },
        )
    };
    assert_eq!(
        with(64, 64, 0).err(),
        Some(StrokeError::InvalidParameters),
        "zero precision"
    );
    assert_eq!(
        with(8, 8, 16).err(),
        Some(StrokeError::InvalidParameters),
        "border below precision"
    );
    assert_eq!(
        with(-64, 64, 16).err(),
        Some(StrokeError::InvalidParameters),
        "negative border"
    );
    assert_eq!(
        with(OUTLINE_MAX + 1, 64, 16).err(),
        Some(StrokeError::InvalidParameters)
    );
    assert!(
        with(64, 16, 16).is_ok(),
        "only the larger border has to reach the precision"
    );

    let mut malformed = Outline::new();
    malformed.push_point(Point::new(0, 0)).unwrap();
    malformed.push_segment(SegmentKind::Quadratic);
    assert_eq!(
        stroke_outline(&malformed, params(3.0, 3.0)).err(),
        Some(StrokeError::Outline(OutlineError::Malformed))
    );
}

#[test]
fn border_that_leaves_the_coordinate_range_is_an_error_not_a_panic() {
    let mut path = Outline::new();
    path.add_rect(0, 0, OUTLINE_MAX - 10, OUTLINE_MAX - 10)
        .unwrap();
    let result = stroke_outline(
        &path,
        StrokeParameters {
            border_x: 4096,
            border_y: 4096,
            precision: 16,
        },
    );
    assert_eq!(
        result.err(),
        Some(StrokeError::Outline(OutlineError::PointOutOfRange))
    );
}
