//! Тесты арифметики караоке: накопление времени слов, раскраска и срок годности.
//!
//! Слова строятся по настоящей предварительной раскладке букв (`preliminary_layout`), теги
//! караоке расставляются вручную на символах: так проверяется ровно то, что делает
//! `ass_process_karaoke_effects`, без разбора файла.

use subtitle_core::SubtitleColor;

use super::*;
use crate::clip::ImagePaint;
use crate::glyph_info::TextCluster;
use crate::layout::preliminary_layout;
use crate::test_support::{embedded_fonts, plain_style, plain_text, shaped_clusters};

const RED: SubtitleColor = SubtitleColor::opaque(255, 0, 0);
const BLUE: SubtitleColor = SubtitleColor::opaque(0, 0, 255);

fn syllable(effect: KaraokeEffect, timing_ms: i32, skip_ms: i32) -> KaraokeMark {
    KaraokeMark {
        effect: Some(effect),
        timing_ms,
        skip_ms,
        resets_timeline: false,
    }
}

/// Кластеры текста с предварительными позициями.
fn laid_out(text: &str) -> Vec<TextCluster> {
    let mut fonts = embedded_fonts();
    let mut clusters = shaped_clusters(&mut fonts, &plain_text(text), &plain_style());
    preliminary_layout(&mut clusters);
    clusters
}

/// Ставит метки: `(номер символа, метка)`; символы с меткой начинают слово, если не указано иное.
fn mark(clusters: &mut [TextCluster], index: usize, mark: KaraokeMark, starts_run: bool) {
    clusters[index].karaoke = mark;
    clusters[index].starts_new_run = starts_run;
}

/// Слово, к которому относится символ `index`.
fn word(plan: &KaraokePlan, index: usize) -> KaraokeWord {
    plan.word_at(index).expect("karaoke word").0
}

/// Два слова по два символа: `ab` и `cd`.
fn two_words(first: KaraokeMark, second: KaraokeMark) -> KaraokePlan {
    let mut clusters = laid_out("abcd");
    clusters[0].starts_new_run = true;
    mark(&mut clusters, 0, first, true);
    mark(&mut clusters, 2, second, true);
    KaraokePlan::build(&clusters)
}

#[test]
fn word_times_follow_skip_and_duration_and_chain_into_the_next_word() {
    let plan = two_words(
        syllable(KaraokeEffect::Fill, 500, 200),
        syllable(KaraokeEffect::Fill, 300, 0),
    );
    let first = word(&plan, 0);
    let second = word(&plan, 2);
    assert_eq!((first.start_ms, first.end_ms), (200, 700));
    assert_eq!((second.start_ms, second.end_ms), (700, 1000));
    assert_eq!(word(&plan, 1), first, "both letters belong to one word");
}

#[test]
fn plain_and_outline_syllables_switch_instantly_at_their_start() {
    for effect in [KaraokeEffect::Plain, KaraokeEffect::Outline] {
        let plan = two_words(syllable(effect, 500, 100), syllable(effect, 200, 0));
        let first = word(&plan, 0);
        assert_eq!((first.start_ms, first.end_ms), (100, 100), "{effect:?}");
        // Следующее слово всё равно отсчитывается от конца длительности предыдущего.
        let second = word(&plan, 2);
        assert_eq!((second.start_ms, second.end_ms), (600, 600), "{effect:?}");
    }
}

#[test]
fn a_word_without_tags_inherits_the_last_effect_and_starts_where_the_last_ended() {
    let plan = two_words(syllable(KaraokeEffect::Fill, 500, 0), KaraokeMark::NONE);
    let second = word(&plan, 2);
    assert_eq!(second.effect, KaraokeEffect::Fill);
    assert_eq!((second.start_ms, second.end_ms), (500, 500));
}

#[test]
fn text_before_the_first_effect_is_not_a_karaoke_word() {
    let mut clusters = laid_out("abcd");
    mark(&mut clusters, 0, KaraokeMark::NONE, true);
    mark(
        &mut clusters,
        2,
        syllable(KaraokeEffect::Plain, 100, 0),
        true,
    );
    let plan = KaraokePlan::build(&clusters);
    assert!(plan.word_at(0).is_none());
    assert!(plan.word_at(1).is_none());
    assert!(plan.word_at(2).is_some());
}

#[test]
fn kt_restarts_the_timeline_with_its_own_skip() {
    let reset = KaraokeMark {
        resets_timeline: true,
        ..syllable(KaraokeEffect::Fill, 0, 100)
    };
    let plan = two_words(syllable(KaraokeEffect::Fill, 500, 0), reset);
    let second = word(&plan, 2);
    assert_eq!(
        second.start_ms, 100,
        "the timeline starts again from the skip"
    );
}

#[test]
fn inner_skip_without_a_run_break_only_delays_the_next_word() {
    // VSFilter: `\k12345\k0` внутри слова. Слово остаётся прежним, а следующее начинается позже.
    let mut clusters = laid_out("abcd");
    mark(
        &mut clusters,
        0,
        syllable(KaraokeEffect::Fill, 500, 0),
        true,
    );
    mark(
        &mut clusters,
        1,
        syllable(KaraokeEffect::Fill, 0, 12345),
        false,
    );
    mark(
        &mut clusters,
        2,
        syllable(KaraokeEffect::Fill, 300, 0),
        true,
    );
    let plan = KaraokePlan::build(&clusters);
    let first = word(&plan, 0);
    assert_eq!(
        (first.start_ms, first.end_ms),
        (0, 500),
        "word is unchanged"
    );
    assert_eq!(word(&plan, 2).start_ms, 500 + 12345);
}

#[test]
fn inner_kt_drops_the_accumulated_end_of_the_word() {
    let mut clusters = laid_out("abcd");
    mark(
        &mut clusters,
        0,
        syllable(KaraokeEffect::Fill, 500, 0),
        true,
    );
    let inner_reset = KaraokeMark {
        resets_timeline: true,
        ..syllable(KaraokeEffect::Fill, 0, 50)
    };
    mark(&mut clusters, 1, inner_reset, false);
    mark(
        &mut clusters,
        2,
        syllable(KaraokeEffect::Fill, 300, 0),
        true,
    );
    let plan = KaraokePlan::build(&clusters);
    // `has_reset`: конец слова (500) не переносится, остаётся только пропуск после `\kt`.
    assert_eq!(word(&plan, 2).start_ms, 50);
}

#[test]
fn fill_progress_moves_from_the_left_edge_to_the_right_edge_of_the_word() {
    let plan = two_words(
        syllable(KaraokeEffect::Fill, 1000, 0),
        syllable(KaraokeEffect::Fill, 1000, 0),
    );
    let first = word(&plan, 0);
    assert_eq!(first.progress_at(-1).x(), -100_000_000);
    let at = |elapsed: i64| first.progress_at(elapsed).x();
    // Граница двигается вправо монотонно и стартует с левого края слова.
    assert!(at(0) < at(250) && at(250) < at(500) && at(500) < at(999));
    assert_eq!(
        at(0),
        plan.word_at(0).map(|(_, origin)| origin).unwrap_or(-1)
    );
    assert!(!first.progress_at(500).colors_swapped());
    assert_eq!(first.progress_at(1000).x(), 100_000_000, "finished word");
    assert_eq!(first.progress_at(-5).x(), -100_000_000, "word not started");
}

#[test]
fn rotation_between_90_and_270_fills_right_to_left_and_swaps_colours() {
    let build = |frz: f64| {
        let mut clusters = laid_out("ab");
        clusters[0].rotation_z_degrees = frz;
        mark(
            &mut clusters,
            0,
            syllable(KaraokeEffect::Fill, 1000, 0),
            true,
        );
        word(&KaraokePlan::build(&clusters), 0)
    };
    for (frz, reversed) in [
        (170.0, true),
        (200.0, true),
        (-150.0, false),
        (-170.0, false),
        (90.0, false),
        (270.0, false),
        (450.0, false),
        (540.0, true),
    ] {
        assert_eq!(
            build(frz).progress_at(500).colors_swapped(),
            reversed,
            "frz {frz}"
        );
    }
    let word = build(180.0);
    assert!(
        word.progress_at(250).x() > word.progress_at(750).x(),
        "reversed fill moves leftwards"
    );
}

fn rule_for(effect: KaraokeEffect, start_skip: i32, timing: i32) -> KaraokeRunRule {
    let mut clusters = laid_out("ab");
    mark(&mut clusters, 0, syllable(effect, timing, start_skip), true);
    let plan = KaraokePlan::build(&clusters);
    let (word, origin) = plan.word_at(0).expect("word");
    KaraokeRunRule::new(RuleInputs {
        word,
        first_origin_x: origin,
        leftmost_x: 640,
        pixel_aspect_x: 1.0,
        primary: RED,
        secondary: BLUE,
    })
}

#[test]
fn fill_rule_splits_left_primary_right_secondary_and_moves_the_split() {
    let rule = rule_for(KaraokeEffect::Fill, 0, 1000);
    let split = |elapsed: i64| match rule.paint_at(Some(elapsed)).glyph {
        ImagePaint::SplitAt { x, left, right } => (x, left, right),
        other => panic!("fill must split, got {other:?}"),
    };
    let (before, left, right) = split(-10);
    assert!(before < -1_000_000, "everything is right of the split");
    assert_eq!((left, right), (RED, BLUE));
    let (middle, _, _) = split(500);
    let (late, _, _) = split(900);
    assert!(
        10 < middle && middle < late,
        "{middle} {late}: leftmost is x = 10 px"
    );
    let (after, _, _) = split(1000);
    assert!(after > 1_000_000, "everything is left of the split");
}

#[test]
fn plain_rule_is_secondary_before_and_primary_from_the_start() {
    let rule = rule_for(KaraokeEffect::Plain, 300, 100);
    let color = |elapsed: Option<i64>| match rule.paint_at(elapsed).glyph {
        ImagePaint::Solid(color) => color,
        other => panic!("plain must be one colour, got {other:?}"),
    };
    assert_eq!(color(Some(299)), BLUE);
    assert_eq!(
        color(Some(300)),
        RED,
        "the start millisecond is already sung"
    );
    assert_eq!(color(None), RED, "no clock: everything is sung");
}

#[test]
fn outline_rule_hides_the_outline_until_the_start() {
    let rule = rule_for(KaraokeEffect::Outline, 300, 100);
    assert!(!rule.paint_at(Some(299)).outline_visible);
    assert!(rule.paint_at(Some(300)).outline_visible);
    assert_eq!(rule.paint_at(Some(299)).glyph, ImagePaint::Solid(BLUE));
    // У остальных видов обводка видна всегда.
    for effect in [KaraokeEffect::Fill, KaraokeEffect::Plain] {
        assert!(rule_for(effect, 300, 100).paint_at(Some(0)).outline_visible);
    }
}

#[test]
fn stage_maps_roles_to_paints_and_reports_the_next_change() {
    let mut builder = KaraokeStageBuilder::default();
    let fill = builder.add_rule(rule_for(KaraokeEffect::Fill, 200, 400));
    let outline = builder.add_rule(rule_for(KaraokeEffect::Outline, 900, 100));
    builder.push_image(ImageRole::Static);
    builder.push_image(ImageRole::Outline { rule: outline });
    builder.push_image(ImageRole::Fill { rule: fill });
    let stage = builder.finish().expect("stage");

    let before = stage.paints_at(Some(0));
    assert_eq!(before[0], ImagePaint::AsStored);
    assert_eq!(before[1], ImagePaint::Hidden, "ko outline before its start");
    assert!(matches!(before[2], ImagePaint::SplitAt { .. }));
    assert_eq!(stage.paints_at(Some(900))[1], ImagePaint::AsStored);

    // Ближайшее изменение: начало первого слова, затем покадровая заливка, затем начало `\ko`.
    assert_eq!(stage.next_change(0), NextChange::AtMs(200));
    assert_eq!(stage.next_change(200), NextChange::EveryFrame);
    assert_eq!(stage.next_change(599), NextChange::EveryFrame);
    assert_eq!(stage.next_change(600), NextChange::AtMs(900));
    assert_eq!(stage.next_change(900), NextChange::Never);

    // Плашка перед текстом сдвигает роли и не получает раскраски.
    let shifted = stage.with_leading_static_images(1).paints_at(Some(0));
    assert_eq!(shifted.len(), 4);
    assert_eq!(shifted[0], ImagePaint::AsStored);
    assert_eq!(shifted[2], ImagePaint::Hidden);
}

#[test]
fn a_stage_without_rules_is_not_built() {
    let mut builder = KaraokeStageBuilder::default();
    builder.push_image(ImageRole::Static);
    assert!(builder.finish().is_none());
}

#[test]
fn syllable_tags_accumulate_like_libass() {
    let mut mark = KaraokeMark::NONE;
    mark.begin_syllable(KaraokeEffect::Plain, 200);
    mark.begin_syllable(KaraokeEffect::Fill, 500);
    assert_eq!(
        (mark.effect, mark.timing_ms, mark.skip_ms),
        (Some(KaraokeEffect::Fill), 500, 200),
        "the first syllable lost its text and became a skip"
    );
    mark.skip_timeline(70);
    assert_eq!(
        (mark.timing_ms, mark.skip_ms, mark.resets_timeline),
        (0, 70, true),
        "kt assigns the skip and zeroes the duration"
    );
    let taken = mark.take();
    assert_eq!(taken.skip_ms, 70);
    assert_eq!(mark, KaraokeMark::NONE, "the next symbol starts clean");
}

#[test]
fn a_symbol_splits_the_run_on_a_duration_or_a_new_effect_type() {
    let plain_fill = syllable(KaraokeEffect::Fill, 0, 0);
    assert!(!plain_fill.splits_run(Some(KaraokeEffect::Fill)));
    assert!(plain_fill.splits_run(Some(KaraokeEffect::Plain)));
    assert!(plain_fill.splits_run(None));
    // Пропуск без длительности участок не рвёт, длительность — рвёт даже при том же виде.
    assert!(!syllable(KaraokeEffect::Fill, 0, 900).splits_run(Some(KaraokeEffect::Fill)));
    assert!(syllable(KaraokeEffect::Fill, 1, 0).splits_run(Some(KaraokeEffect::Fill)));
    assert!(!KaraokeMark::NONE.splits_run(Some(KaraokeEffect::Fill)));
}
