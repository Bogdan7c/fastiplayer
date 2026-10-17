//! Тесты разбора тегов геометрии слоя 2 (`\frx \fry \frz \fr \fax \fay \org \fad \fade`):
//! число аргументов, умолчания «тег без аргумента», счётчики отчёта и жёсткие переопределения.

use subtitle_core::{
    AssFade, AssHardOverrides, AssOverride, AssRotationAxis, AssShearAxis, AssTextItem,
    AssUnsupportedTag,
};

use super::hard_overrides::scan_hard_overrides;
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

/// Теги первого блока реплики.
fn tags_of(text: &str) -> Vec<AssOverride> {
    match parse(text).0.into_iter().next() {
        Some(AssTextItem::Overrides(tags)) => tags,
        other => panic!("ожидался блок тегов, получено {other:?}"),
    }
}

fn rotation(axis: AssRotationAxis, degrees: Option<f64>) -> AssOverride {
    AssOverride::Rotation { axis, degrees }
}

#[test]
fn rotation_tags_keep_axis_and_degrees() {
    assert_eq!(
        tags_of(r"{\frx30\fry-45.5\frz90\fr12}x"),
        vec![
            rotation(AssRotationAxis::X, Some(30.0)),
            rotation(AssRotationAxis::Y, Some(-45.5)),
            rotation(AssRotationAxis::Z, Some(90.0)),
            rotation(AssRotationAxis::Z, Some(12.0)),
        ]
    );
}

#[test]
fn rotation_without_argument_is_recorded_as_reset_request() {
    // X и Y сбрасываются в 0, Z — в угол стиля: решает растеризатор по `None`.
    assert_eq!(
        tags_of(r"{\frx\fry\frz\fr}x"),
        vec![
            rotation(AssRotationAxis::X, None),
            rotation(AssRotationAxis::Y, None),
            rotation(AssRotationAxis::Z, None),
            rotation(AssRotationAxis::Z, None),
        ]
    );
}

#[test]
fn rotation_accepts_parenthesised_argument_and_garbage_as_zero() {
    assert_eq!(
        tags_of(r"{\frz(33)\frxabc}x"),
        vec![
            rotation(AssRotationAxis::Z, Some(33.0)),
            rotation(AssRotationAxis::X, Some(0.0)),
        ]
    );
}

#[test]
fn shear_tags_parse_factor_and_reset() {
    assert_eq!(
        tags_of(r"{\fax0.5\fay-1\fax\fay}x"),
        vec![
            AssOverride::Shear {
                axis: AssShearAxis::X,
                factor: Some(0.5)
            },
            AssOverride::Shear {
                axis: AssShearAxis::Y,
                factor: Some(-1.0)
            },
            AssOverride::Shear {
                axis: AssShearAxis::X,
                factor: None
            },
            AssOverride::Shear {
                axis: AssShearAxis::Y,
                factor: None
            },
        ]
    );
}

#[test]
fn origin_needs_exactly_two_arguments() {
    assert_eq!(
        tags_of(r"{\org(100, 200.5)}x"),
        vec![AssOverride::Origin { x: 100.0, y: 200.5 }]
    );
    for broken in [r"{\org(1)}x", r"{\org(1,2,3)}x", r"{\org}x"] {
        let (items, stats) = parse(broken);
        assert!(no_tag_blocks(&items), "{broken}: {items:?}");
        assert_eq!(stats.malformed(), 1, "{broken}");
    }
}

/// В реплике не осталось блоков тегов (неверный тег отброшен, пустой блок не создаётся).
fn no_tag_blocks(items: &[AssTextItem]) -> bool {
    !items
        .iter()
        .any(|item| matches!(item, AssTextItem::Overrides(_)))
}

#[test]
fn simple_fade_keeps_raw_in_and_out() {
    assert_eq!(
        tags_of(r"{\fad(300, 700)}x"),
        vec![AssOverride::Fade(AssFade::Simple {
            fade_in_ms: 300,
            fade_out_ms: 700
        })]
    );
}

#[test]
fn full_fade_keeps_alphas_and_times_in_order() {
    assert_eq!(
        tags_of(r"{\fade(255,0,128,10,20,30,40)}x"),
        vec![AssOverride::Fade(AssFade::Full {
            alphas: [255, 0, 128],
            times_ms: [10, 20, 30, 40]
        })]
    );
}

#[test]
fn fade_with_other_argument_counts_is_malformed_and_dropped() {
    for broken in [
        r"{\fad(1)}x",
        r"{\fad(1,2,3)}x",
        r"{\fade(1,2,3,4,5,6)}x",
        r"{\fade(1,2,3,4,5,6,7,8)}x",
    ] {
        let (items, stats) = parse(broken);
        assert!(no_tag_blocks(&items), "{broken}: {items:?}");
        assert_eq!(stats.malformed(), 1, "{broken}");
    }
}

#[test]
fn repeated_fade_and_origin_are_all_kept_in_raw_order() {
    // «Первый побеждает» — правило растеризатора: парсер хранит исходный порядок.
    let tags = tags_of(r"{\fad(1,2)\fad(3,4)\org(1,2)\org(3,4)}x");
    assert_eq!(tags.len(), 4);
    assert_eq!(
        tags.first(),
        Some(&AssOverride::Fade(AssFade::Simple {
            fade_in_ms: 1,
            fade_out_ms: 2
        }))
    );
    assert_eq!(tags.get(3), Some(&AssOverride::Origin { x: 3.0, y: 4.0 }));
}

#[test]
fn geometry_tags_no_longer_count_as_unsupported() {
    let (_, stats) =
        parse(r"{\frx1\fry2\frz3\fr4\fax5\fay6\org(1,2)\fad(1,2)\fade(1,2,3,4,5,6,7)}x");
    assert!(stats.unsupported().is_empty(), "{:?}", stats.unsupported());
    assert_eq!(stats.unknown_total(), 0);
    assert_eq!(stats.malformed(), 0);
}

#[test]
fn prefix_neighbours_still_resolve_to_their_own_tags() {
    // `\fr` не должен «съедать» `\frx`, `\fs` — `\fscx`, `\fad` — `\fade`.
    let (_, stats) = parse(r"{\frx1\fscx50\fs20\fade(1,2,3,4,5,6,7)\fad(1,2)\k1}x");
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Karaoke), 1);
    assert_eq!(stats.unknown_total(), 0);
}

#[test]
fn org_stays_a_hard_override_for_the_raw_text_scan() {
    assert_eq!(
        scan_hard_overrides(r"{\org(1,2)\frz30}x"),
        AssHardOverrides::Present
    );
    assert_eq!(
        scan_hard_overrides(r"{\frz30\fax1\fad(1,2)}x"),
        AssHardOverrides::Absent
    );
}
