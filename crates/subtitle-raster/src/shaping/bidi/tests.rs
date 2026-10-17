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

#[test]
fn ascii_fast_paths_match_the_full_bidi_computation() {
    // Управляющие ASCII-символы с особыми классами BiDi: NUL (BN), табуляция и 0x1F (S),
    // 0x1C-0x1E и перевод строки (B, разделители абзацев), 0x7F (BN).
    let texts = [
        "Hello, world 123",
        "a\nb\n\nc",
        "  leading and trailing  ",
        "1 + 2 = 3 (ok) #1 50% $5 a@b.c",
        "",
        "x",
        "a\tb\t",
        "a\u{0}b\u{7f}c",
        "a\u{1c}b\u{1d}c\u{1e}d\u{1f}e",
        "\u{1c}\u{1d}\u{1e}\u{1f}",
        "\u{1f}x\u{1f}",
        "\t\n\r\u{b}\u{c}",
        "end of paragraph\r\nnext",
    ];
    for text in texts {
        let symbols = chars(text);
        assert!(symbols.iter().all(char::is_ascii), "{text:?} is ASCII");
        let length = symbols.len();
        // Границы участков стиля: нет, в одном месте, на каждом символе, в чередовании.
        let mut start_patterns = vec![
            vec![false; length],
            vec![true; length],
            (0..length).map(|index| index % 2 == 1).collect(),
            (0..length).map(|index| index == length / 2).collect(),
        ];
        if length > 3 {
            start_patterns.push((0..length).map(|index| index == 2).collect());
        }
        for starts in &start_patterns {
            for base in [BaseDirection::LeftToRight, BaseDirection::Auto] {
                assert_eq!(
                    resolve_levels(&symbols, starts, base),
                    resolve_levels_in_full(&symbols, starts, base),
                    "{text:?} {starts:?} {base:?}"
                );
            }
        }
        let levels = vec![0_u8; length];
        assert_eq!(
            visual_order(&symbols, &levels, 0),
            visual_order_in_full(&symbols, &levels, 0),
            "{text:?}"
        );
        // Независимо от полного расчёта: в ASCII все уровни равны нулю, порядок логический.
        let fast = resolve_levels(&symbols, &start_patterns[0], BaseDirection::Auto);
        assert_eq!(fast.embedding, levels, "{text:?}");
        assert_eq!(fast.paragraph, levels, "{text:?}");
        assert_eq!(
            visual_order(&symbols, &levels, 0),
            (0..length).collect::<Vec<_>>(),
            "{text:?}"
        );
    }
}

#[test]
fn zero_levels_with_an_rtl_base_still_go_through_the_full_reordering() {
    let symbols = chars("ab c");
    let levels = vec![0_u8; symbols.len()];
    // Базовый уровень 1 сбрасывает хвостовые пробелы на уровень 1: это уже не тождество.
    assert_eq!(
        visual_order(&symbols, &levels, 1),
        visual_order_in_full(&symbols, &levels, 1)
    );
}
