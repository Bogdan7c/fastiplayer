//! Разбор тегов слоя 3: `\blur`, `\be`, `\clip`, `\iclip`, `\pbo` (ass_parse.c libass 0.17.5).

use subtitle_core::{AssClip, AssClipMode, AssClipShape, AssOverride, AssTextItem};

use super::limits::AssParseLimits;
use super::report::AssTagStats;
use super::text::parse_event_text;

/// Максимум тегов на реплику в этих тестах (с большим запасом).
const TEST_MAX_TAGS: usize = 1000;

/// Разбирает текст реплики; возвращает элементы и статистику тегов.
fn parse_text(text: &str) -> (Vec<AssTextItem>, AssTagStats) {
    let limits = AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: TEST_MAX_TAGS,
        max_tracked_unknown_tag_names: 4,
    };
    let mut stats = AssTagStats::new(&limits);
    let items = parse_event_text(text, TEST_MAX_TAGS, &mut stats);
    (items, stats)
}

/// Теги единственного блока `{<block>}`; пустой список, если блок не дал ни одного тега.
fn parse(block: &str) -> (Vec<AssOverride>, AssTagStats) {
    let (items, stats) = parse_text(&format!("{{{block}}}x"));
    let overrides = match items.into_iter().next() {
        Some(AssTextItem::Overrides(overrides)) => overrides,
        _ => Vec::new(),
    };
    (overrides, stats)
}

fn rect(mode: AssClipMode, x0: i32, y0: i32, x1: i32, y1: i32) -> AssOverride {
    AssOverride::Clip(AssClip {
        mode,
        shape: AssClipShape::Rect { x0, y0, x1, y1 },
    })
}

fn vector(mode: AssClipMode, scale: i32, drawing: &str) -> AssOverride {
    AssOverride::Clip(AssClip {
        mode,
        shape: AssClipShape::Vector {
            scale,
            drawing: drawing.to_owned(),
        },
    })
}

#[test]
fn blur_keeps_raw_value_and_no_argument_means_reset() {
    assert_eq!(parse(r"\blur2.5").0, vec![AssOverride::Blur(Some(2.5))]);
    assert_eq!(parse(r"\blur(3)").0, vec![AssOverride::Blur(Some(3.0))]);
    // Зажатие — дело растеризатора (после интерполяции `\t`): парсер хранит как написано.
    assert_eq!(parse(r"\blur-4").0, vec![AssOverride::Blur(Some(-4.0))]);
    assert_eq!(parse(r"\blur250").0, vec![AssOverride::Blur(Some(250.0))]);
    assert_eq!(parse(r"\blur").0, vec![AssOverride::Blur(None)]);
    // Мусор вместо числа: `argtod` даёт 0.
    assert_eq!(parse(r"\blurabc").0, vec![AssOverride::Blur(Some(0.0))]);
}

#[test]
fn edge_blur_keeps_raw_double_and_no_argument_means_reset() {
    assert_eq!(parse(r"\be2").0, vec![AssOverride::EdgeBlur(Some(2.0))]);
    assert_eq!(parse(r"\be1.6").0, vec![AssOverride::EdgeBlur(Some(1.6))]);
    assert_eq!(parse(r"\be-1").0, vec![AssOverride::EdgeBlur(Some(-1.0))]);
    assert_eq!(parse(r"\be").0, vec![AssOverride::EdgeBlur(None)]);
}

#[test]
fn drawing_baseline_offset_is_a_double_and_defaults_to_zero() {
    assert_eq!(
        parse(r"\pbo-5.5").0,
        vec![AssOverride::DrawingBaselineOffset(-5.5)]
    );
    assert_eq!(
        parse(r"\pbo").0,
        vec![AssOverride::DrawingBaselineOffset(0.0)]
    );
}

#[test]
fn four_arguments_make_a_rect_clip_for_both_clip_tags() {
    assert_eq!(
        parse(r"\clip(1,2,30,40)").0,
        vec![rect(AssClipMode::Inside, 1, 2, 30, 40)]
    );
    assert_eq!(
        parse(r"\iclip(1, 2, 30, 40)").0,
        vec![rect(AssClipMode::Outside, 1, 2, 30, 40)]
    );
    // `argtoi32`: дробная часть отбрасывается, мусор даёт 0, порядок углов не нормализуется.
    assert_eq!(
        parse(r"\clip(9.7,x,3,-4)").0,
        vec![rect(AssClipMode::Inside, 9, 0, 3, -4)]
    );
}

#[test]
fn one_or_two_arguments_make_a_vector_clip() {
    assert_eq!(
        parse(r"\clip(m 0 0 l 100 0 100 50)").0,
        vec![vector(AssClipMode::Inside, 1, "m 0 0 l 100 0 100 50")]
    );
    assert_eq!(
        parse(r"\iclip(3,m 0 0 b 1 1 2 2 3 3)").0,
        vec![vector(AssClipMode::Outside, 3, "m 0 0 b 1 1 2 2 3 3")]
    );
    assert_eq!(
        parse(r"\clip( 2 , m 5 5 )").0,
        vec![vector(AssClipMode::Inside, 2, "m 5 5")]
    );
}

#[test]
fn clip_with_unusable_argument_count_is_malformed_and_dropped() {
    for block in [
        r"\clip",
        r"\clip()",
        r"\clip(1,2,3)",
        r"\iclip(1,2,3,4,5)",
        r"\clip(1,m 0 0,2)",
    ] {
        let (overrides, stats) = parse(block);
        assert!(overrides.is_empty(), "{block}: {overrides:?}");
        assert_eq!(stats.malformed(), 1, "{block}");
    }
}

#[test]
fn every_occurrence_is_kept_in_order_because_winner_rules_belong_to_the_renderer() {
    let (overrides, stats) = parse(
        r"\clip(0,0,1,1)\clip(2,2,3,3)\clip(m 0 0)\iclip(m 9 9)\blur1\blur2\be1\be3\pbo1\pbo2",
    );
    assert_eq!(
        overrides,
        vec![
            rect(AssClipMode::Inside, 0, 0, 1, 1),
            rect(AssClipMode::Inside, 2, 2, 3, 3),
            vector(AssClipMode::Inside, 1, "m 0 0"),
            vector(AssClipMode::Outside, 1, "m 9 9"),
            AssOverride::Blur(Some(1.0)),
            AssOverride::Blur(Some(2.0)),
            AssOverride::EdgeBlur(Some(1.0)),
            AssOverride::EdgeBlur(Some(3.0)),
            AssOverride::DrawingBaselineOffset(1.0),
            AssOverride::DrawingBaselineOffset(2.0),
        ]
    );
    assert_eq!(stats.malformed(), 0);
}

#[test]
fn prefix_traps_resolve_to_the_right_tag() {
    // `\iclip` не становится `\i`, `\blur` — `\b`, `\be` — `\b`, `\pbo` — `\p`, `\clip` — `\c`.
    let (overrides, stats) = parse(r"\iclip(1,2,3,4)\blur1\be1\pbo2\clip(1,2,3,4)\bord3\b1\p1");
    assert_eq!(overrides.len(), 8);
    assert!(matches!(overrides[0], AssOverride::Clip(_)));
    assert!(matches!(overrides[1], AssOverride::Blur(_)));
    assert!(matches!(overrides[2], AssOverride::EdgeBlur(_)));
    assert!(matches!(
        overrides[3],
        AssOverride::DrawingBaselineOffset(_)
    ));
    assert!(matches!(overrides[4], AssOverride::Clip(_)));
    assert!(matches!(overrides[5], AssOverride::Border { .. }));
    assert!(matches!(overrides[6], AssOverride::Bold(_)));
    assert!(matches!(overrides[7], AssOverride::Drawing(_)));
    assert_eq!(stats.unknown_total(), 0);
    assert_eq!(stats.malformed(), 0);
}

#[test]
fn full_text_with_layer_three_tags_keeps_the_visible_text() {
    let (items, stats) = parse_text(r"{\blur2\clip(1,2,3,4)}Hi{\p1}m 0 0 l 5 5{\p0}");
    assert!(items.contains(&AssTextItem::Text("Hi".to_owned())));
    assert_eq!(stats.malformed(), 0);
}
