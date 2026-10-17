//! Разбор тега `\t(...)` (ass_parse.c libass 0.17.5, ветка `complex_tag("t")`): четыре формы
//! аргументов, вложенные теги, некорректные формы, «первая `)`» и бюджет тегов реплики.

use subtitle_core::{
    AssAxes, AssClip, AssClipMode, AssClipShape, AssFontSize, AssOverride, AssTextItem,
    AssTransform, AssTransformTiming,
};

use super::limits::AssParseLimits;
use super::report::AssTagStats;
use super::text::parse_event_text;
use super::transform_tags::MAX_TRANSFORM_NESTING;

/// Максимум тегов на реплику в тестах, где бюджет не проверяется (с большим запасом).
const GENEROUS_BUDGET: usize = 1000;

/// Разбирает текст реплики с заданным бюджетом тегов.
fn parse_with_budget(text: &str, max_tags: usize) -> (Vec<AssTextItem>, AssTagStats) {
    let limits = AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: max_tags,
        max_tracked_unknown_tag_names: 4,
    };
    let mut stats = AssTagStats::new(&limits);
    let items = parse_event_text(text, max_tags, &mut stats);
    (items, stats)
}

/// Теги первого блока `{<block>}x`.
fn parse(block: &str) -> (Vec<AssOverride>, AssTagStats) {
    let (items, stats) = parse_with_budget(&format!("{{{block}}}x"), GENEROUS_BUDGET);
    let overrides = match items.into_iter().next() {
        Some(AssTextItem::Overrides(overrides)) => overrides,
        _ => Vec::new(),
    };
    (overrides, stats)
}

/// Единственный `\t` блока.
fn single_transform(block: &str) -> AssTransform {
    let (mut overrides, stats) = parse(block);
    assert_eq!(overrides.len(), 1, "{overrides:?}");
    assert_eq!(stats.unknown_total(), 0, "{block}");
    assert_eq!(stats.malformed(), 0, "{block}");
    match overrides.remove(0) {
        AssOverride::Transform(transform) => transform,
        other => panic!("expected a transform, got {other:?}"),
    }
}

fn font_size(value: f64) -> AssOverride {
    AssOverride::FontSize(AssFontSize::Absolute(value))
}

#[test]
fn only_tags_means_default_timing() {
    let transform = single_transform(r"\t(\fs50)");
    assert_eq!(transform.timing, AssTransformTiming::Default);
    assert_eq!(transform.tags, vec![font_size(50.0)]);
}

#[test]
fn one_number_is_acceleration() {
    let transform = single_transform(r"\t(0.5,\fs50)");
    assert_eq!(transform.timing, AssTransformTiming::Accel(0.5));
    assert_eq!(transform.tags, vec![font_size(50.0)]);
    // Мусор вместо ускорения: `argtod` даёт 0.
    let garbage = single_transform(r"\t(abc,\fs50)");
    assert_eq!(garbage.timing, AssTransformTiming::Accel(0.0));
}

#[test]
fn two_numbers_are_fractional_times_truncated_toward_zero() {
    let transform = single_transform(r"\t(100,200,\fs50)");
    assert_eq!(
        transform.timing,
        AssTransformTiming::Range {
            t1_ms: 100,
            t2_ms: 200
        }
    );
    // VSFilter читает времена как дробные: `dtoi32(argtod)`, экспоненты допустимы.
    let fractional = single_transform(r"\t(100.9,2e2,\fs50)");
    assert_eq!(
        fractional.timing,
        AssTransformTiming::Range {
            t1_ms: 100,
            t2_ms: 200
        }
    );
    // Усечение к нулю, а не к минус бесконечности.
    let negative = single_transform(r"\t(-1.9,250.5,\fs50)");
    assert_eq!(
        negative.timing,
        AssTransformTiming::Range {
            t1_ms: -1,
            t2_ms: 250
        }
    );
}

#[test]
fn out_of_range_fractional_times_become_int_min_like_dtoi32() {
    let transform = single_transform(r"\t(1e20,-1e20,\fs50)");
    assert_eq!(
        transform.timing,
        AssTransformTiming::Range {
            t1_ms: i32::MIN,
            t2_ms: i32::MIN
        }
    );
}

#[test]
fn three_numbers_are_integer_times_and_acceleration() {
    let transform = single_transform(r"\t(100,200,0.5,\fs50)");
    assert_eq!(
        transform.timing,
        AssTransformTiming::RangeAccel {
            t1_ms: 100,
            t2_ms: 200,
            accel: 0.5
        }
    );
    // С тремя числами времена целые (`argtoi32`): после точки/экспоненты разбор останавливается.
    let integer_only = single_transform(r"\t(100.9,2e2,3,\fs50)");
    assert_eq!(
        integer_only.timing,
        AssTransformTiming::RangeAccel {
            t1_ms: 100,
            t2_ms: 2,
            accel: 3.0
        }
    );
}

#[test]
fn zero_end_time_is_kept_raw_for_the_rasterizer() {
    // `t2 == 0` libass превращает в длительность реплики; это знание о времени реплики, поэтому
    // парсер хранит ноль как есть.
    let transform = single_transform(r"\t(500,0,\fs50)");
    assert_eq!(
        transform.timing,
        AssTransformTiming::Range {
            t1_ms: 500,
            t2_ms: 0
        }
    );
}

#[test]
fn empty_arguments_do_not_count_like_push_arg() {
    // Пустой первый аргумент не сохраняется, поэтому `500` становится ускорением (cnt == 1).
    let transform = single_transform(r"\t(,500,\fs50)");
    assert_eq!(transform.timing, AssTransformTiming::Accel(500.0));
}

#[test]
fn spaces_around_arguments_are_tolerated() {
    let transform = single_transform(r"\t( 10 , 20 , \fs50 )");
    assert_eq!(
        transform.timing,
        AssTransformTiming::Range {
            t1_ms: 10,
            t2_ms: 20
        }
    );
    assert_eq!(transform.tags, vec![font_size(50.0)]);
}

#[test]
fn nested_tags_keep_their_order_and_arguments() {
    let transform = single_transform(r"\t(0,100,\fs50\bord2\fscx150)");
    assert_eq!(transform.tags.len(), 3);
    assert_eq!(transform.tags[0], font_size(50.0));
    assert_eq!(
        transform.tags[1],
        AssOverride::Border {
            axes: AssAxes::Both,
            value: Some(2.0)
        }
    );
    assert_eq!(transform.tags[2], AssOverride::ScaleX(Some(1.5)));
}

#[test]
fn nested_transform_is_kept_inside_the_outer_one() {
    // Первая `)` обрывает внешний аргумент, поэтому внутренний `\t` не закрыт: его тег
    // `\bord9` — единственное содержимое. Последняя `)` внешнего разбора — просто мусор.
    let outer = single_transform(r"\t(0,100,\fs50\t(\bord9))");
    assert_eq!(outer.tags.len(), 2);
    assert_eq!(outer.tags[0], font_size(50.0));
    assert_eq!(
        outer.tags[1],
        AssOverride::Transform(AssTransform {
            timing: AssTransformTiming::Default,
            tags: vec![AssOverride::Border {
                axes: AssAxes::Both,
                value: Some(9.0)
            }],
        })
    );
}

#[test]
fn invalid_forms_are_kept_as_empty_transforms() {
    // Они всё равно отключают поиск места реплики и заменяют коэффициент внешнего `\t`.
    for block in [
        r"\t",
        r"\t()",
        r"\t(1,2,3,4,\fs50)",
        r"\t(1,2,3,4,5,6,\fs50)",
    ] {
        let transform = single_transform(block);
        assert_eq!(transform.timing, AssTransformTiming::Default, "{block}");
        assert!(transform.tags.is_empty(), "{block}: {:?}", transform.tags);
    }
}

#[test]
fn form_without_backslash_applies_nothing_but_keeps_timing() {
    let one = single_transform(r"\t(300)");
    assert_eq!(one.timing, AssTransformTiming::Default);
    assert!(one.tags.is_empty());
    let range = single_transform(r"\t(1,2,3)");
    assert_eq!(
        range.timing,
        AssTransformTiming::Range { t1_ms: 1, t2_ms: 2 }
    );
    assert!(range.tags.is_empty());
}

#[test]
fn tags_of_an_invalid_form_are_not_parsed_at_all() {
    // В libass такие вложенные теги пропускаются целиком: они не считаются ни неизвестными,
    // ни тратящими бюджет.
    let (items, stats) = parse_with_budget(r"{\t(1,2,3,4,\zzz1\fs50)}x", 2);
    assert_eq!(stats.unknown_total(), 0);
    assert_eq!(stats.over_limit(), 0);
    let Some(AssTextItem::Overrides(tags)) = items.first() else {
        panic!("expected an override block, got {items:?}");
    };
    assert_eq!(tags.len(), 1);
}

#[test]
fn text_after_the_first_closing_paren_continues_at_the_outer_level() {
    // VSFilter: `\clip(1,2,3,4)` внутри `\t` кончается на первой `)`; остальное (`\fs20)` и
    // `\bord3`) читается как обычные теги блока.
    let (overrides, stats) = parse(r"\t(0,100,\clip(1,2,3,4)\fs20)\bord3");
    assert_eq!(stats.unknown_total(), 0);
    assert_eq!(overrides.len(), 3, "{overrides:?}");
    assert_eq!(
        overrides[0],
        AssOverride::Transform(AssTransform {
            timing: AssTransformTiming::Range {
                t1_ms: 0,
                t2_ms: 100
            },
            tags: vec![AssOverride::Clip(AssClip {
                mode: AssClipMode::Inside,
                shape: AssClipShape::Rect {
                    x0: 1,
                    y0: 2,
                    x1: 3,
                    y1: 4
                },
            })],
        })
    );
    // `\fs20)`: хвост имени «20)» читается как число 20 (`argtod` останавливается на `)`).
    assert_eq!(overrides[1], font_size(20.0));
    assert_eq!(
        overrides[2],
        AssOverride::Border {
            axes: AssAxes::Both,
            value: Some(3.0)
        }
    );
}

#[test]
fn unterminated_transform_takes_the_rest_of_the_block() {
    let transform = single_transform(r"\t(0,100,\fs50\bord2");
    assert_eq!(transform.tags.len(), 2);
}

#[test]
fn tags_after_a_closed_transform_stay_outside() {
    let (overrides, _) = parse(r"\fs10\t(\fs50)\fs60");
    assert_eq!(overrides.len(), 3);
    assert_eq!(overrides[0], font_size(10.0));
    assert!(matches!(&overrides[1], AssOverride::Transform(t) if t.tags == vec![font_size(50.0)]));
    assert_eq!(overrides[2], font_size(60.0));
}

#[test]
fn name_is_matched_by_prefix_and_case_sensitively() {
    // Как у libass: `t` — префикс, поэтому `\tfoo` тоже `\t`; заглавная `\T` — неизвестный тег.
    let prefix = single_transform(r"\tfoo");
    assert_eq!(prefix.timing, AssTransformTiming::Default);
    assert!(prefix.tags.is_empty());
    let (overrides, stats) = parse(r"\T(\fs50)");
    assert!(overrides.is_empty());
    assert_eq!(stats.unknown_total(), 1);
}

#[test]
fn nested_tags_spend_the_event_budget() {
    // Бюджет 3: сам `\t`, `\fs1`, `\fs2`; `\fs3` внутри и `\fs4` снаружи отбрасываются.
    let (items, stats) = parse_with_budget(r"{\t(0,100,\fs1\fs2\fs3)\fs4}x", 3);
    assert_eq!(stats.over_limit(), 2);
    let Some(AssTextItem::Overrides(tags)) = items.first() else {
        panic!("expected an override block, got {items:?}");
    };
    assert_eq!(tags.len(), 1);
    assert!(
        matches!(&tags[0], AssOverride::Transform(t) if t.tags == vec![font_size(1.0), font_size(2.0)]),
        "{tags:?}"
    );
}

#[test]
fn budget_is_shared_between_blocks_and_nested_tags() {
    let (items, stats) = parse_with_budget(r"{\t(\fs1\fs2)}a{\fs3\fs4}b", 4);
    // Бюджет 4: `\t`, `\fs1`, `\fs2`, `\fs3` -> `\fs4` отброшен.
    assert_eq!(stats.over_limit(), 1);
    assert!(
        matches!(items.get(2), Some(AssTextItem::Overrides(tags)) if tags == &vec![font_size(3.0)]),
        "{items:?}"
    );
}

#[test]
fn nesting_depth_is_bounded_without_losing_the_outer_tags() {
    let levels = MAX_TRANSFORM_NESTING + 4;
    let block = format!("{}\\fs5", "\\t(".repeat(levels));
    let (items, stats) = parse_with_budget(&format!("{{{block}}}x"), GENEROUS_BUDGET);
    assert_eq!(stats.over_limit(), 1);
    assert_eq!(stats.unknown_total(), 0);
    let Some(AssTextItem::Overrides(tags)) = items.first() else {
        panic!("expected an override block, got {items:?}");
    };
    // Считаем цепочку вложенных `\t`: глубины 0..=MAX_TRANSFORM_NESTING, самый глубокий пуст.
    let mut depth = 0;
    let mut current = tags.as_slice();
    while let Some(AssOverride::Transform(transform)) = current.first() {
        depth += 1;
        current = transform.tags.as_slice();
    }
    assert_eq!(depth, MAX_TRANSFORM_NESTING + 1);
    assert!(current.is_empty());
}

#[test]
fn realistic_depth_never_hits_the_bound() {
    // Реальные файлы встречают один-два уровня; граница не должна срабатывать.
    let (_, stats) = parse(r"\t(0,100,\fs50\t(50,80,\bord4\t(\blur2");
    assert_eq!(stats.over_limit(), 0);
    assert_eq!(stats.unknown_total(), 0);
}
