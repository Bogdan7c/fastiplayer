//! Текст рисунка `\p<n>` в разборе реплики: сырой кусок без escape-последовательностей
//! (ass_render.c libass 0.17.5, `parse_events`, ветка `drawing_scale`).

use subtitle_core::{AssDrawingMode, AssOverride, AssTextItem};

use super::limits::AssParseLimits;
use super::report::AssTagStats;
use super::text::parse_event_text;

fn parse_text(text: &str) -> Vec<AssTextItem> {
    let limits = AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: 1000,
        max_tracked_unknown_tag_names: 4,
    };
    let mut stats = AssTagStats::new(&limits);
    parse_event_text(text, 1000, &mut stats)
}

fn drawing_on(scale: u32) -> AssTextItem {
    AssTextItem::Overrides(vec![AssOverride::Drawing(AssDrawingMode::On { scale })])
}

fn text(value: &str) -> AssTextItem {
    AssTextItem::Text(value.to_owned())
}

#[test]
fn escapes_inside_a_drawing_stay_raw_text() {
    // Вне рисунка `\N` — перенос, `\h` — неразрывный пробел, табуляция — пробел.
    assert_eq!(
        parse_text("a\\Nb\\hc\td"),
        [text("a"), AssTextItem::HardBreak, text("b\u{a0}c d"),]
    );
    // Внутри рисунка те же символы остаются как есть.
    assert_eq!(
        parse_text("{\\p1}m 0 0\\N l 1 1\\h\t2 2{\\p0}"),
        [
            drawing_on(1),
            text("m 0 0\\N l 1 1\\h\t2 2"),
            AssTextItem::Overrides(vec![AssOverride::Drawing(AssDrawingMode::Off)]),
        ]
    );
}

#[test]
fn text_after_p0_is_regular_text_again() {
    let items = parse_text("{\\p1}m 0 0{\\p0}a\\Nb");
    assert_eq!(items.len(), 6);
    assert_eq!(items[1], text("m 0 0"));
    assert_eq!(items[3], text("a"));
    assert_eq!(items[4], AssTextItem::HardBreak);
    assert_eq!(items[5], text("b"));
}

#[test]
fn reset_tag_does_not_leave_drawing_mode() {
    // `\r` не трогает `\p`: следующий текст по-прежнему рисунок.
    let items = parse_text("{\\p1}m 0 0{\\r}l 5\\N5");
    assert_eq!(items.last(), Some(&text("l 5\\N5")));
}

#[test]
fn unmatched_brace_starts_a_new_drawing_piece_like_libass() {
    // `{` без `}` — не блок: кусок рисунка заканчивается перед ней, а сама она начинает
    // следующий кусок (каждый кусок в libass — отдельный псевдоглиф).
    assert_eq!(
        parse_text("{\\p1}m 0 0{ l 1 1"),
        [drawing_on(1), text("m 0 0"), text("{ l 1 1")]
    );
}

#[test]
fn p_with_a_non_positive_scale_is_not_a_drawing() {
    assert_eq!(
        parse_text("{\\p-1}a\\Nb"),
        [
            AssTextItem::Overrides(vec![AssOverride::Drawing(AssDrawingMode::Off)]),
            text("a"),
            AssTextItem::HardBreak,
            text("b"),
        ]
    );
}

#[test]
fn last_p_in_a_block_wins() {
    assert_eq!(
        parse_text("{\\p1\\p0}a\\Nb")[1..],
        [text("a"), AssTextItem::HardBreak, text("b")]
    );
}
