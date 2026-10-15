//! Тесты контура, преобразований и адаптера пера skrifa.

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

use super::{ControlBox, OUTLINE_MAX, Outline, OutlineError, Point, SegmentKind};
use crate::bitmap::outline_to_bitmap;
use crate::fonts::EMBEDDED_FONTS;
use crate::outline::pen::GlyphOutlinePen;
use crate::raster::Rasterizer;

/// Квадрат в целых пикселях (координаты 26.6).
fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Outline {
    let mut outline = Outline::new();
    outline
        .add_rect(x0 * 64, y0 * 64, x1 * 64, y1 * 64)
        .unwrap();
    outline
}

#[test]
fn rect_walks_as_four_lines_closing_to_the_first_corner() {
    let outline = rect(1, 2, 5, 7);
    let views: Vec<_> = outline.segments_view().map(Result::unwrap).collect();
    assert_eq!(views.len(), 4);
    assert!(views[0].starts_contour && !views[0].ends_contour);
    assert!(views[3].ends_contour && !views[3].starts_contour);
    // Обход (x0,y0) -> (x1,y0) -> (x1,y1) -> (x0,y1) -> назад в (x0,y0).
    let starts: Vec<Point> = views.iter().map(|view| view.owned[0]).collect();
    assert_eq!(
        starts,
        [
            Point::new(64, 128),
            Point::new(320, 128),
            Point::new(320, 448),
            Point::new(64, 448)
        ]
    );
    assert_eq!(views[3].end, Point::new(64, 128));
    assert_eq!(views[0].end, Point::new(320, 128));
}

#[test]
fn add_rect_refuses_to_start_inside_an_open_contour() {
    let mut outline = Outline::new();
    outline.push_point(Point::new(0, 0)).unwrap();
    outline.push_segment(SegmentKind::Line);
    assert_eq!(outline.add_rect(0, 0, 64, 64), Err(OutlineError::Malformed));
    // Состояние не испорчено отказом.
    assert_eq!(outline.points().len(), 1);
}

#[test]
fn points_outside_the_coordinate_range_are_rejected() {
    let mut outline = Outline::new();
    assert!(
        outline
            .push_point(Point::new(OUTLINE_MAX, -OUTLINE_MAX))
            .is_ok()
    );
    assert_eq!(
        outline.push_point(Point::new(OUTLINE_MAX + 1, 0)),
        Err(OutlineError::PointOutOfRange)
    );
    assert_eq!(
        outline.push_point(Point::new(0, i32::MIN)),
        Err(OutlineError::PointOutOfRange)
    );
    assert_eq!(outline.points().len(), 1);
    assert_eq!(
        outline.add_rect(0, 0, OUTLINE_MAX + 1, 5),
        Err(OutlineError::PointOutOfRange)
    );
    assert_eq!(outline.points().len(), 1);
}

#[test]
fn inconsistent_segments_are_reported_not_panicked() {
    // Сегмент без точек.
    let mut no_points = Outline::new();
    no_points.push_segment(SegmentKind::Line);
    assert_eq!(no_points.validate(), Err(OutlineError::Malformed));

    // Кубический сегмент владеет тремя точками, а есть две.
    let mut short = Outline::new();
    short.push_point(Point::new(0, 0)).unwrap();
    short.push_point(Point::new(1, 1)).unwrap();
    short.push_segment(SegmentKind::Cubic);
    assert_eq!(short.validate(), Err(OutlineError::Malformed));

    // Лишняя точка после последнего сегмента.
    let mut extra = rect(0, 0, 1, 1);
    extra.push_point(Point::new(7, 7)).unwrap();
    assert_eq!(extra.validate(), Err(OutlineError::Malformed));

    // Контур не закрыт.
    let mut open = Outline::new();
    open.push_point(Point::new(0, 0)).unwrap();
    open.push_point(Point::new(64, 0)).unwrap();
    open.push_segment(SegmentKind::Line);
    open.push_segment(SegmentKind::Line);
    assert_eq!(open.validate(), Err(OutlineError::Malformed));
    assert!(rect(0, 0, 1, 1).validate().is_ok());
}

#[test]
fn close_contour_reports_nothing_to_close() {
    let mut outline = Outline::new();
    assert!(!outline.close_contour(), "no segments yet");
    outline.push_point(Point::new(0, 0)).unwrap();
    outline.push_segment(SegmentKind::Line);
    assert!(outline.close_contour());
    assert!(!outline.close_contour(), "already closed");
}

#[test]
fn control_box_covers_all_points_including_control_points() {
    let mut outline = Outline::new();
    for point in [(0, 0), (30, -40), (70, 90), (100, 10)] {
        outline.push_point(Point::new(point.0, point.1)).unwrap();
    }
    outline.push_segment(SegmentKind::Cubic);
    outline.push_segment(SegmentKind::Line);
    assert!(outline.close_contour());
    let mut cbox = ControlBox::EMPTY;
    assert!(cbox.is_empty());
    outline.update_control_box(&mut cbox);
    assert_eq!(
        cbox,
        ControlBox {
            x_min: 0,
            y_min: -40,
            x_max: 100,
            y_max: 90
        }
    );
}

#[test]
fn scaled_pow2_multiplies_and_floors_like_the_c_shift() {
    let mut outline = Outline::new();
    outline.add_rect(-3, 5, 7, 9).unwrap();
    let up = outline.scaled_pow2(2, 1).unwrap();
    assert_eq!(up.points()[0], Point::new(-12, 10));
    assert_eq!(up.points()[2], Point::new(28, 18));
    // Масштаб вниз округляет к минус бесконечности: -3 >> 1 == -2.
    let down = outline.scaled_pow2(-1, -2).unwrap();
    assert_eq!(down.points()[0], Point::new(-2, 1));
    assert_eq!(down.points()[2], Point::new(3, 2));
    // Сегменты сохраняются.
    assert_eq!(down.segments(), outline.segments());
}

#[test]
fn scaled_pow2_rejects_overflow_and_keeps_empty_outlines_empty() {
    let mut big = Outline::new();
    big.add_rect(0, 0, OUTLINE_MAX, 1).unwrap();
    assert_eq!(big.scaled_pow2(1, 0), Err(OutlineError::TransformOverflow));
    assert_eq!(big.scaled_pow2(40, 0), Err(OutlineError::TransformOverflow));
    assert!(big.scaled_pow2(-3, 0).is_ok());
    assert!(Outline::new().scaled_pow2(5, 5).unwrap().is_empty());
}

#[test]
fn transformed_2d_rounds_half_to_even_like_lrint() {
    let mut outline = Outline::new();
    for x in [1, 3, 5, 7] {
        outline.push_point(Point::new(x, 0)).unwrap();
        outline.push_segment(SegmentKind::Line);
    }
    assert!(outline.close_contour());
    // x * 0.5: 0.5 -> 0, 1.5 -> 2, 2.5 -> 2, 3.5 -> 4.
    let halved = outline
        .transformed_2d(&[[0.5, 0.0, 0.0], [0.0, 1.0, 0.0]])
        .unwrap();
    let xs: Vec<i32> = halved.points().iter().map(|point| point.x).collect();
    assert_eq!(xs, [0, 2, 2, 4]);
}

#[test]
fn transformed_2d_applies_the_matrix_and_rejects_overflow_and_nan() {
    let mut outline = Outline::new();
    outline.add_rect(0, 0, 10, 20).unwrap();
    // Поворот на 90 градусов со сдвигом: (x, y) -> (-y + 100, x + 5).
    let rotated = outline
        .transformed_2d(&[[0.0, -1.0, 100.0], [1.0, 0.0, 5.0]])
        .unwrap();
    assert_eq!(rotated.points()[1], Point::new(100, 15));
    assert_eq!(rotated.points()[2], Point::new(80, 15));
    assert_eq!(
        outline.transformed_2d(&[[1e12, 0.0, 0.0], [0.0, 1.0, 0.0]]),
        Err(OutlineError::TransformOverflow)
    );
    assert_eq!(
        outline.transformed_2d(&[[f64::NAN, 0.0, 0.0], [0.0, 1.0, 0.0]]),
        Err(OutlineError::TransformOverflow)
    );
}

/// Рисует символ встроенным Noto Sans Regular через адаптер пера; размер в пикселях.
fn glyph_outline(character: char, pixels: f32) -> Outline {
    let font = FontRef::new(EMBEDDED_FONTS[0].data).expect("embedded font must parse");
    let glyph_id = font.charmap().map(character).expect("glyph must exist");
    let glyph = font
        .outline_glyphs()
        .get(glyph_id)
        .expect("outline must exist");
    let mut pen = GlyphOutlinePen::new();
    glyph
        .draw(
            DrawSettings::unhinted(Size::new(pixels), LocationRef::default()),
            &mut pen,
        )
        .expect("glyph must draw");
    pen.finish().expect("pen must produce an outline")
}

#[test]
fn glyph_outline_is_valid_closed_and_has_screen_orientation() {
    let outline = glyph_outline('H', 100.0);
    outline.validate().unwrap();
    assert!(outline.is_closed());
    let mut cbox = ControlBox::EMPTY;
    outline.update_control_box(&mut cbox);
    // Заглавная H стоит на базовой линии: низ около 0, верх выше (Y вниз => отрицательный).
    assert!(
        cbox.y_max.abs() <= 64,
        "baseline is near y = 0, got {}",
        cbox.y_max
    );
    let cap_height = -cbox.y_min;
    assert!(
        (6400 * 65 / 100..=6400 * 80 / 100).contains(&cap_height),
        "cap height of Noto Sans is ~71% of the size, got {cap_height} (26.6)"
    );
    // H — один замкнутый контур.
    let contours = outline
        .segments()
        .iter()
        .filter(|segment| segment.ends_contour)
        .count();
    assert_eq!(contours, 1);
}

#[test]
fn glyph_with_a_hole_has_two_contours_and_rasterizes_with_the_hole() {
    // «O»: внешний и внутренний контуры; в центре дыра, на кольце заливка.
    let outline = glyph_outline('O', 120.0);
    let contours = outline
        .segments()
        .iter()
        .filter(|segment| segment.ends_contour)
        .count();
    assert_eq!(contours, 2);
    let mut rasterizer = Rasterizer::new(16);
    let bitmap = outline_to_bitmap(&mut rasterizer, Some(&outline), None)
        .unwrap()
        .expect("glyph is not empty");
    let mut cbox = ControlBox::EMPTY;
    outline.update_control_box(&mut cbox);
    let at = |x_d6: i32, y_d6: i32| -> u8 {
        let column = (x_d6 >> 6) - bitmap.left;
        let row = (y_d6 >> 6) - bitmap.top;
        bitmap.row(row).expect("row inside the bitmap")[column as usize]
    };
    let center_x = (cbox.x_min + cbox.x_max) / 2;
    let center_y = (cbox.y_min + cbox.y_max) / 2;
    assert_eq!(at(center_x, center_y), 0, "hole in the middle of O");
    // Левая стенка кольца: на трети ширины от левого края (толщина штриха ~12% кегля).
    let wall_x = cbox.x_min + (cbox.x_max - cbox.x_min) / 22;
    assert_eq!(
        at(wall_x + 64 * 2, center_y),
        255,
        "ring of O is fully covered"
    );
}

#[test]
fn pen_drops_degenerate_contours_and_closes_open_ones() {
    let mut pen = GlyphOutlinePen::new();
    // Вырожденный контур из двух точек пропадает.
    pen.move_to(0.0, 0.0);
    pen.line_to(10.0, 0.0);
    pen.close();
    // Треугольник без явного замыкания получает замыкающую линию.
    pen.move_to(0.0, 0.0);
    pen.line_to(10.0, 0.0);
    pen.line_to(10.0, 10.0);
    pen.close();
    let outline = pen.finish().unwrap();
    outline.validate().unwrap();
    assert_eq!(outline.segments().len(), 3, "two lines + the closing line");
    assert!(
        outline
            .segments()
            .iter()
            .filter(|segment| segment.ends_contour)
            .count()
            == 1
    );
    // Y инвертирован, координаты в 26.6: (10, 10) -> (640, -640).
    assert_eq!(outline.points()[2], Point::new(640, -640));
}

#[test]
fn pen_uses_the_last_curve_as_the_closing_segment_when_it_returns_to_the_start() {
    let mut pen = GlyphOutlinePen::new();
    pen.move_to(0.0, 0.0);
    pen.quad_to(10.0, 0.0, 10.0, 10.0);
    pen.curve_to(10.0, 20.0, 0.0, 20.0, 0.0, 0.0);
    pen.close();
    let outline = pen.finish().unwrap();
    outline.validate().unwrap();
    let kinds: Vec<_> = outline
        .segments()
        .iter()
        .map(|segment| segment.kind)
        .collect();
    assert_eq!(kinds, [SegmentKind::Quadratic, SegmentKind::Cubic]);
    assert!(outline.segments()[1].ends_contour);
    assert_eq!(
        outline.points().len(),
        5,
        "2 + 3 owned points, no duplicate start"
    );
}

#[test]
fn pen_rounds_coordinates_to_26_6_away_from_zero() {
    let mut pen = GlyphOutlinePen::new();
    pen.move_to(0.0, 0.0);
    // 0.0078125 px = 0.5/64 -> округляется до 1/64; 1.5/64 -> 2/64.
    pen.line_to(0.0078125, 0.0234375);
    pen.line_to(0.0, 1.0);
    pen.close();
    let outline = pen.finish().unwrap();
    assert_eq!(outline.points()[1], Point::new(1, -2));
}

#[test]
fn pen_reports_out_of_range_coordinates_and_protocol_violations() {
    let mut huge = GlyphOutlinePen::new();
    huge.move_to(0.0, 0.0);
    huge.line_to(1.0e7, 0.0);
    huge.line_to(0.0, 5.0);
    huge.close();
    assert_eq!(huge.finish().err(), Some(OutlineError::PointOutOfRange));

    let mut no_move = GlyphOutlinePen::new();
    no_move.line_to(1.0, 1.0);
    assert_eq!(no_move.finish().err(), Some(OutlineError::Malformed));

    // Пустой глиф (пробел): пустой контур, не ошибка.
    assert!(GlyphOutlinePen::new().finish().unwrap().is_empty());
}
