//! Lifecycle-события не теряются, пока UI не забирает tick-телеметрию.
//!
//! Дефект: tick-телеметрия и одноразовые события (`AudioPlaybackResumed`, ...)
//! делили один bounded канал на 512; при вращающемся decoder-е (tick раз в ~2 мс)
//! канал забивался tick-ами и lifecycle-события молча выбрасывались.

use super::*;
use event_stream::{WorkerEventConsumer, worker_event_stream};

/// Больше ёмкости lifecycle-очереди (512): на старом общем канале это гарантированное переполнение.
const FLOOD_TICKS: u64 = 2_000;

fn presented_tick() -> PlayerTickResult {
    PlayerTickResult {
        video_frames_presented: 1,
        decoded_video_frames: 2,
        ..PlayerTickResult::default()
    }
}

fn audio_resumed_event() -> PlayerWorkerEvent {
    PlayerWorkerEvent::Player(CorrelatedPlayerEvent::new(
        None,
        PlayerEvent::AudioPlaybackResumed,
    ))
}

/// Runtime с подключённым к тесту consumer-ом (fixture по умолчанию свой consumer выбрасывает).
fn runtime_with_event_consumer() -> (PlayerWorkerRuntime, WorkerEventConsumer) {
    let mut runtime = runtime_for_tests(Instant::now());
    let (publisher, consumer) = worker_event_stream();
    runtime.event_tx = publisher;
    (runtime, consumer)
}

#[test]
fn lifecycle_event_after_tick_flood_is_delivered_and_ticks_are_aggregated() {
    let (mut runtime, consumer) = runtime_with_event_consumer();

    // UI «залип» и ничего не забирает, а worker публикует tick за tick-ом.
    for _ in 0..FLOOD_TICKS {
        runtime.publish_tick_result(presented_tick());
    }
    runtime.publish_worker_event(audio_resumed_event());

    assert_eq!(
        consumer.drain_lifecycle_events(),
        vec![audio_resumed_event()],
        "lifecycle-событие обязано дойти независимо от потока tick-ов"
    );
    let aggregate = consumer
        .take_tick_telemetry()
        .expect("tick-и опубликованы, агрегат должен быть");
    assert_eq!(aggregate.video_frames_presented, FLOOD_TICKS);
    assert_eq!(aggregate.decoded_video_frames, FLOOD_TICKS * 2);
    assert_eq!(
        consumer.take_tick_telemetry(),
        None,
        "агрегат забирается один раз"
    );
}

#[test]
fn lifecycle_events_keep_publication_order() {
    let (runtime, consumer) = runtime_with_event_consumer();
    let order = [
        PlayerEvent::AudioPlaybackResumed,
        PlayerEvent::ShutdownRequested,
    ];
    for event in order.clone() {
        runtime.publish_worker_event(PlayerWorkerEvent::Player(CorrelatedPlayerEvent::new(
            None, event,
        )));
    }

    let received: Vec<_> = consumer
        .drain_lifecycle_events()
        .into_iter()
        .filter_map(|event| match event {
            PlayerWorkerEvent::Player(correlated) => Some(correlated.event),
            PlayerWorkerEvent::Scrub(_) | PlayerWorkerEvent::RenderError(_) => None,
        })
        .collect();
    assert_eq!(received, order);
}

#[test]
fn empty_ticks_do_not_create_an_aggregate() {
    let (mut runtime, consumer) = runtime_with_event_consumer();
    for _ in 0..10 {
        runtime.publish_tick_result(PlayerTickResult::default());
    }
    assert_eq!(consumer.take_tick_telemetry(), None);
}

#[test]
fn tick_published_while_ui_holds_slot_is_carried_to_next_publish_without_loss() {
    let (mut publisher, consumer) = worker_event_stream();

    let held_slot = consumer.hold_tick_slot_for_tests();
    publisher.publish_tick(presented_tick()); // не блокируется, уходит в carry
    drop(held_slot);
    assert_eq!(
        consumer.take_tick_telemetry(),
        None,
        "слот был занят, порция в carry"
    );

    publisher.publish_tick(presented_tick());
    let aggregate = consumer
        .take_tick_telemetry()
        .expect("carry слит со следующим tick-ом");
    assert_eq!(aggregate.video_frames_presented, 2);
}

#[test]
fn full_lifecycle_queue_drops_new_event_without_blocking_worker() {
    let (publisher, consumer) = event_stream::worker_event_stream_with_lifecycle_capacity(1);
    publisher.publish_lifecycle(audio_resumed_event());
    publisher.publish_lifecycle(PlayerWorkerEvent::Player(CorrelatedPlayerEvent::new(
        None,
        PlayerEvent::ShutdownRequested,
    )));

    // Второе событие потеряно (и залогировано warn-ом), первое не затронуто.
    assert_eq!(
        consumer.drain_lifecycle_events(),
        vec![audio_resumed_event()]
    );
}
