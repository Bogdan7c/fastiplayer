//! Тесты таймлайна: границы интервалов, порядок, дубликаты и сверка с перебором.

use super::shorten::MAX_REMEMBERED_SHORTENINGS;
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
                other => panic!("unexpected content in timeline test: {other:?}"),
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

#[test]
fn events_from_index_returns_only_later_insertions_in_id_order() {
    let mut timeline = SubtitleTimeline::new();
    timeline.extend([
        event(500, 600, "late start"),
        event(100, 200, "early start"),
    ]);
    timeline.extend([event(300, 400, "third")]);

    let tail: Vec<(u32, SubtitleTime)> = timeline
        .events_from_index(1)
        .map(|(id, event)| (id.get(), event.start()))
        .collect();

    assert_eq!(tail, vec![(1, t(100)), (2, t(300))]);
    assert_eq!(timeline.events_from_index(3).count(), 0);
}

#[test]
fn latest_change_within_reports_the_last_start_or_end_in_the_window() {
    let (timeline, _) = SubtitleTimeline::from_events([event(100, 300, "a"), event(200, 250, "b")]);
    // Окно (after, up_to]: нижняя граница не включена, верхняя включена.
    assert_eq!(timeline.latest_change_within(t(0), t(99)), None);
    assert_eq!(timeline.latest_change_within(t(0), t(100)), Some(t(100)));
    assert_eq!(timeline.latest_change_within(t(100), t(150)), None);
    assert_eq!(timeline.latest_change_within(t(0), t(220)), Some(t(200)));
    // Конец «b» (250) позже начала «b» (200).
    assert_eq!(timeline.latest_change_within(t(0), t(260)), Some(t(250)));
    assert_eq!(timeline.latest_change_within(t(250), t(299)), None);
    assert_eq!(timeline.latest_change_within(t(250), t(300)), Some(t(300)));
    assert_eq!(timeline.latest_change_within(t(0), t(1_000)), Some(t(300)));
}

#[test]
fn latest_change_within_matches_brute_force_over_all_boundaries() {
    let events = [
        event(0, 40, "a"),
        event(10, 500, "b"),
        event(30, 60, "c"),
        event(60, 90, "d"),
        event(200, 210, "e"),
    ];
    let (timeline, _) = SubtitleTimeline::from_events(events);
    for after in (-10..520).step_by(7) {
        for up_to in (after..530).step_by(11) {
            let brute_force = timeline
                .events
                .iter()
                .flat_map(|e| [e.start(), e.end()])
                .filter(|&boundary| boundary > t(after) && boundary <= t(up_to))
                .max();
            assert_eq!(
                timeline.latest_change_within(t(after), t(up_to)),
                brute_force,
                "window ({after}, {up_to}]"
            );
        }
    }
}

#[test]
fn shortening_moves_the_end_and_keeps_ids() {
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(0, 1_000, "open"));
    timeline.insert(event(100, 200, "other"));
    let revision_before = timeline.revision();

    let outcome = timeline.shorten_event_end(SubtitleEventId(0), t(300));

    assert_eq!(
        outcome,
        Ok(ShortenOutcome::Shortened {
            previous_end: t(1_000),
            new_end: t(300)
        })
    );
    assert_eq!(timeline.revision(), revision_before + 1);
    assert_eq!(
        timeline.event(SubtitleEventId(0)).map(|e| e.end()),
        Some(t(300))
    );
    assert_eq!(active_texts(&timeline, 299), ["open"]);
    assert_eq!(active_texts(&timeline, 300), Vec::<String>::new());
    assert_eq!(timeline.next_change_after(t(200)), Some(t(300)));
    assert_eq!(timeline.latest_change_within(t(0), t(1_000)), Some(t(300)));
}

#[test]
fn shortening_to_the_same_end_is_a_noop_without_revision_change() {
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(0, 50, "a"));
    let revision_before = timeline.revision();
    assert_eq!(
        timeline.shorten_event_end(SubtitleEventId(0), t(50)),
        Ok(ShortenOutcome::AlreadyEndsThere)
    );
    assert_eq!(timeline.revision(), revision_before);
}

#[test]
fn shortening_errors_leave_the_timeline_untouched() {
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(100, 200, "a"));
    let revision_before = timeline.revision();
    assert_eq!(
        timeline.shorten_event_end(SubtitleEventId(7), t(150)),
        Err(ShortenError::UnknownEvent(SubtitleEventId(7)))
    );
    assert_eq!(
        timeline.shorten_event_end(SubtitleEventId(0), t(201)),
        Err(ShortenError::WouldExtend {
            current_end: t(200),
            new_end: t(201)
        })
    );
    assert_eq!(
        timeline.shorten_event_end(SubtitleEventId(0), t(100)),
        Err(ShortenError::NotAfterStart {
            start: t(100),
            new_end: t(100)
        })
    );
    assert_eq!(timeline.revision(), revision_before);
    assert_eq!(active_texts(&timeline, 199), ["a"]);
}

#[test]
fn revision_grows_only_when_events_are_really_added() {
    let mut timeline = SubtitleTimeline::new();
    assert_eq!(timeline.revision(), 0);
    timeline.insert(event(0, 10, "a"));
    assert_eq!(timeline.revision(), 1);
    // Дубликат ревизию не двигает.
    timeline.insert(event(0, 10, "a"));
    assert_eq!(timeline.revision(), 1);
    timeline.extend([event(0, 10, "a")]);
    assert_eq!(timeline.revision(), 1);
    timeline.extend([event(5, 15, "b"), event(6, 16, "c")]);
    assert_eq!(timeline.revision(), 2);
}

#[test]
fn reinserting_the_original_long_event_after_shortening_is_not_a_duplicate() {
    // Задокументированное взаимодействие с дедупликацией: сравнение идёт по полному событию.
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(0, 1_000, "open"));
    timeline
        .shorten_event_end(SubtitleEventId(0), t(100))
        .expect("shorten");
    let again = timeline.insert(event(0, 1_000, "open"));
    assert_eq!(again, TimelineInsertOutcome::Inserted(SubtitleEventId(1)));
}

#[test]
fn shortening_matches_brute_force_on_pseudo_random_data() {
    let mut rng = Lcg(0xc0ffee);
    let mut events = Vec::new();
    for index in 0..200 {
        let start = i64::try_from(rng.next_below(50)).expect("fits") * 10;
        let length = i64::try_from(rng.next_below(500)).expect("fits") + 2;
        let layer = i32::try_from(rng.next_below(3)).expect("fits");
        events.push(event_with_layer(
            start,
            start + length,
            layer,
            &format!("e{index}"),
        ));
    }
    let (mut timeline, _) = SubtitleTimeline::from_events(events);
    // Случайные укорочения вперемешку с заведомо недопустимыми (удлинение/до начала).
    for _ in 0..150 {
        let id = SubtitleEventId(u32::try_from(rng.next_below(200)).expect("fits"));
        let Some(current) = timeline.event(id) else {
            continue;
        };
        let (start, end) = (current.start().as_micros(), current.end().as_micros());
        let candidate = start + i64::try_from(rng.next_below(600)).expect("fits") - 20;
        let result = timeline.shorten_event_end(id, t(candidate));
        match candidate {
            c if c == end => assert_eq!(result, Ok(ShortenOutcome::AlreadyEndsThere)),
            c if c > end => assert!(matches!(result, Err(ShortenError::WouldExtend { .. }))),
            c if c <= start => assert!(matches!(result, Err(ShortenError::NotAfterStart { .. }))),
            _ => assert!(matches!(result, Ok(ShortenOutcome::Shortened { .. }))),
        }
    }
    let mut ids = Vec::new();
    for time in -5..600 {
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
        let brute_force_change = timeline
            .events
            .iter()
            .flat_map(|e| [e.start(), e.end()])
            .filter(|&boundary| boundary > t(time))
            .min();
        assert_eq!(timeline.next_change_after(t(time)), brute_force_change);
        let brute_latest = timeline
            .events
            .iter()
            .flat_map(|e| [e.start(), e.end()])
            .filter(|&b| b > t(time - 3) && b <= t(time + 40))
            .max();
        assert_eq!(
            timeline.latest_change_within(t(time - 3), t(time + 40)),
            brute_latest
        );
    }
}

#[test]
fn shortenings_since_returns_only_changes_after_the_given_revision() {
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(0, 1_000, "a"));
    timeline.insert(event(10, 1_000, "b"));
    let after_inserts = timeline.revision();
    timeline
        .shorten_event_end(SubtitleEventId(0), t(500))
        .expect("shorten a");
    let after_first = timeline.revision();
    timeline
        .shorten_event_end(SubtitleEventId(1), t(600))
        .expect("shorten b");

    let since_inserts: Vec<_> = timeline
        .shortenings_since(after_inserts)
        .expect("history is complete")
        .collect();
    assert_eq!(
        since_inserts,
        vec![
            EndShortening {
                id: SubtitleEventId(0),
                previous_end: t(1_000),
                new_end: t(500)
            },
            EndShortening {
                id: SubtitleEventId(1),
                previous_end: t(1_000),
                new_end: t(600)
            },
        ]
    );
    let since_first: Vec<_> = timeline
        .shortenings_since(after_first)
        .expect("history is complete")
        .map(|shortening| shortening.id)
        .collect();
    assert_eq!(since_first, vec![SubtitleEventId(1)]);
    assert_eq!(
        timeline
            .shortenings_since(timeline.revision())
            .expect("history is complete")
            .count(),
        0
    );
}

#[test]
fn shortening_history_is_bounded_and_reports_when_it_is_lost() {
    let mut timeline = SubtitleTimeline::new();
    timeline.insert(event(0, 1_000_000, "long"));
    let start_revision = timeline.revision();
    // Больше, чем помнит журнал: каждое укорочение на 1 мкс.
    for step in 1..=(MAX_REMEMBERED_SHORTENINGS as i64 + 5) {
        timeline
            .shorten_event_end(SubtitleEventId(0), t(1_000_000 - step))
            .expect("shorten");
    }
    assert_eq!(
        timeline.shortenings_since(start_revision).err(),
        Some(ShorteningHistoryLost {
            requested_revision: start_revision
        })
    );
    // Недавний потребитель по-прежнему получает свои записи.
    let recent = timeline.revision() - 3;
    assert_eq!(
        timeline.shortenings_since(recent).expect("recent").count(),
        3
    );
}

/// Событие `[start, end)` из слов, каждое со своим моментом появления (`None` — виден сразу).
fn event_with_reveals(start: i64, end: i64, words: &[(&str, Option<i64>)]) -> SubtitleEvent {
    let spans = words
        .iter()
        .map(|(word, reveal_at)| {
            let mut span = TextSpan::text(*word, SpanStyle::default());
            span.reveal_at = reveal_at.map(t);
            span
        })
        .collect();
    SubtitleEvent::new(
        t(start),
        t(end),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(SubtitleTextCue {
            text: StyledText::new(spans),
            placement: CuePlacement::default(),
        }),
    )
    .expect("valid interval")
}

#[test]
fn next_change_includes_word_reveal_of_active_event() {
    let (timeline, _) = SubtitleTimeline::from_events([event_with_reveals(
        100,
        1000,
        &[("a", None), (" b", Some(400)), (" c", Some(700))],
    )]);
    assert_eq!(timeline.next_change_after(t(100)), Some(t(400)));
    assert_eq!(timeline.next_change_after(t(400)), Some(t(700)));
    assert_eq!(
        timeline.next_change_after(t(700)),
        Some(t(1000)),
        "after the last reveal only the end remains"
    );
}

#[test]
fn reveals_outside_the_event_interval_are_not_changes() {
    // Слово «до начала» видно сразу, слово «после конца» не появится никогда.
    let (timeline, _) = SubtitleTimeline::from_events([event_with_reveals(
        100,
        1000,
        &[("a", Some(50)), (" b", Some(1000)), (" c", Some(5000))],
    )]);
    assert_eq!(timeline.next_change_after(t(100)), Some(t(1000)));
}

#[test]
fn reveals_of_not_yet_started_event_are_not_reported_before_its_start() {
    let (timeline, _) = SubtitleTimeline::from_events([event_with_reveals(
        500,
        1000,
        &[("a", None), (" b", Some(800))],
    )]);
    assert_eq!(
        timeline.next_change_after(t(0)),
        Some(t(500)),
        "the start comes first; the reveal at 800 is reported once the event is active"
    );
}

#[test]
fn latest_change_within_sees_word_reveal() {
    let (timeline, _) = SubtitleTimeline::from_events([event_with_reveals(
        100,
        1000,
        &[("a", None), (" b", Some(400))],
    )]);
    assert_eq!(timeline.latest_change_within(t(100), t(600)), Some(t(400)));
    assert_eq!(timeline.latest_change_within(t(400), t(600)), None);
}
