//! Таблицы разрешённых и запрещённых разрывов: каждая строка — пара соседних символов.

use super::*;
use crate::test_support::{embedded_fonts, plain_style, plain_text, shaped_clusters};

/// Два соседних кластера из настоящего шейпинга (CJK-глифов во встроенных шрифтах нет —
/// рисуется `.notdef`, но исходный символ в кластере сохраняется, а правилам нужен только он).
fn pair(first: char, second: char) -> Vec<TextCluster> {
    let mut fonts = embedded_fonts();
    let text = format!("{first}{second}");
    let clusters = shaped_clusters(&mut fonts, &plain_text(&text), &plain_style());
    assert_eq!(clusters.len(), 2, "one cluster per source symbol: {text:?}");
    clusters
}

fn breaks(first: char, second: char, policy: BreakOpportunities) -> bool {
    let clusters = pair(first, second);
    allows_break_between(&clusters[0], clusters.get(1), policy)
}

#[test]
fn spaces_only_breaks_after_a_space_and_nowhere_else() {
    let policy = BreakOpportunities::SpacesOnly;
    assert!(breaks(' ', 'a', policy));
    assert!(breaks(' ', '你', policy));
    assert!(!breaks('你', '好', policy));
    assert!(!breaks('a', '你', policy));
    assert!(!breaks('あ', 'い', policy));
}

#[test]
fn cjk_policy_allows_breaks_inside_cjk_text_and_on_its_borders() {
    let policy = BreakOpportunities::SpacesAndCjk;
    let allowed = [
        ('你', '好'),        // иероглифы
        ('あ', 'い'),        // хирагана
        ('カ', 'タ'),        // катакана
        ('한', '글'),        // хангыль
        ('a', '你'),         // латиница -> иероглиф
        ('你', 'a'),         // иероглиф -> латиница
        ('2', '年'),         // цифра -> иероглиф
        ('你', '「'),        // иероглиф -> открывающая скобка
        ('。', '你'),        // после точки можно
        ('\u{20000}', '好'), // дополнительная плоскость
        (' ', 'a'),          // пробел как прежде
    ];
    for (first, second) in allowed {
        assert!(
            breaks(first, second, policy),
            "{first:?}|{second:?} must allow a break"
        );
    }
}

#[test]
fn cjk_policy_keeps_latin_words_unbroken() {
    let policy = BreakOpportunities::SpacesAndCjk;
    assert!(!breaks('a', 'b', policy));
    assert!(!breaks('1', '2', policy));
    // Перед пробелом разрыв не нужен: пробел сам разрешает разрыв после себя.
    assert!(!breaks('你', ' ', policy));
}

#[test]
fn kinsoku_forbids_breaks_before_closing_marks_and_after_opening_marks() {
    let policy = BreakOpportunities::SpacesAndCjk;
    let forbidden = [
        // Нельзя ПЕРЕД:
        ('你', '、'),
        ('你', '。'),
        ('你', '，'),
        ('你', '」'),
        ('你', '）'),
        ('你', '！'),
        ('你', '？'),
        ('你', '…'),
        ('你', '・'),
        ('ア', 'ー'),
        ('你', '々'),
        ('ゝ', 'ゞ'),
        ('あ', 'ぁ'),
        ('か', 'っ'),
        ('カ', 'ッ'),
        ('你', ')'),
        ('你', ','),
        ('你', '.'),
        ('你', '!'),
        ('你', '?'),
        ('你', ':'),
        ('你', ';'),
        ('ｱ', 'ﾞ'),
        ('ｱ', 'ｰ'),
        // Нельзя ПОСЛЕ:
        ('「', '你'),
        ('（', '你'),
        ('【', '你'),
        ('『', '你'),
        ('“', '你'),
        ('(', '你'),
        ('[', '你'),
        ('{', '你'),
    ];
    for (first, second) in forbidden {
        assert!(
            !breaks(first, second, policy),
            "{first:?}|{second:?} must not break"
        );
    }
}

#[test]
fn marks_zero_width_joiner_and_non_breaking_spaces_stay_attached() {
    let policy = BreakOpportunities::SpacesAndCjk;
    for (first, second) in [
        ('か', '\u{3099}'), // дакутэн
        ('你', '\u{0301}'), // комбинирующий акут
        ('你', '\u{FE0F}'), // селектор начертания
        ('你', '\u{200D}'), // ZWJ после основы
        ('\u{200D}', '你'), // ZWJ перед основой
        ('你', '\u{00A0}'), // неразрывный пробел
        ('\u{00A0}', '你'),
    ] {
        assert!(
            !breaks(first, second, policy),
            "{first:?}|{second:?} must stay together"
        );
    }
}

#[test]
fn absorbed_drawing_and_bidi_boundaries_forbid_breaks() {
    let policy = BreakOpportunities::SpacesAndCjk;
    let mut clusters = pair('你', '好');
    assert!(allows_break_between(&clusters[0], clusters.get(1), policy));

    clusters[1].skip = true; // поглощён лигатурой
    assert!(!allows_break_between(&clusters[0], clusters.get(1), policy));
    clusters[1].skip = false;

    clusters[1].bidi_level = 1; // другой уровень BiDi
    assert!(!allows_break_between(&clusters[0], clusters.get(1), policy));
    clusters[1].bidi_level = clusters[0].bidi_level;

    assert!(
        !allows_break_between(&clusters[1], None, policy),
        "no next cluster: nothing to break"
    );
    assert!(allows_break_between(&clusters[0], clusters.get(1), policy));
}
