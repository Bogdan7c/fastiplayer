//! Разбор тегов караоке `\k \K \kf \ko \kt` (ass_parse.c libass 0.17.5): вид слога, сырая
//! длительность, «тег без аргумента» и ловушки префиксов.

use subtitle_core::{AssKaraoke, AssKaraokeKind, AssOverride, AssTextItem};

use super::limits::AssParseLimits;
use super::report::AssTagStats;
use super::text::parse_event_text;

fn parse(text: &str) -> (Vec<AssTextItem>, AssTagStats) {
    let limits = AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: 1000,
        max_tracked_unknown_tag_names: 4,
    };
    let mut stats = AssTagStats::new(&limits);
    let items = parse_event_text(text, 1000, &mut stats);
    (items, stats)
}

/// Теги первого блока `{<block>}x`, без неизвестных и кривых.
fn tags_of(block: &str) -> Vec<AssOverride> {
    let (items, stats) = parse(&format!("{{{block}}}x"));
    assert_eq!(stats.unknown_total(), 0, "{block}");
    assert_eq!(stats.malformed(), 0, "{block}");
    match items.into_iter().next() {
        Some(AssTextItem::Overrides(tags)) => tags,
        other => panic!("expected an override block, got {other:?}"),
    }
}

fn karaoke(kind: AssKaraokeKind, centiseconds: Option<f64>) -> AssOverride {
    AssOverride::Karaoke(AssKaraoke { kind, centiseconds })
}

#[test]
fn each_tag_maps_to_its_kind_and_keeps_the_raw_duration() {
    assert_eq!(
        tags_of(r"\k20"),
        vec![karaoke(AssKaraokeKind::Plain, Some(20.0))]
    );
    assert_eq!(
        tags_of(r"\kf30"),
        vec![karaoke(AssKaraokeKind::Fill, Some(30.0))]
    );
    assert_eq!(
        tags_of(r"\K40"),
        vec![karaoke(AssKaraokeKind::Fill, Some(40.0))]
    );
    assert_eq!(
        tags_of(r"\ko50"),
        vec![karaoke(AssKaraokeKind::Outline, Some(50.0))]
    );
}

#[test]
fn duration_is_a_raw_double_not_rounded_by_the_parser() {
    assert_eq!(
        tags_of(r"\k1.55"),
        vec![karaoke(AssKaraokeKind::Plain, Some(1.55))]
    );
    assert_eq!(
        tags_of(r"\k-5"),
        vec![karaoke(AssKaraokeKind::Plain, Some(-5.0))]
    );
    assert_eq!(
        tags_of(r"\k(7)"),
        vec![karaoke(AssKaraokeKind::Plain, Some(7.0))]
    );
    // Мусор вместо числа при непустом аргументе: `argtod` даёт 0 (это не «без аргумента»).
    assert_eq!(
        tags_of(r"\kfxyz"),
        vec![karaoke(AssKaraokeKind::Fill, Some(0.0))]
    );
}

#[test]
fn missing_argument_is_none_so_the_rasterizer_uses_one_second() {
    for (block, kind) in [
        (r"\k", AssKaraokeKind::Plain),
        (r"\K", AssKaraokeKind::Fill),
        (r"\kf", AssKaraokeKind::Fill),
        (r"\ko", AssKaraokeKind::Outline),
    ] {
        assert_eq!(tags_of(block), vec![karaoke(kind, None)], "{block}");
    }
}

#[test]
fn skip_tag_keeps_the_raw_shift_and_none_without_argument() {
    assert_eq!(
        tags_of(r"\kt25"),
        vec![AssOverride::KaraokeSkip(Some(25.0))]
    );
    assert_eq!(
        tags_of(r"\kt-3.5"),
        vec![AssOverride::KaraokeSkip(Some(-3.5))]
    );
    assert_eq!(tags_of(r"\kt"), vec![AssOverride::KaraokeSkip(None)]);
}

#[test]
fn prefix_order_is_kt_kf_capital_k_ko_k() {
    // `\kt` не должен стать `\k` с хвостом «t», а `\kf`/`\ko` — `\k` с хвостом «f»/«o».
    assert_eq!(
        tags_of(r"\kt5\kf10\ko20\k30\K40"),
        vec![
            AssOverride::KaraokeSkip(Some(5.0)),
            karaoke(AssKaraokeKind::Fill, Some(10.0)),
            karaoke(AssKaraokeKind::Outline, Some(20.0)),
            karaoke(AssKaraokeKind::Plain, Some(30.0)),
            karaoke(AssKaraokeKind::Fill, Some(40.0)),
        ]
    );
}

#[test]
fn karaoke_tags_between_text_runs_keep_their_position() {
    let (items, stats) = parse(r"{\k10}Ka{\k20}ra{\kf30\k40}ok");
    assert_eq!(stats.unknown_total(), 0);
    assert_eq!(
        items,
        vec![
            AssTextItem::Overrides(vec![karaoke(AssKaraokeKind::Plain, Some(10.0))]),
            AssTextItem::Text("Ka".to_owned()),
            AssTextItem::Overrides(vec![karaoke(AssKaraokeKind::Plain, Some(20.0))]),
            AssTextItem::Text("ra".to_owned()),
            AssTextItem::Overrides(vec![
                karaoke(AssKaraokeKind::Fill, Some(30.0)),
                karaoke(AssKaraokeKind::Plain, Some(40.0)),
            ]),
            AssTextItem::Text("ok".to_owned()),
        ]
    );
}

#[test]
fn karaoke_inside_a_transform_is_parsed_as_a_nested_tag() {
    let tags = tags_of(r"\t(\k10)");
    let [AssOverride::Transform(transform)] = tags.as_slice() else {
        panic!("expected one transform, got {tags:?}");
    };
    assert_eq!(
        transform.tags,
        vec![karaoke(AssKaraokeKind::Plain, Some(10.0))]
    );
}
