use super::*;
use crate::outline::SegmentKind;

/// Квадрат со стороной `side` (26.6) над базовой линией, обход по часовой стрелке на экране.
fn clockwise_square(side: i32) -> Outline {
    let mut outline = Outline::new();
    outline.add_rect(0, -side, side, 0).expect("rect");
    outline
}

/// Добавляет контур-«дыру»: обход против часовой стрелки.
fn add_counter_clockwise_hole(outline: &mut Outline, min: i32, max: i32) {
    let corners = [(min, min), (min, max), (max, max), (max, min)];
    for (x, y) in corners {
        outline.push_point(Point::new(x, y)).expect("point");
    }
    for _ in 0..4 {
        outline.push_segment(SegmentKind::Line);
    }
    assert!(outline.close_contour());
}

fn bounds(outline: &Outline) -> ControlBox {
    let mut control_box = ControlBox::EMPTY;
    outline.update_control_box(&mut control_box);
    control_box
}

#[test]
fn winding_distinguishes_clockwise_counter_clockwise_and_collapsed() {
    assert_eq!(
        clockwise_square(640).outer_winding(),
        OuterWinding::Clockwise
    );
    let mut reversed = Outline::new();
    add_counter_clockwise_hole(&mut reversed, 0, 640);
    assert_eq!(reversed.outer_winding(), OuterWinding::CounterClockwise);
    let mut flat = Outline::new();
    flat.add_rect(0, 0, 640, 0).expect("flat");
    assert_eq!(flat.outer_winding(), OuterWinding::Undefined);
    assert_eq!(
        Outline::new().outer_winding(),
        OuterWinding::Clockwise,
        "пустой контур, как у FreeType, считается TrueType"
    );
}

#[test]
fn shear_moves_the_top_right_and_keeps_the_baseline() {
    // Сдвиг 1.0 (0x10000): точка на высоте h уезжает вправо ровно на h.
    let sheared = clockwise_square(640).sheared(0x10000).expect("shear");
    let result = bounds(&sheared);
    assert_eq!((result.x_min, result.x_max), (0, 640 + 640));
    assert_eq!((result.y_min, result.y_max), (-640, 0), "Y не меняется");
    // Нижние точки (на базовой линии) остались на месте.
    assert!(sheared.points().contains(&Point::new(0, 0)));
    assert!(sheared.points().contains(&Point::new(640, 0)));
    // Верхние точки сдвинуты на высоту.
    assert!(sheared.points().contains(&Point::new(640, -640)));
    assert!(sheared.points().contains(&Point::new(1280, -640)));
}

#[test]
fn shear_below_the_baseline_moves_left() {
    let mut below = Outline::new();
    below.add_rect(0, 0, 640, 640).expect("rect");
    let sheared = below.sheared(0x10000).expect("shear");
    assert_eq!(bounds(&sheared).x_min, -640);
}

#[test]
fn embolden_grows_square_by_strength_keeping_the_origin() {
    let bold = clockwise_square(640).emboldened(64).expect("embolden");
    let result = bounds(&bold);
    // Выросло на strength по обеим осям; левый нижний угол остался в начале координат.
    assert_eq!((result.x_min, result.x_max), (0, 704));
    assert_eq!((result.y_min, result.y_max), (-704, 0));
}

#[test]
fn embolden_shrinks_holes_instead_of_growing_them() {
    let mut glyph = Outline::new();
    glyph.add_rect(0, -1000, 1000, 0).expect("outer");
    add_counter_clockwise_hole(&mut glyph, -700, -300);
    let bold = glyph.emboldened(64).expect("embolden");
    let outer = bold.points().get(..4).expect("outer points");
    let hole = bold.points().get(4..8).expect("hole points");
    let width = |points: &[Point]| {
        let min = points.iter().map(|point| point.x).min().expect("min");
        let max = points.iter().map(|point| point.x).max().expect("max");
        max - min
    };
    assert_eq!(width(outer), 1000 + 64, "внешний контур вырос");
    assert_eq!(width(hole), 400 - 64, "дыра сузилась на ту же величину");
}

#[test]
fn embolden_of_collapsed_outline_is_a_no_op() {
    let mut flat = Outline::new();
    flat.add_rect(0, 0, 640, 0).expect("flat");
    assert_eq!(flat.emboldened(64).expect("embolden"), flat);
}

#[test]
fn embolden_reports_range_overflow_instead_of_wrapping() {
    let mut huge = Outline::new();
    huge.add_rect(
        0,
        -(super::super::OUTLINE_MAX - 10),
        super::super::OUTLINE_MAX - 10,
        0,
    )
    .expect("rect");
    assert_eq!(huge.emboldened(1 << 10), Err(OutlineError::PointOutOfRange));
}
