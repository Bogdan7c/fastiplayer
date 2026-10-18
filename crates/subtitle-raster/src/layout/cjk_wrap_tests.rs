//! Перенос строк CJK-текста без пробелов: раскладка целиком, от кластеров до границ строк.

use super::*;
use crate::event_style::BreakOpportunities;
use crate::overlay::CanvasSize;
use crate::test_support::{embedded_fonts, plain_style, plain_text, shaped_clusters};

fn arranged_with(text: &str, policy: BreakOpportunities) -> (Vec<TextCluster>, TextLayout) {
    let mut style = plain_style();
    style.break_opportunities = policy;
    let mut fonts = embedded_fonts();
    let mut clusters = shaped_clusters(&mut fonts, &plain_text(text), &style);
    let canvas = CanvasSize::new(1280, 720).expect("canvas");
    let layout = arrange_text(&mut clusters, &style, canvas);
    (clusters, layout)
}

/// Символы, с которых начинается каждая строка, кроме первой.
fn line_leaders(clusters: &[TextCluster]) -> Vec<char> {
    clusters
        .iter()
        .filter(|cluster| cluster.line_start.begins_line())
        .map(|cluster| cluster.symbol)
        .collect()
}

/// Длинный текст с пунктуацией, знаком долготы и комбинирующим дакутэном; `shift` сдвигает
/// позиции символов, чтобы разрыв попадал на разные места.
fn punctuated_text(shift: usize) -> String {
    let mut text = "あ".repeat(shift);
    for _ in 0..8 {
        text.push_str("今日は、天気がいい。「コーヒー」ーか\u{3099}好きです。");
    }
    text
}

#[test]
fn long_cjk_text_wraps_into_several_lines_inside_the_text_area() {
    let text = "你好世界".repeat(28); // 112 символов
    let (_, spaces_only) = arranged_with(&text, BreakOpportunities::SpacesOnly);
    assert_eq!(
        spaces_only.measure.lines.len(),
        1,
        "libass behaviour: no break points"
    );

    let (clusters, layout) = arranged_with(&text, BreakOpportunities::SpacesAndCjk);
    assert!(
        layout.measure.lines.len() >= 2,
        "lines: {}",
        layout.measure.lines.len()
    );
    // Все строки укладываются в область текста 40..1240.
    let left = layout.device_x + layout.bounds.x_min;
    let right = layout.device_x + layout.bounds.x_max;
    assert!(left >= 39.0 && right <= 1241.0, "block {left}..{right}");
    assert!(
        clusters
            .iter()
            .any(|cluster| cluster.line_start.begins_line())
    );
}

#[test]
fn no_line_starts_with_closing_punctuation_or_a_detached_mark() {
    for shift in 0..45 {
        let (clusters, layout) =
            arranged_with(&punctuated_text(shift), BreakOpportunities::SpacesAndCjk);
        assert!(layout.measure.lines.len() >= 2, "shift {shift}");
        for leader in line_leaders(&clusters) {
            assert!(
                !"、。」ー\u{3099}ぁっ".contains(leader),
                "shift {shift}: line starts with {leader:?}"
            );
        }
    }
}

#[test]
fn no_line_ends_with_an_opening_bracket() {
    for shift in 0..45 {
        let text = format!("{}{}", "あ".repeat(shift), "「テスト」".repeat(30));
        let (clusters, _) = arranged_with(&text, BreakOpportunities::SpacesAndCjk);
        for (index, cluster) in clusters.iter().enumerate() {
            if cluster.line_start.begins_line() && index > 0 {
                assert_ne!(clusters[index - 1].symbol, '「', "shift {shift}");
            }
        }
    }
}

#[test]
fn rebalance_avoids_a_one_character_last_line() {
    // Ширина символа берётся из шейпинга: ровно на одну строку + 1 символ текста.
    let (probe, _) = arranged_with("你", BreakOpportunities::SpacesAndCjk);
    let advance = crate::fixed_point::d6_to_double(probe[0].cluster_advance.x);
    assert!(advance > 1.0, "advance {advance}");
    let per_line = (1200.0 / advance).floor() as usize;
    let (clusters, layout) =
        arranged_with(&"你".repeat(per_line + 1), BreakOpportunities::SpacesAndCjk);
    assert_eq!(layout.measure.lines.len(), 2);
    let second_line_start = clusters
        .iter()
        .position(|cluster| cluster.line_start.begins_line())
        .expect("second line");
    // Без выравнивания на второй строке остался бы один символ; после него — примерно поровну.
    let second_len = clusters.len() - second_line_start;
    assert!(second_len > 2, "second line has {second_len} symbol(s)");
}

#[test]
fn latin_text_with_spaces_wraps_identically_under_both_policies() {
    let text = "The quick brown fox jumps over the lazy dog and keeps running far away. ".repeat(4);
    let (old, _) = arranged_with(&text, BreakOpportunities::SpacesOnly);
    let (new, _) = arranged_with(&text, BreakOpportunities::SpacesAndCjk);
    let starts = |clusters: &[TextCluster]| -> Vec<_> {
        clusters.iter().map(|cluster| cluster.line_start).collect()
    };
    assert!(
        starts(&old).iter().any(|start| start.begins_line()),
        "text must wrap"
    );
    assert_eq!(starts(&old), starts(&new));
}

#[test]
fn explicit_line_break_still_works_in_cjk_text() {
    let (clusters, layout) = arranged_with("你好\n世界", BreakOpportunities::SpacesAndCjk);
    assert_eq!(layout.measure.lines.len(), 2);
    assert!(clusters[3].line_start.begins_line());
}
