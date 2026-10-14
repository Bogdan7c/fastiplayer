//! Тесты таймлайна: границы интервалов, порядок, дубликаты и сверка с перебором.

use super::*;
use crate::event::{SubtitleContent, SubtitleLayer, SubtitleTextCue};
use crate::placement::CuePlacement;
use crate::styled_text::{SpanStyle, StyledText, TextSpan};

fn t(micros: i64) -> SubtitleTime {
    SubtitleTime::from_micros(micros)
}

fn event_with_layer(start: i64, end: i64, layer: i32, text: &str) -> SubtitleEvent {
    SubtitleEvent::new(
        t(start),
        t(end),
        SubtitleLayer::new(layer),
        SubtitleContent::Text(SubtitleTextCue {
            text: StyledText::new(vec![TextSpan::text(text, SpanStyle::default())]),
            placement: CuePlacement::default(),
        }),
    )
    .expect("valid interval")
}

fn event(start: i64, end: i64, text: &str) -> SubtitleEvent {
    event_with_layer(start, end, 0, text)
}

fn active_texts(timeline: &SubtitleTimeline, time: i64) -> Vec<String> {
    let mut ids = Vec::new();
    timeline.collect_active_at(t(time), &mut ids);
    ids.iter()
        .map(
            |&id| match timeline.event(id).expect("known id").content() {
                SubtitleContent::Text(cue) => cue.text.plain_text(),
            },
        )
        .collect()
}

#[test]
fn interval_is_half_open_at_both_boundaries() {
    let (timeline, _) = SubtitleTimeline::from_events([event(1_000, 2_000, "a")]);
    assert_eq!(active_texts(&timeline, 999), Vec::<String>::new());
    assert_eq!(active_texts(&timeline, 1_000), ["a"]);
    assert_eq!(active_texts(&timeline, 1_999), ["a"]);
    assert_eq!(active_texts(&timeline, 2_000), Vec::<String>::new());
}

#[test]
fn overlapping_events_are_ordered_by_layer_then_appearance() {
    let (timeline, _) = SubtitleTimeline::from_events([
        event_with_layer(0, 100, 1, "top"),
        event_with_layer(20, 100, 0, "late-bottom"),
        event_with_layer(10, 100, 0, "early-bottom"),
    ]);
    assert_eq!(
        active_texts(&timeline, 50),
        ["early-bottom", "late-bottom", "top"]
    );
}

#[test]
fn equal_start_and_layer_fall_back_to_insertion_order() {
    let (timeline, _) =
        SubtitleTimeline::from_events([event(0, 10, "first"), event(0, 20, "second")]);
    assert_eq!(active_texts(&timeline, 5), ["first", "second"]);
    assert_eq!(active_texts(&timeline, 15), ["second"]);
}

#[test]
fn event_appended_after_build_becomes_visible() {
    let (mut timeline, _) = SubtitleTimeline::from_events([event(0, 10, "old")]);
    assert_eq!(active_texts(&timeline, 55), Vec::<String>::new());
    let outcome = timeline.insert(event(50, 60, "new"));
    assert_eq!(outcome, TimelineInsertOutcome::Inserted(SubtitleEventId(1)));
    assert_eq!(active_texts(&timeline, 55), ["new"]);
    assert_eq!(active_texts(&timeline, 5), ["old"]);
}

#[test]
fn insert_before_existing_start_keeps_sorted_order() {
    let (mut timeline, _) = SubtitleTimeline::from_events([event(100, 200, "later")]);
    timeline.insert(event(0, 150, "earlier"));
    let starts: Vec<i64> = timeline
        .iter()
        .map(|(_, e)| e.start().as_micros())
        .collect();
    assert_eq!(starts, [0, 100]);
    assert_eq!(active_texts(&timeline, 120), ["earlier", "later"]);
}

#[test]
fn duplicate_is_ignored_and_points_to_existing_event() {
    let mut timeline = SubtitleTimeline::new();
    let first = timeline.insert(event(0, 10, "x"));
    let again = timeline.insert(event(0, 10, "x"));
    assert_eq!(first, TimelineInsertOutcome::Inserted(SubtitleEventId(0)));
    assert_eq!(
        again,
        TimelineInsertOutcome::DuplicateIgnored(SubtitleEventId(0))
    );
    assert_eq!(timeline.len(), 1);
}

#[test]
fn same_interval_with_different_text_or_layer_is_not_duplicate() {
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(0, 10, "x"));
    let other_text = timeline.insert(event(0, 10, "y"));
    let other_layer = timeline.insert(event_with_layer(0, 10, 3, "x"));
    assert!(matches!(other_text, TimelineInsertOutcome::Inserted(_)));
    assert!(matches!(other_layer, TimelineInsertOutcome::Inserted(_)));
    assert_eq!(timeline.len(), 3);
}

#[test]
fn bulk_load_counts_duplicates_inside_batch_and_against_existing() {
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(0, 10, "existing"));
    let report = timeline.extend([
        event(0, 10, "existing"),
        event(5, 15, "new"),
        event(5, 15, "new"),
        event(5, 15, "other"),
    ]);
    assert_eq!(
        report,
        TimelineBuildReport {
            inserted: 2,
            duplicates_ignored: 2,
            rejected_over_capacity: 0
        }
    );
    assert_eq!(timeline.len(), 3);
    assert_eq!(active_texts(&timeline, 7), ["existing", "new", "other"]);
}

#[test]
fn long_event_at_beginning_does_not_hide_later_short_events() {
    let mut events = vec![event(0, 1_000_000, "long")];
    for index in 0..200 {
        events.push(event(index * 100, index * 100 + 50, "short"));
    }
    let (timeline, _) = SubtitleTimeline::from_events(events);
    assert_eq!(active_texts(&timeline, 10_020), ["long", "short"]);
    assert_eq!(active_texts(&timeline, 10_060), ["long"]);
    assert_eq!(active_texts(&timeline, 1_000_000), Vec::<String>::new());
}

/// Детерминированный линейный конгруэнтный генератор: без внешних зависимостей.
struct Lcg(u64);

impl Lcg {
    fn next_below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

#[test]
fn matches_brute_force_on_pseudo_random_data() {
    let mut rng = Lcg(0x5eed);
    let mut events = Vec::new();
    for index in 0..400 {
        // Малый диапазон начал даёт много равных start; редкие длинные события.
        let start = i64::try_from(rng.next_below(60)).expect("fits") * 10;
        let long = rng.next_below(10) == 0;
        let length = if long {
            i64::try_from(rng.next_below(600)).expect("fits") + 1
        } else {
            i64::try_from(rng.next_below(40)).expect("fits") + 1
        };
        let layer = i32::try_from(rng.next_below(3)).expect("fits");
        events.push(event_with_layer(
            start,
            start + length,
            layer,
            &format!("e{index}"),
        ));
    }
    let (timeline, report) = SubtitleTimeline::from_events(events);
    assert_eq!(report.inserted + report.duplicates_ignored, 400);

    let mut ids = Vec::new();
    for time in -5..700 {
        timeline.collect_active_at(t(time), &mut ids);
        let mut expected: Vec<SubtitleEventId> = timeline
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_active_at(t(time)))
            .map(|(index, _)| SubtitleEventId(u32::try_from(index).expect("fits")))
            .collect();
        expected.sort_by_key(|&id| {
            let e = timeline.event(id).expect("known id");
            (e.layer(), e.start(), id)
        });
        assert_eq!(ids, expected, "active set differs at t={time}");

        // next_change_after сверяется с перебором всех границ.
        let brute_force_change = timeline
            .events
            .iter()
            .flat_map(|e| [e.start(), e.end()])
            .filter(|&boundary| boundary > t(time))
            .min();
        assert_eq!(
            timeline.next_change_after(t(time)),
            brute_force_change,
            "change after t={time}"
        );
    }
}

#[test]
fn reusing_buffer_clears_previous_result_and_does_not_grow() {
    let (timeline, _) =
        SubtitleTimeline::from_events([event(0, 10, "a"), event(0, 10, "b"), event(20, 30, "c")]);
    let mut ids = Vec::new();
    timeline.collect_active_at(t(5), &mut ids);
    assert_eq!(ids.len(), 2);
    let capacity_after_first = ids.capacity();
    timeline.collect_active_at(t(5), &mut ids);
    assert_eq!(ids.len(), 2);
    assert_eq!(ids.capacity(), capacity_after_first);
    // Запрос без результатов обязан очистить буфер от старых id.
    timeline.collect_active_at(t(15), &mut ids);
    assert!(ids.is_empty());
}

#[test]
fn next_change_reports_nearest_start_or_end_strictly_after() {
    let (timeline, _) = SubtitleTimeline::from_events([event(100, 300, "a"), event(200, 250, "b")]);
    assert_eq!(timeline.next_change_after(t(-1)), Some(t(100)));
    assert_eq!(timeline.next_change_after(t(100)), Some(t(200)));
    assert_eq!(timeline.next_change_after(t(200)), Some(t(250)));
    assert_eq!(timeline.next_change_after(t(250)), Some(t(300)));
    assert_eq!(timeline.next_change_after(t(300)), None);
}

#[test]
fn empty_timeline_has_no_active_events_and_no_changes() {
    let timeline = SubtitleTimeline::new();
    assert!(timeline.is_empty());
    assert_eq!(active_texts(&timeline, 0), Vec::<String>::new());
    assert_eq!(timeline.next_change_after(t(0)), None);
}

#[test]
fn unknown_event_id_returns_none() {
    let timeline = SubtitleTimeline::new();
    assert!(timeline.event(SubtitleEventId(0)).is_none());
}
