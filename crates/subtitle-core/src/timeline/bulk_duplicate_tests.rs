//! Пакетная загрузка множества событий с одним началом: время и точность отсева дубликатов.

use std::time::{Duration, Instant};

use super::*;
use crate::event::{SubtitleContent, SubtitleLayer, SubtitleTextCue};
use crate::placement::CuePlacement;
use crate::styled_text::{SpanStyle, StyledText, TextSpan};

/// Щедрая граница: исправный код укладывается в доли секунды, квадратичный (50k^2/2 сравнений
/// событий в debug) — заметно дольше.
const BULK_BUILD_TIME_BOUND: Duration = Duration::from_secs(3);

fn same_start_event(text: &str) -> SubtitleEvent {
    SubtitleEvent::new(
        SubtitleTime::from_micros(1_000),
        SubtitleTime::from_micros(2_000),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(SubtitleTextCue {
            text: StyledText::new(vec![TextSpan::text(text, SpanStyle::default())]),
            placement: CuePlacement::default(),
        }),
    )
    .expect("valid interval")
}

#[test]
fn many_same_start_events_build_in_bounded_time_and_all_are_kept() {
    const EVENT_COUNT: usize = 50_000;
    let events = (0..EVENT_COUNT).map(|index| same_start_event(&format!("line {index}")));

    let started = Instant::now();
    let (timeline, report) = SubtitleTimeline::from_events(events);
    let elapsed = started.elapsed();

    assert_eq!(report.inserted, EVENT_COUNT);
    assert_eq!(report.duplicates_ignored, 0);
    assert_eq!(timeline.len(), EVENT_COUNT);
    assert!(
        elapsed < BULK_BUILD_TIME_BOUND,
        "bulk build took {elapsed:?}"
    );
}

#[test]
fn duplicates_among_many_same_start_events_are_dropped_exactly_first_wins() {
    const DISTINCT_COUNT: usize = 20_000;
    // Каждое событие идёт дважды подряд: ожидаем ровно DISTINCT_COUNT принятых.
    let events = (0..DISTINCT_COUNT).flat_map(|index| {
        let text = format!("line {index}");
        [same_start_event(&text), same_start_event(&text)]
    });

    let (timeline, report) = SubtitleTimeline::from_events(events);

    assert_eq!(report.inserted, DISTINCT_COUNT);
    assert_eq!(report.duplicates_ignored, DISTINCT_COUNT);
    // Первое вхождение победило: id приняты подряд в порядке первых вхождений.
    let texts: Vec<String> = timeline
        .events_from_index(0)
        .map(|(_, event)| match event.content() {
            SubtitleContent::Text(cue) => cue.text.plain_text(),
            other => panic!("unexpected content: {other:?}"),
        })
        .collect();
    let expected: Vec<String> = (0..DISTINCT_COUNT).map(|i| format!("line {i}")).collect();
    assert_eq!(texts, expected);
}

#[test]
fn extend_drops_candidates_equal_to_existing_same_start_events() {
    let (mut timeline, _) =
        SubtitleTimeline::from_events([same_start_event("a"), same_start_event("b")]);

    let report = timeline.extend([
        same_start_event("b"),
        same_start_event("c"),
        same_start_event("c"),
    ]);

    assert_eq!(report.inserted, 1);
    assert_eq!(report.duplicates_ignored, 2);
    assert_eq!(timeline.len(), 3);
}
