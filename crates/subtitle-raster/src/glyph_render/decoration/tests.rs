use skrifa::{FontRef, MetadataProvider};

use super::*;
use crate::fonts::FaceMetrics;
use crate::outline::{ControlBox, Point};

const NOTO_REGULAR: &[u8] = include_bytes!("../../../fonts/NotoSans-Regular.ttf");

/// Масштаб и начертание встроенного Noto Sans Regular.
fn noto() -> (FaceScale, FaceStyleInfo) {
    let font = FontRef::new(NOTO_REGULAR).expect("font");
    let fonts = crate::test_support::embedded_fonts();
    let face = fonts
        .resolve_family(&crate::fonts::FontRequest {
            family: None,
            weight: crate::fonts::FontWeightClass::Regular,
            style: crate::fonts::FontStyle::Normal,
        })
        .face;
    let metrics: FaceMetrics = fonts.face_metrics(face).expect("metrics");
    (
        FaceScale::from_metrics(&metrics).expect("scale"),
        FaceStyleInfo::from_font(&font),
    )
}

fn both() -> TextDecorations {
    TextDecorations {
        underline: true,
        strike_out: true,
    }
}

fn clockwise_square() -> Outline {
    let mut outline = Outline::new();
    outline.add_rect(100, -1000, 900, 0).expect("square");
    outline
}

fn bounds(outline: &Outline) -> ControlBox {
    let mut control_box = ControlBox::EMPTY;
    outline.update_control_box(&mut control_box);
    control_box
}

#[test]
fn underline_is_a_rectangle_below_the_baseline_across_the_advance() {
    let (scale, style) = noto();
    let mut outline = clockwise_square();
    let letter_points = outline.points().len();
    add_decoration_lines(
        &mut outline,
        &scale,
        &style,
        TextDecorations {
            underline: true,
            strike_out: false,
        },
        2000,
    )
    .expect("decoration");
    let rect = outline.points().get(letter_points..).expect("rect points");
    assert_eq!(rect.len(), 4, "one rectangle");
    // Прямоугольник на всю ширину продвижения; Y вниз, поэтому «под строкой» — Y > 0.
    let (min_x, max_x) = (
        rect.iter().map(|point| point.x).min().expect("min"),
        rect.iter().map(|point| point.x).max().expect("max"),
    );
    assert_eq!((min_x, max_x), (0, 2000));
    let (min_y, max_y) = (
        rect.iter().map(|point| point.y).min().expect("min"),
        rect.iter().map(|point| point.y).max().expect("max"),
    );
    assert!(
        min_y > 0 && max_y > min_y,
        "underline hangs under the baseline"
    );
}

#[test]
fn strikeout_crosses_the_x_height_above_the_baseline() {
    let (scale, style) = noto();
    let mut outline = clockwise_square();
    add_decoration_lines(
        &mut outline,
        &scale,
        &style,
        TextDecorations {
            underline: false,
            strike_out: true,
        },
        2000,
    )
    .expect("decoration");
    let rect = outline.points().get(4..).expect("rect points");
    assert!(
        rect.iter().all(|point| point.y < 0),
        "strike-out sits above the baseline"
    );
}

#[test]
fn zero_advance_draws_no_lines() {
    let (scale, style) = noto();
    let mut outline = clockwise_square();
    let before = outline.clone();
    add_decoration_lines(&mut outline, &scale, &style, both(), 0).expect("decoration");
    assert_eq!(outline, before);
    add_decoration_lines(&mut outline, &scale, &style, both(), -64).expect("decoration");
    assert_eq!(outline, before, "negative advance is treated like zero");
}

#[test]
fn no_decoration_flags_leave_the_outline_untouched() {
    let (scale, style) = noto();
    let mut outline = clockwise_square();
    let before = outline.clone();
    add_decoration_lines(
        &mut outline,
        &scale,
        &style,
        TextDecorations::default(),
        2000,
    )
    .expect("decoration");
    assert_eq!(outline, before);
}

#[test]
fn rectangle_winding_follows_the_glyph_so_lines_never_cancel_the_letter() {
    let (scale, style) = noto();
    let underline_only = TextDecorations {
        underline: true,
        strike_out: false,
    };
    let mut clockwise = clockwise_square();
    add_decoration_lines(&mut clockwise, &scale, &style, underline_only, 2000).expect("cw");
    // Прямоугольник у «часового» глифа обходится слева направо по верхней кромке.
    let top_left = clockwise
        .points()
        .get(4)
        .copied()
        .expect("first rect point");
    let top_right = clockwise
        .points()
        .get(5)
        .copied()
        .expect("second rect point");
    assert_eq!(top_left.y, top_right.y);
    assert!(top_left.x < top_right.x);

    let mut reversed = Outline::new();
    for (x, y) in [(100, -1000), (100, 0), (900, 0), (900, -1000)] {
        reversed.push_point(Point::new(x, y)).expect("point");
    }
    for _ in 0..4 {
        reversed.push_segment(crate::outline::SegmentKind::Line);
    }
    assert!(reversed.close_contour());
    add_decoration_lines(&mut reversed, &scale, &style, underline_only, 2000).expect("ccw");
    let first = reversed.points().get(4).copied().expect("first rect point");
    let second = reversed
        .points()
        .get(5)
        .copied()
        .expect("second rect point");
    assert_eq!(first.y, second.y);
    assert!(first.x < second.x, "starts on the left edge");
    assert!(
        first.y > clockwise.points()[4].y,
        "reversed glyph starts from the lower edge"
    );
}

#[test]
fn lines_on_an_empty_glyph_still_produce_a_visible_box() {
    // Пробел: контура нет, но линия под ним рисуется (в libass линия кладётся и на пустой глиф).
    let (scale, style) = noto();
    let mut outline = Outline::new();
    add_decoration_lines(&mut outline, &scale, &style, both(), 1000).expect("decoration");
    let result = bounds(&outline);
    assert_eq!((result.x_min, result.x_max), (0, 1000));
    assert!(
        result.y_min < 0 && result.y_max > 0,
        "strike-out above and underline below"
    );
    assert!(
        FontRef::new(NOTO_REGULAR)
            .expect("font")
            .charmap()
            .map(' ')
            .is_some()
    );
}
