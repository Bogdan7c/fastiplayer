use super::*;
use crate::layout::arrange_text;
use crate::overlay::CanvasSize;
use crate::test_support::{embedded_fonts, plain_style, plain_text, shaped_clusters};

fn boxes_of(text: &str) -> Vec<LineBox> {
    let style = plain_style();
    let canvas = CanvasSize::new(1280, 720).expect("canvas");
    let mut fonts = embedded_fonts();
    let mut clusters = shaped_clusters(&mut fonts, &plain_text(text), &style);
    let layout = arrange_text(&mut clusters, &style, canvas);
    line_boxes(&clusters, &layout, &style)
}

#[test]
fn single_line_box_is_centered_and_sits_on_the_bottom_margin() {
    let boxes = boxes_of("Hello");
    assert_eq!(boxes.len(), 1);
    let line = boxes[0];
    assert!(
        ((line.left + line.right) / 2.0 - 640.0).abs() < 3.0,
        "{line:?}"
    );
    assert!(line.right - line.left > 50.0, "{line:?}");
    // Низ ячейки стоит на `высота - поле` = 680.
    assert!((line.bottom - 680.0).abs() < 0.01, "{line:?}");
    assert!(
        (line.bottom - line.top - 48.0).abs() < 1.0,
        "cell is the font size: {line:?}"
    );
}

#[test]
fn two_lines_stack_without_gap_and_each_has_its_own_width() {
    let boxes = boxes_of("Hi\nmuch longer second");
    assert_eq!(boxes.len(), 2);
    assert!((boxes[0].bottom - boxes[1].top).abs() < 0.5, "{boxes:?}");
    let first_width = boxes[0].right - boxes[0].left;
    let second_width = boxes[1].right - boxes[1].left;
    assert!(
        second_width > first_width * 3.0,
        "{first_width} vs {second_width}"
    );
}

#[test]
fn blank_line_between_text_gets_no_box() {
    let boxes = boxes_of("a\n\nb");
    assert_eq!(boxes.len(), 2, "{boxes:?}");
    // Между строками с буквами лежит пустая строка: зазор заметно больше нуля.
    assert!(boxes[1].top - boxes[0].bottom > 5.0, "{boxes:?}");
}
