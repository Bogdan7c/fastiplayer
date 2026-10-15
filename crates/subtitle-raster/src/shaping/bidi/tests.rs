use super::*;

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

#[test]
fn latin_text_stays_at_level_zero() {
    let symbols = chars("Hello");
    let levels = resolve_levels(
        &symbols,
        &[true, false, false, false, false],
        BaseDirection::LeftToRight,
    );
    assert_eq!(levels.embedding, vec![0; 5]);
    assert_eq!(levels.paragraph, vec![0; 5]);
}

#[test]
fn hebrew_inside_ltr_paragraph_gets_level_one() {
    let symbols = chars("abc אבג");
    let starts = vec![false; symbols.len()];
    let levels = resolve_levels(&symbols, &starts, BaseDirection::LeftToRight);
    assert_eq!(levels.embedding, vec![0, 0, 0, 0, 1, 1, 1]);
    assert_eq!(levels.paragraph, vec![0; 7]);
}

#[test]
fn auto_direction_takes_the_first_strong_letter() {
    let symbols = chars("אבג abc");
    let starts = vec![false; symbols.len()];
    let levels = resolve_levels(&symbols, &starts, BaseDirection::Auto);
    assert!(
        levels.paragraph.iter().all(|level| *level == 1),
        "paragraph is RTL"
    );
    assert_eq!(levels.embedding[0], 1);
    assert_eq!(
        levels.embedding[4], 2,
        "Latin inside RTL paragraph is one level higher"
    );
}

#[test]
fn newline_separates_paragraphs_with_independent_direction() {
    let symbols = chars("אב\nab");
    let starts = vec![false; symbols.len()];
    let levels = resolve_levels(&symbols, &starts, BaseDirection::Auto);
    assert_eq!(levels.paragraph[0], 1);
    assert_eq!(levels.paragraph[3], 0, "second paragraph starts with Latin");
}

#[test]
fn style_boundary_splits_paragraphs_only_for_ltr_base() {
    // «א» и «ב» в разных стилях: при базе LTR каждый участок — свой абзац.
    let symbols = chars("אב");
    let starts = [true, true];
    let ltr = resolve_levels(&symbols, &starts, BaseDirection::LeftToRight);
    assert_eq!(ltr.embedding, vec![1, 1]);
    let auto = resolve_levels(&symbols, &starts, BaseDirection::Auto);
    assert_eq!(auto.paragraph, vec![1, 1]);
}

#[test]
fn visual_order_reverses_rtl_run_inside_ltr_line() {
    let symbols = chars("abc אבג");
    let levels = [0, 0, 0, 0, 1, 1, 1];
    assert_eq!(
        visual_order(&symbols, &levels, 0),
        vec![0, 1, 2, 3, 6, 5, 4]
    );
}

#[test]
fn visual_order_places_rtl_paragraph_right_to_left() {
    // Строка RTL с латиницей внутри: латинский кусок сохраняет внутренний порядок.
    let symbols = chars("אב ab");
    let levels = [1, 1, 1, 2, 2];
    assert_eq!(visual_order(&symbols, &levels, 1), vec![3, 4, 2, 1, 0]);
}

#[test]
fn trailing_whitespace_is_reset_to_paragraph_level() {
    // Пробел в конце RTL-строки уходит на базовый уровень (левый край), а не разворачивается.
    let symbols = chars("אב ");
    let levels = [1, 1, 1];
    let order = visual_order(&symbols, &levels, 0);
    assert_eq!(
        order,
        vec![1, 0, 2],
        "space stays after the Hebrew run in an LTR line"
    );
}

#[test]
fn empty_line_has_empty_order() {
    assert!(visual_order(&[], &[], 0).is_empty());
}
