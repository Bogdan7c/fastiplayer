//! Сбои: пропуск сегмента, порог недоступности (ровно одно сообщение), сбой индекса,
//! ожидание привязки времени и его границы.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::fake::{
    FakeTransport, GatedPending, config, event_segments, index, ready_segment, start,
};
use crate::stream_subtitles::{
    SegmentDecode, StreamIndexRefresh, StreamSegmentSkipReason, StreamSubtitleError,
    StreamSubtitleFailureKind, StreamSubtitleMessage, StreamTrackUnavailableReason,
};

fn unavailable_count(messages: &[StreamSubtitleMessage]) -> usize {
    messages
        .iter()
        .filter(|message| matches!(message, StreamSubtitleMessage::TrackUnavailable(_)))
        .count()
}

#[test]
fn one_missing_segment_is_skipped_and_the_rest_still_arrive() {
    // Сегмент 3 даёт 404; порог 3, остальные успешны -> серия сбрасывается.
    let transport = FakeTransport::new(
        |_| Ok(index(0, 6, StreamIndexRefresh::Never)),
        |key| {
            if key.value() == 3 {
                Err(StreamSubtitleError::NotFound)
            } else {
                Ok(ready_segment(key.value(), None))
            }
        },
    );
    let harness = start(transport, config(), 25);

    let messages = harness.collect_until(|messages| {
        event_segments(messages).len() >= 3
            && messages
                .iter()
                .any(|m| matches!(m, StreamSubtitleMessage::Recovered))
    });
    let mut delivered = event_segments(&messages);
    delivered.sort_unstable();
    assert_eq!(delivered, vec![1, 2, 4], "segment 3 has no cues, others do");
    let skipped: Vec<_> = messages
        .iter()
        .filter_map(|message| match message {
            StreamSubtitleMessage::SegmentSkipped { segment, reason } => {
                Some((segment.value(), *reason))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        skipped,
        vec![(
            3,
            StreamSegmentSkipReason::Failed(StreamSubtitleFailureKind::NotFound)
        )]
    );
    assert_eq!(unavailable_count(&messages), 0);
    assert_eq!(
        unavailable_count(&harness.messages_within(Duration::from_millis(400))),
        0
    );
}

#[test]
fn consecutive_failures_report_unavailable_exactly_once_and_stop_the_feed() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 6, StreamIndexRefresh::Never)),
        |_| Err(StreamSubtitleError::NotFound),
    );
    let harness = start(Arc::clone(&transport), config(), 25);

    let messages = harness.collect_until(|messages| unavailable_count(messages) >= 1);
    assert_eq!(
        messages.last(),
        Some(&StreamSubtitleMessage::TrackUnavailable(
            StreamTrackUnavailableReason::ConsecutiveSegmentFailures { count: 3 }
        ))
    );
    // После терминального сообщения лента молчит и больше ничего не качает.
    let after = harness.messages_within(Duration::from_millis(600));
    assert!(after.is_empty(), "feed kept talking: {after:?}");
    assert_eq!(transport.fetched().len(), 3, "stopped at the threshold");
}

#[test]
fn failing_first_index_load_makes_the_track_unavailable_once() {
    let transport = FakeTransport::new(
        |_| Err(StreamSubtitleError::Malformed),
        |_| unreachable_segment(),
    );
    let harness = start(Arc::clone(&transport), config(), 0);

    let messages = harness.collect_until(|messages| unavailable_count(messages) >= 1);
    assert_eq!(
        messages,
        vec![StreamSubtitleMessage::TrackUnavailable(
            StreamTrackUnavailableReason::IndexLoadFailed(StreamSubtitleFailureKind::Malformed)
        )]
    );
    assert!(
        harness
            .messages_within(Duration::from_millis(400))
            .is_empty()
    );
    assert!(transport.fetched().is_empty());
}

fn unreachable_segment() -> Result<SegmentDecode, StreamSubtitleError> {
    Err(StreamSubtitleError::Stale)
}

#[test]
fn cancellation_from_the_transport_ends_the_feed_silently() {
    let transport = FakeTransport::new(
        |_| Ok(index(0, 3, StreamIndexRefresh::Never)),
        |_| Err(StreamSubtitleError::Cancelled),
    );
    let harness = start(transport, config(), 0);
    let messages = harness.messages_within(Duration::from_millis(500));
    assert!(
        messages.is_empty(),
        "cancellation is not a failure: {messages:?}"
    );
}

#[test]
fn segment_waits_for_the_timeline_origin_and_is_delivered_once_it_appears() {
    let released = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&released);
    let transport = FakeTransport::new(
        |_| Ok(index(0, 1, StreamIndexRefresh::Never)),
        move |key| {
            Ok(SegmentDecode::Pending(Box::new(GatedPending {
                released: Arc::clone(&gate),
                key: key.value(),
            })))
        },
    );
    let harness = start(transport, config(), 0);

    // Пока точки отсчёта нет, реплик нет, и это не сбой.
    assert!(
        harness
            .messages_within(Duration::from_millis(400))
            .is_empty()
    );
    released.store(true, Ordering::SeqCst);
    let messages = harness.collect_until(|messages| !event_segments(messages).is_empty());
    assert_eq!(event_segments(&messages), vec![0]);
}

#[test]
fn pending_list_is_bounded_and_evicts_the_oldest_segment() {
    let released = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&released);
    let transport = FakeTransport::new(
        |_| Ok(index(0, 3, StreamIndexRefresh::Never)),
        move |key| {
            Ok(SegmentDecode::Pending(Box::new(GatedPending {
                released: Arc::clone(&gate),
                key: key.value(),
            })))
        },
    );
    let mut limited = config();
    limited.max_pending_segments = std::num::NonZeroUsize::new(1).expect("non-zero");
    // Высокий порог: проверяем именно вытеснение, а не недоступность дорожки.
    limited.failure_threshold = std::num::NonZeroU32::new(50).expect("non-zero");
    // Позиция 40 с уже прошла сегменты 0, 1, 2 (до 30 с): ждать привязку им незачем.
    limited.behind = Duration::from_secs(100);
    let harness = start(transport, limited, 40);

    // Три ожидающих сегмента в списке на один: сначала скачан ближайший к позиции (2), затем
    // следующий вытесняет самый старый из ожидающих - порядок вытеснения 2, 1.
    let messages = harness.collect_until(|messages| {
        messages
            .iter()
            .filter(|m| matches!(m, StreamSubtitleMessage::SegmentSkipped { .. }))
            .count()
            >= 2
    });
    let evicted: Vec<u64> = messages
        .iter()
        .filter_map(|message| match message {
            StreamSubtitleMessage::SegmentSkipped { segment, reason } => {
                assert_eq!(*reason, StreamSegmentSkipReason::PendingOverflow);
                Some(segment.value())
            }
            _ => None,
        })
        .collect();
    assert_eq!(evicted[..2], [2, 1]);
}

#[test]
fn segments_that_never_get_their_timeline_origin_end_in_exactly_one_unavailable() {
    // Эпоха разрыва субтитров так и не опубликована видео (расхождение плейлистов): каждый
    // сегмент остаётся ожидающим. Раньше вытесненные считались «обработанными» навсегда -
    // тихо и без итога. Теперь вытеснение - сбой, и серия кончается одним сообщением.
    let released = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&released);
    let transport = FakeTransport::new(
        |_| Ok(index(0, 3, StreamIndexRefresh::Never)),
        move |key| {
            Ok(SegmentDecode::Pending(Box::new(GatedPending {
                released: Arc::clone(&gate),
                key: key.value(),
            })))
        },
    );
    let mut limited = config();
    limited.max_pending_segments = std::num::NonZeroUsize::new(1).expect("non-zero");
    // Воспроизведение (40 с) уже прошло все сегменты (до 30 с), а точки отсчёта всё нет.
    limited.behind = Duration::from_secs(100);
    let harness = start(Arc::clone(&transport), limited, 40);

    let messages = harness.collect_until(|messages| unavailable_count(messages) == 1);
    let reason = messages
        .iter()
        .find_map(|message| match message {
            StreamSubtitleMessage::TrackUnavailable(reason) => Some(*reason),
            _ => None,
        })
        .expect("unavailable reason");
    assert!(matches!(
        reason,
        StreamTrackUnavailableReason::ConsecutiveSegmentFailures { count: 3 }
    ));
    // Лента остановилась: больше ни сообщений, ни скачиваний.
    let fetched_before = transport.fetched().len();
    let extra = harness.messages_within(Duration::from_millis(500));
    assert_eq!(unavailable_count(&extra), 0, "no second notification");
    assert_eq!(transport.fetched().len(), fetched_before);
    // Вытесненный сегмент снова доступен для скачивания: на него потрачено больше одной попытки.
    let fetched = transport.fetched();
    assert!(
        fetched.iter().filter(|key| **key == 0).count() >= 2 || fetched.len() > 3,
        "evicted segments are retried, got {fetched:?}"
    );
}

#[test]
fn window_reaching_an_epoch_the_video_has_not_entered_yet_is_not_a_failure() {
    // VOD с разрывом (реклама): сегменты 0..2 - эпоха 1, 3.. - эпоха 2, до которой видео ещё не
    // дошло (точка отсчёта не опубликована). Окно вперёд охватывает много сегментов эпохи 2.
    let released = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&released);
    let transport = FakeTransport::new(
        |_| Ok(index(0, 10, StreamIndexRefresh::Never)),
        move |key| {
            if key.value() < 3 {
                Ok(ready_segment(key.value(), None))
            } else {
                Ok(SegmentDecode::Pending(Box::new(GatedPending {
                    released: Arc::clone(&gate),
                    key: key.value(),
                })))
            }
        },
    );
    let mut wide = config();
    wide.ahead = Duration::from_secs(60);
    let harness = start(Arc::clone(&transport), wide, 0);

    let messages = harness.collect_until(|messages| event_segments(messages).len() >= 3);
    let extra = harness.messages_within(Duration::from_millis(600));
    assert_eq!(unavailable_count(&messages), 0);
    assert_eq!(
        unavailable_count(&extra),
        0,
        "healthy stream stays available"
    );
    assert!(
        !extra
            .iter()
            .any(|m| matches!(m, StreamSubtitleMessage::SegmentSkipped { .. })),
        "no overflow churn: {extra:?}"
    );
    // Скачан лишь один «зонд» эпохи 2, остальные ждут без запросов.
    assert_eq!(transport.fetched(), vec![0, 1, 2, 3]);

    // Видео вошло в эпоху 2: реплики приходят, остальные сегменты окна докачиваются.
    released.store(true, Ordering::SeqCst);
    let messages = harness.collect_until(|messages| {
        let segments = event_segments(messages);
        [3, 4, 5].iter().all(|key| segments.contains(key))
    });
    assert_eq!(unavailable_count(&messages), 0);
    assert_eq!(transport.fetched(), vec![0, 1, 2, 3, 4, 5]);
}
