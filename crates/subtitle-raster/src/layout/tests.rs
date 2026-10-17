use subtitle_core::ScreenAnchor;

use super::*;
use crate::overlay::CanvasSize;
use crate::test_support::{embedded_fonts, plain_style, plain_text, shaped_clusters};

fn canvas() -> CanvasSize {
    CanvasSize::new(1280, 720).expect("canvas")
}

fn arranged(text: &str, style: &EventStyle) -> (Vec<TextCluster>, TextLayout) {
    let mut fonts = embedded_fonts();
    let mut clusters = shaped_clusters(&mut fonts, &plain_text(text), style);
    let layout = arrange_text(&mut clusters, style, canvas());
    (clusters, layout)
}

#[test]
fn short_text_stays_on_one_line_and_is_centered() {
    let (clusters, layout) = arranged("Hello", &plain_style());
    assert_eq!(layout.measure.lines.len(), 1);
    let left = layout.bounds.x_min;
    let right = layout.bounds.x_max;
    // Блок центрируется между полями 40..1240: середина совпадает с серединой кадра.
    let middle = layout.device_x + (left + right) / 2.0;
    assert!((middle - 640.0).abs() < 0.5, "middle {middle}");
    assert!(clusters.iter().all(|c| !c.line_start.begins_line()));
}

#[test]
fn bottom_anchor_puts_the_last_line_bottom_on_height_minus_margin() {
    let style = plain_style();
    let (_, layout) = arranged("Hello", &style);
    let line = layout.measure.lines[0];
    let line_bottom = layout.device_y + line.descent;
    assert!((line_bottom - 680.0).abs() < 1e-9, "bottom {line_bottom}");
}

#[test]
fn top_and_middle_anchors_place_the_block_accordingly() {
    let mut style = plain_style();
    style.alignment = ScreenAnchor::TopCenter;
    let (_, top) = arranged("Hello", &style);
    assert!((top.device_y - top.measure.lines[0].ascent - 40.0).abs() < 1e-9);
    style.alignment = ScreenAnchor::MiddleCenter;
    let (_, middle) = arranged("Hello", &style);
    let block_middle = middle.device_y + (middle.bounds.y_max + middle.bounds.y_min) / 2.0;
    assert!((block_middle - 360.0).abs() < 1e-9);
}

#[test]
fn explicit_line_break_makes_two_lines_and_hides_the_break_symbol() {
    let (clusters, layout) = arranged("First\nSecond", &plain_style());
    assert_eq!(layout.measure.lines.len(), 2);
    let second = clusters.iter().position(|c| c.symbol == 'S').expect("S");
    assert!(clusters[second].line_start.begins_line());
    assert!(
        clusters[5].skip && clusters[5].is_trimmed_whitespace,
        "the \\n symbol is trimmed"
    );
    // Вторая строка ниже первой на высоту строки.
    assert!(clusters[second].root.pos.y > clusters[0].root.pos.y);
    assert!(layout.measure.height > 2.0 * 40.0);
}

#[test]
fn long_text_wraps_and_lines_are_balanced() {
    let long = "The quick brown fox jumps over the lazy dog and keeps running far away from here";
    let mut style = plain_style();
    style.margins.left = 100;
    style.margins.right = 100;
    let (clusters, layout) = arranged(long, &style);
    assert_eq!(layout.measure.lines.len(), 2);
    let second_start = clusters
        .iter()
        .position(|c| c.line_start.begins_line())
        .expect("break");
    // Перенос после пробела; ширины строк отличаются меньше, чем на длину самого длинного слова.
    assert_eq!(clusters[second_start - 1].symbol, ' ');
    let width = |range: std::ops::Range<usize>| -> i32 {
        range
            .filter(|i| !clusters[*i].skip)
            .map(|i| clusters[i].cluster_advance.x)
            .sum::<i32>()
    };
    let first = width(0..second_start);
    let second = width(second_start..clusters.len());
    assert!((first - second).abs() < 64 * 200, "{first} vs {second}");
    assert!(
        first.max(second) < 64 * 1080,
        "both lines fit into the text width"
    );
}

#[test]
fn no_wrap_style_keeps_a_single_overlong_line() {
    let long = "The quick brown fox jumps over the lazy dog and keeps running far away from here";
    let mut style = plain_style();
    style.wrap_style = crate::event_style::WrapStyle::NoWrap;
    let (_, layout) = arranged(long, &style);
    assert_eq!(layout.measure.lines.len(), 1);
}

#[test]
fn leading_and_trailing_spaces_are_trimmed_and_do_not_shift_the_text() {
    let (plain, plain_layout) = arranged("Hi", &plain_style());
    let (padded, padded_layout) = arranged("   Hi   ", &plain_style());
    assert!(padded[..3].iter().all(|c| c.skip));
    assert!(padded[5..].iter().all(|c| c.skip));
    let x = |clusters: &[TextCluster], at: usize| clusters[at].root.pos.x;
    assert_eq!(x(&plain, 0), x(&padded, 3));
    assert_eq!(
        plain_layout.measure.lines[0],
        padded_layout.measure.lines[0]
    );
}

#[test]
fn rtl_run_is_displayed_in_reverse_order_inside_the_ltr_line() {
    let (clusters, _) = arranged("abc אבג", &plain_style());
    let x = |index: usize| clusters[index].root.pos.x;
    // Латиница слева направо, затем иврит справа налево: «א» правее «ב», «ב» правее «ג».
    assert!(x(0) < x(1) && x(1) < x(2) && x(2) < x(3));
    assert!(
        x(4) > x(5) && x(5) > x(6),
        "Hebrew letters go right to left"
    );
    assert!(x(6) > x(3), "the whole Hebrew run is after the Latin part");
}

#[test]
fn right_alignment_moves_the_line_to_the_right_margin() {
    let mut style = plain_style();
    style.alignment = ScreenAnchor::BottomRight;
    let (clusters, layout) = arranged("Hello", &style);
    let last = clusters.last().expect("cluster");
    let right_edge = layout.device_x + (last.root.pos.x + last.cluster_advance.x) as f64 / 64.0;
    assert!((right_edge - 1240.0).abs() < 0.1, "right edge {right_edge}");
}

#[test]
fn pen_positions_saturate_instead_of_overflowing_on_absurd_advances() {
    let mut fonts = embedded_fonts();
    let style = plain_style();
    let mut clusters = shaped_clusters(&mut fonts, &plain_text("abc"), &style);
    // Продвижения, при сумме которых i32 переполнился бы (в debug это паника).
    for cluster in &mut clusters {
        cluster.root.advance.x = i32::MAX / 2 + 1;
        cluster.cluster_advance.x = i32::MAX / 2 + 1;
    }
    preliminary_layout(&mut clusters);
    let positions: Vec<i32> = clusters.iter().map(|c| c.root.pos.x).collect();
    assert_eq!(positions[0], 0);
    assert_eq!(positions[1], i32::MAX / 2 + 1);
    assert_eq!(positions[2], i32::MAX, "the sum stops at the maximum");
}

// --- Сдвиг базовой линии `\fay` и центр поворота ---

/// Раскладка с заданным наклоном `\fay` у всех символов.
fn arranged_with_fay(text: &str, fay: f64) -> (Vec<TextCluster>, TextLayout) {
    let style = plain_style();
    let mut fonts = embedded_fonts();
    let mut clusters = shaped_clusters(&mut fonts, &plain_text(text), &style);
    for cluster in &mut clusters {
        cluster.baseline_shear = fay;
    }
    let layout = arrange_text(&mut clusters, &style, canvas());
    (clusters, layout)
}

#[test]
fn fay_accumulates_a_baseline_lift_along_the_line_and_restarts_on_the_next_line() {
    let (flat, flat_layout) = arranged_with_fay("abcd\nefgh", 0.0);
    let (sheared, layout) = arranged_with_fay("abcd\nefgh", 0.5);
    let dy = |index: usize| sheared[index].root.pos.y - flat[index].root.pos.y;
    // Первая буква строки не сдвигается, дальше сдвиг растёт на fay * продвижение.
    assert_eq!(dy(0), 0);
    assert!(
        dy(1) > 0 && dy(2) > dy(1) && dy(3) > dy(2),
        "{:?}",
        (dy(1), dy(2), dy(3))
    );
    let expected = (0.5 * f64::from(flat[0].root.advance.x)) as i32;
    assert_eq!(dy(1), expected);
    // Вторая строка начинается заново (символ 4 — разделитель строк, 5 — первая буква второй).
    assert_eq!(dy(5), 0);
    assert!(dy(6) > 0);
    // Габариты блока считаются до сдвига: рамка и измерения не меняются.
    assert_eq!(layout.bounds, flat_layout.bounds);
    assert_eq!(layout.measure, flat_layout.measure);
}

#[test]
fn rotation_centre_is_the_alignment_base_point_unless_org_is_set() {
    let mut style = plain_style();
    let (_, layout) = arranged("Hello", &style);
    // Нижнее выравнивание по центру: середина по x и нижний край рамки текста.
    let centre = layout.rotation_center(&style);
    let expected_x = layout.device_x + (layout.bounds.x_min + layout.bounds.x_max) / 2.0;
    let expected_y = layout.device_y + layout.bounds.y_max;
    assert!((centre.x - expected_x).abs() < 1e-9, "{centre:?}");
    assert!((centre.y - expected_y).abs() < 1e-9, "{centre:?}");

    // `\org` задаёт точку напрямую и не зависит от раскладки.
    let crate::event_style::EventPlacement::ByMargins = style.placement else {
        panic!("plain style uses margins");
    };
    style.placement =
        crate::event_style::EventPlacement::Exact(crate::event_style::ExactPlacement {
            text_left: 40.0,
            text_width: 1200.0,
            vertical_margin: 40.0,
            anchor: crate::event_style::BlockAnchor::ByAlignment,
            rotation_origin: RotationOrigin::AtPoint { x: 123.0, y: 45.5 },
            pixel_aspect_x: 1.0,
        });
    let with_org = layout.rotation_center(&style);
    assert_eq!((with_org.x, with_org.y), (123.0, 45.5));
}
