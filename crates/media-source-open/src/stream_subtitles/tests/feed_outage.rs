//! Сетевой перебой и гибель потока: временные сбои повторяются и не убивают дорожку,
//! затяжной перебой даёт ровно одно `TrackUnavailable`, паника потока не остаётся тихой.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::fake::{
    FakeTransport, WAIT, config, event_segments, index, quick_outage_policy, ready_segment,
    start_with_outage_policy,
};
use crate::stream_subtitles::feed::OutagePolicy;
use crate::stream_subtitles::{
    StreamIndexRefresh, StreamSubtitleError, StreamSubtitleFailureKind, StreamSubtitleMessage,
    StreamTrackUnavailableReason,
};

fn unavailable_reasons(messages: &[StreamSubtitleMessage]) -> Vec<StreamTrackUnavailableReason> {
    messages
        .iter()
        .filter_map(|message| match message {
            StreamSubtitleMessage::TrackUnavailable(reason) => Some(*reason),
            _ => None,
        })
        .collect()
}

#[test]
fn short_network_outage_does_not_lose_the_segment() {
    // Первые 5 запросов сегмента 0 падают по сети, потом сеть вернулась.
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let transport = FakeTransport::new(
        |_| Ok(index(0, 1, StreamIndexRefresh::Never)),
        move |key| {
            if counter.fetch_add(1, Ordering::SeqCst) < 5 {
                Err(StreamSubtitleError::Network)
            } else {
                Ok(ready_segment(key.value(), None))
            }
        },
    );
    let harness = start_with_outage_policy(
        Arc::clone(&transport),
        config(),
        0,
        quick_outage_policy(Duration::from_secs(30)),
    );

    let messages = harness.collect_until(|messages| !event_segments(messages).is_empty());
    assert_eq!(
        event_segments(&messages),
        vec![0],
        "the same segment arrives"
    );
    assert!(unavailable_reasons(&messages).is_empty());
    assert!(
        !messages
            .iter()
            .any(|m| matches!(m, StreamSubtitleMessage::SegmentSkipped { .. })),
        "a network error postpones the segment, it does not skip it: {messages:?}"
    );
    assert_eq!(transport.fetched(), vec![0; 6]);
}

#[test]
fn outage_longer_than_the_budget_reports_unavailable_exactly_once() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 3, StreamIndexRefresh::Never)),
        |_| Err(StreamSubtitleError::Network),
    );
    let started = Instant::now();
    let harness = start_with_outage_policy(
        Arc::clone(&transport),
        config(),
        0,
        quick_outage_policy(Duration::from_millis(400)),
    );

    let messages = harness.collect_until(|messages| !unavailable_reasons(messages).is_empty());
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "gave up before the outage budget ran out"
    );
    let reasons = unavailable_reasons(&messages);
    assert_eq!(reasons.len(), 1);
    // Сдались не на пороге в 3 сегмента: за бюджет сделано заметно больше попыток.
    assert!(matches!(
        reasons[0],
        StreamTrackUnavailableReason::ConsecutiveSegmentFailures { count } if count > 3
    ));
    // Лента остановилась: ни сообщений, ни новых запросов.
    let fetched_after_stop = transport.fetched().len();
    let extra = harness.messages_within(Duration::from_millis(400));
    assert!(extra.is_empty(), "feed kept talking: {extra:?}");
    assert_eq!(transport.fetched().len(), fetched_after_stop);
}

#[test]
fn retries_pause_between_attempts_instead_of_busy_looping() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 1, StreamIndexRefresh::Never)),
        |_| Err(StreamSubtitleError::Network),
    );
    let policy = OutagePolicy {
        initial_backoff: Duration::from_millis(100),
        maximum_backoff: Duration::from_millis(100),
        budget: Duration::from_secs(30),
    };
    let harness = start_with_outage_policy(Arc::clone(&transport), config(), 0, policy);
    let _ = harness.messages_within(Duration::from_millis(550));
    // Пауза 100 мс: за ~550 мс не больше ~6-7 попыток (без паузы были бы тысячи).
    let attempts = transport.fetched().len();
    assert!(
        (3..=8).contains(&attempts),
        "expected paced retries, got {attempts}"
    );
}

#[test]
fn permanent_failure_keeps_the_old_skip_and_threshold_behaviour() {
    // 404 - не временный сбой: сегмент пропускается, три подряд - недоступность без пауз.
    let transport = FakeTransport::new(
        |_| Ok(index(0, 6, StreamIndexRefresh::Never)),
        |_| Err(StreamSubtitleError::NotFound),
    );
    let harness = start_with_outage_policy(
        Arc::clone(&transport),
        config(),
        25,
        quick_outage_policy(Duration::from_secs(30)),
    );
    let messages = harness.collect_until(|messages| !unavailable_reasons(messages).is_empty());
    assert_eq!(
        unavailable_reasons(&messages),
        vec![StreamTrackUnavailableReason::ConsecutiveSegmentFailures { count: 3 }]
    );
    assert_eq!(transport.fetched().len(), 3);
}

#[test]
fn initial_index_network_failure_is_retried_and_the_track_works() {
    let transport = FakeTransport::new(
        |call| {
            if call < 3 {
                Err(StreamSubtitleError::Network)
            } else {
                Ok(index(0, 1, StreamIndexRefresh::Never))
            }
        },
        |key| Ok(ready_segment(key.value(), None)),
    );
    let harness = start_with_outage_policy(
        Arc::clone(&transport),
        config(),
        0,
        quick_outage_policy(Duration::from_secs(30)),
    );
    let messages = harness.collect_until(|messages| !event_segments(messages).is_empty());
    assert_eq!(event_segments(&messages), vec![0]);
    assert!(unavailable_reasons(&messages).is_empty());
    assert_eq!(transport.index_loads(), 4);
}

#[test]
fn initial_index_outage_past_the_budget_reports_unavailable_once() {
    let transport = FakeTransport::new(
        |_| Err(StreamSubtitleError::Network),
        |key| Ok(ready_segment(key.value(), None)),
    );
    let harness = start_with_outage_policy(
        Arc::clone(&transport),
        config(),
        0,
        quick_outage_policy(Duration::from_millis(300)),
    );
    let messages = harness.collect_until(|messages| !unavailable_reasons(messages).is_empty());
    assert_eq!(
        messages,
        vec![StreamSubtitleMessage::TrackUnavailable(
            StreamTrackUnavailableReason::IndexLoadFailed(StreamSubtitleFailureKind::Network)
        )]
    );
    assert!(
        transport.index_loads() > 3,
        "kept retrying within the budget"
    );
    assert!(
        harness
            .messages_within(Duration::from_millis(300))
            .is_empty()
    );
    assert!(transport.fetched().is_empty());
}

#[test]
fn live_index_reload_survives_a_network_blip_longer_than_the_failure_threshold() {
    // Порог 3, а перечитывание живого индекса падает 5 раз подряд, затем приходит новый сегмент.
    let transport = FakeTransport::new(
        |call| match call {
            0 => Ok(index(
                0,
                1,
                StreamIndexRefresh::After(Duration::from_millis(40)),
            )),
            1..=5 => Err(StreamSubtitleError::Network),
            _ => Ok(index(
                0,
                2,
                StreamIndexRefresh::After(Duration::from_millis(40)),
            )),
        },
        |key| Ok(ready_segment(key.value(), None)),
    );
    let harness = start_with_outage_policy(
        Arc::clone(&transport),
        config(),
        0,
        quick_outage_policy(Duration::from_secs(30)),
    );
    let messages = harness.collect_until(|messages| event_segments(messages).contains(&1));
    assert!(unavailable_reasons(&messages).is_empty());
}

#[test]
fn cancel_interrupts_a_long_retry_pause_promptly() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 1, StreamIndexRefresh::Never)),
        |_| Err(StreamSubtitleError::Network),
    );
    let policy = OutagePolicy {
        initial_backoff: Duration::from_secs(60),
        maximum_backoff: Duration::from_secs(60),
        budget: Duration::from_secs(600),
    };
    let harness = start_with_outage_policy(Arc::clone(&transport), config(), 0, policy);
    let deadline = Instant::now() + WAIT;
    while transport.fetched().is_empty() {
        assert!(Instant::now() < deadline, "first attempt never happened");
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.feed.cancel();
    while !harness.feed.worker_finished_cleanly() {
        assert!(
            Instant::now() < deadline,
            "feed thread is still sleeping through its retry pause"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(transport.fetched().len(), 1, "no retry after cancellation");
}

#[test]
fn panicked_worker_is_reported_as_lost_exactly_once() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 1, StreamIndexRefresh::Never)),
        |_| panic!("simulated feed thread crash"),
    );
    let harness = start_with_outage_policy(
        transport,
        config(),
        0,
        quick_outage_policy(Duration::from_secs(30)),
    );
    let messages = harness.collect_until(|messages| !unavailable_reasons(messages).is_empty());
    assert_eq!(
        messages,
        vec![StreamSubtitleMessage::TrackUnavailable(
            StreamTrackUnavailableReason::WorkerLost
        )]
    );
    assert!(
        harness
            .messages_within(Duration::from_millis(300))
            .is_empty(),
        "the lost-worker signal must not repeat"
    );
}

#[test]
fn cancelled_feed_does_not_report_a_lost_worker() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 1, StreamIndexRefresh::Never)),
        |key| Ok(ready_segment(key.value(), None)),
    );
    let harness = start_with_outage_policy(
        transport,
        config(),
        0,
        quick_outage_policy(Duration::from_secs(30)),
    );
    harness.feed.cancel();
    let deadline = Instant::now() + WAIT;
    while !harness.feed.worker_finished_cleanly() {
        assert!(Instant::now() < deadline, "worker did not stop");
        std::thread::sleep(Duration::from_millis(5));
    }
    let messages = harness.messages_within(Duration::from_millis(200));
    assert!(unavailable_reasons(&messages).is_empty(), "{messages:?}");
}
