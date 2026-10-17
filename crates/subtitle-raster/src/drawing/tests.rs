use super::*;
use crate::outline::OUTLINE_MAX;

/// Сколько контуров закрыто в контуре.
fn closed_contours(parsed: &ParsedDrawing) -> usize {
    parsed
        .outline
        .segments()
        .iter()
        .filter(|segment| segment.ends_contour)
        .count()
}

#[test]
fn square_becomes_a_closed_contour_with_matching_bounds() {
    let parsed = parse_drawing("m 0 0 l 10 0 10 10 0 10").expect("parse");
    assert_eq!(
        parsed.outline.points(),
        [
            Point::new(0, 0),
            Point::new(640, 0),
            Point::new(640, 640),
            Point::new(0, 640),
        ]
    );
    assert_eq!(parsed.outline.segments().len(), 4);
    assert_eq!(closed_contours(&parsed), 1);
    assert_eq!((parsed.width(), parsed.height()), (640, 640));
    parsed.outline.validate().expect("consistent outline");
}

#[test]
fn m_closes_the_previous_contour_but_n_keeps_it_open() {
    let two = parse_drawing("m 0 0 l 10 0 10 10 m 20 20 l 30 20 30 30").expect("parse");
    assert_eq!(closed_contours(&two), 2);
    two.outline.validate().expect("consistent outline");

    let one = parse_drawing("m 0 0 l 10 0 10 10 n 20 20 l 30 20").expect("parse");
    assert_eq!(closed_contours(&one), 1, "n не закрывает контур");
    one.outline.validate().expect("consistent outline");
}

#[test]
fn bezier_curve_keeps_its_four_points_and_one_cubic_segment() {
    let parsed = parse_drawing("m 0 0 b 10 0 20 10 30 10").expect("parse");
    assert_eq!(
        parsed.outline.points(),
        [
            Point::new(0, 0),
            Point::new(640, 0),
            Point::new(1280, 640),
            Point::new(1920, 640),
        ]
    );
    let kinds: Vec<SegmentKind> = parsed
        .outline
        .segments()
        .iter()
        .map(|segment| segment.kind)
        .collect();
    assert_eq!(kinds, [SegmentKind::Cubic, SegmentKind::Line]);
    parsed.outline.validate().expect("consistent outline");
}

#[test]
fn bspline_points_are_converted_with_integer_arithmetic() {
    let converted = bspline_to_bezier([
        Point::new(0, 0),
        Point::new(192, 0),
        Point::new(384, 192),
        Point::new(576, 192),
    ]);
    assert_eq!(
        converted,
        [
            Point::new(192, 32),
            Point::new(256, 64),
            Point::new(320, 128),
            Point::new(384, 160),
        ]
    );
}

#[test]
fn bspline_division_truncates_toward_zero_and_the_shift_is_arithmetic() {
    // x01 = -4 / 3 = -1 (усечение к нулю, не -2): p0.x = -4 + ((0 - -1) >> 1) = -4.
    let truncated = bspline_to_bezier([
        Point::new(0, 0),
        Point::new(-4, 0),
        Point::new(-4, 0),
        Point::new(-4, 0),
    ]);
    assert_eq!(truncated[0].x, -4);
    // x12 = 3, x23 = 0: p3.x = 9 + ((0 - 3) >> 1) = 9 - 2 = 7 (сдвиг вниз, не усечение).
    let shifted = bspline_to_bezier([
        Point::new(0, 0),
        Point::new(0, 0),
        Point::new(9, 0),
        Point::new(9, 0),
    ]);
    assert_eq!(shifted[3].x, 7);
}

#[test]
fn closed_bspline_forms_a_valid_contour() {
    let parsed = parse_drawing("m 0 0 s 10 0 10 10 0 10 c").expect("parse");
    parsed.outline.validate().expect("consistent outline");
    assert_eq!(closed_contours(&parsed), 1);
    assert!(parsed.outline.segments().len() >= 4);
}

#[test]
fn empty_or_rejected_drawing_has_no_outline_and_libass_wrapped_metrics() {
    for text in ["", "garbage", "n 1 1", "l 1 1"] {
        let parsed = parse_drawing(text).expect("parse");
        assert!(parsed.outline.is_empty(), "{text:?}");
        assert!(parsed.bounds.is_empty(), "{text:?}");
        // Перевёрнутая рамка: i32::MIN - i32::MAX заворачивается в 1, как в C.
        assert_eq!((parsed.width(), parsed.height()), (1, 1), "{text:?}");
    }
}

#[test]
fn lone_move_has_a_point_box_but_no_outline() {
    let parsed = parse_drawing("m 5 7").expect("parse");
    assert!(parsed.outline.is_empty());
    assert_eq!((parsed.width(), parsed.height()), (0, 0));
}

#[test]
fn bounds_include_move_targets_that_draw_nothing() {
    let parsed = parse_drawing("m 0 0 l 10 0 10 10 m 100 100").expect("parse");
    assert_eq!((parsed.width(), parsed.height()), (6400, 6400));
}

#[test]
fn point_beyond_the_outline_range_rejects_the_drawing() {
    let limit_in_pixels = f64::from(OUTLINE_MAX) / 64.0;
    let too_far = format!("m 0 0 l {} 0", limit_in_pixels + 2.0);
    assert!(matches!(
        parse_drawing(&too_far),
        Err(DrawingError::Outline(OutlineError::PointOutOfRange))
    ));
    // 99999999999 * 64 по модулю 2^32 = 498728896 (как в C): всё равно за пределом контура.
    assert!(parse_drawing("m 0 0 l 99999999999 0").is_err());
    let at_limit = format!("m 0 0 l {} 0", limit_in_pixels.floor());
    assert!(parse_drawing(&at_limit).is_ok());
}

#[test]
fn coordinates_wrap_modulo_2_pow_32_like_libass_instead_of_saturating() {
    // 67108874 px = 2^32 / 64 + 10 px: в C `int` оставляет младшие 32 бита, то есть 10 px.
    let wrapped = parse_drawing("m 0 0 l 67108874 0 l 0 10").expect("parse");
    assert_eq!(
        wrapped.outline.points(),
        [Point::new(0, 0), Point::new(640, 0), Point::new(0, 640)]
    );
}

#[test]
fn non_finite_and_out_of_i64_coordinates_become_zero_like_libass() {
    // `1e999` - бесконечность, `1e30` не влезает в 64 бита: `lrint` даёт i64::MIN, а после
    // усечения до `int` - 0. Фигура остаётся валидной, в отличие от насыщения до i32::MAX.
    for far in ["1e999", "-1e999", "1e30"] {
        let parsed = parse_drawing(&format!("m 0 0 l {far} 0 l 0 10")).expect(far);
        assert_eq!(
            parsed.outline.points(),
            [Point::new(0, 0), Point::new(0, 0), Point::new(0, 640)],
            "{far}"
        );
    }
}
