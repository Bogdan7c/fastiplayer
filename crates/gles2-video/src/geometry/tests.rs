use super::*;

fn apply(rows: [[f32; 3]; 2], u: f32, v: f32) -> [f32; 2] {
    let dot = |row: [f32; 3]| row[0] * u + row[1] * v + row[2];
    [dot(rows[0]), dot(rows[1])]
}

#[test]
fn quarter_turns_map_display_corners_to_rotated_source_corners() {
    // Верхний левый угол экрана после поворота кадра на 90° по часовой показывает нижний левый
    // угол кадра декодера: source = (v, 1 − u).
    assert_eq!(
        apply(QuarterTurn::Identity.display_to_source_rows(), 0.0, 0.0),
        [0.0, 0.0]
    );
    assert_eq!(
        apply(QuarterTurn::Clockwise90.display_to_source_rows(), 0.0, 0.0),
        [0.0, 1.0]
    );
    assert_eq!(
        apply(QuarterTurn::Half.display_to_source_rows(), 0.0, 0.0),
        [1.0, 1.0]
    );
    assert_eq!(
        apply(QuarterTurn::Clockwise270.display_to_source_rows(), 0.0, 0.0),
        [1.0, 0.0]
    );
}

#[test]
fn y_axis_is_flipped_for_gl_window_coordinates() {
    let rect = PixelRect {
        x: 10,
        y: 20,
        width: 30,
        height: 40,
    };
    assert_eq!(
        to_gl_window_rect(rect, 100),
        Ok(GlWindowRect {
            x: 10,
            y: 40,
            width: 30,
            height: 40
        })
    );
}

#[test]
fn rectangle_below_the_target_gets_negative_gl_bottom() {
    let rect = PixelRect {
        x: 0,
        y: 90,
        width: 10,
        height: 20,
    };
    assert_eq!(to_gl_window_rect(rect, 100).map(|gl| gl.y), Ok(-10));
}

#[test]
fn huge_rectangle_is_a_typed_error() {
    let rect = PixelRect {
        x: u32::MAX,
        y: 0,
        width: 1,
        height: 1,
    };
    assert_eq!(
        to_gl_window_rect(rect, 100),
        Err(RectOutOfRange {
            rect,
            target_height: 100
        })
    );
}

#[test]
fn uncovered_parts_leave_only_what_no_rect_covers() {
    let target = PixelRect {
        x: 0,
        y: 0,
        width: 100,
        height: 50,
    };
    assert_eq!(uncovered_parts(target, &[target]), Vec::new());
    let sidebar_free = PixelRect {
        x: 30,
        y: 0,
        width: 70,
        height: 50,
    };
    assert_eq!(
        uncovered_parts(target, &[sidebar_free]),
        vec![PixelRect {
            x: 0,
            y: 0,
            width: 30,
            height: 50
        }]
    );
    // Две части рядом накрывают всё.
    let top = PixelRect {
        x: 0,
        y: 0,
        width: 100,
        height: 20,
    };
    let bottom = PixelRect {
        x: 0,
        y: 20,
        width: 100,
        height: 30,
    };
    assert_eq!(uncovered_parts(target, &[top, bottom]), Vec::new());
    // Прямоугольник за пределами цели ничего не накрывает.
    let outside = PixelRect {
        x: 200,
        y: 200,
        width: 5,
        height: 5,
    };
    assert_eq!(uncovered_parts(target, &[outside]), vec![target]);
}

#[test]
fn intersection_handles_touching_and_huge_rects() {
    let left = PixelRect {
        x: 0,
        y: 0,
        width: 10,
        height: 10,
    };
    let touching = PixelRect {
        x: 10,
        y: 0,
        width: 10,
        height: 10,
    };
    assert_eq!(left.intersection(touching), None);
    let huge = PixelRect {
        x: 5,
        y: 5,
        width: u32::MAX,
        height: u32::MAX,
    };
    assert_eq!(
        left.intersection(huge),
        Some(PixelRect {
            x: 5,
            y: 5,
            width: 5,
            height: 5
        })
    );
}
