//! Окно загрузки VOD: порядок от позиции вперёд, отсутствие повторов, seek, закрытие.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use subtitle_core::SubtitleContent;

use super::fake::{FakeTransport, WAIT, config, event_segments, index, ready_segment, start};
use crate::stream_subtitles::{StreamIndexRefresh, StreamSubtitleMessage};

fn vod_transport() -> Arc<FakeTransport> {
    FakeTransport::new(
        |_| Ok(index(0, 6, StreamIndexRefresh::Never)),
        |key| Ok(ready_segment(key.value(), None)),
    )
}

#[test]
fn vod_window_is_fetched_forward_from_position_and_not_repeated() {
    let transport = vod_transport();
    // Позиция 25 с, ahead 20 / behind 10: окно [15, 45] -> сегменты 2, 3, 4, затем 1.
    let harness = start(Arc::clone(&transport), config(), 25);

    let messages = harness.collect_until(|messages| event_segments(messages).len() >= 4);
    assert_eq!(event_segments(&messages), vec![2, 3, 4, 1]);
    // Содержимое реплик дошло до приложения, а не только факт вызова.
    let first_events = messages
        .iter()
        .find_map(|message| match message {
            StreamSubtitleMessage::Events(events) => Some(events),
            _ => None,
        })
        .expect("events message");
    assert_eq!(first_events.events.len(), 1);
    let SubtitleContent::Text(cue) = first_events.events[0].content() else {
        panic!("text cue expected");
    };
    assert!(format!("{cue:?}").contains("segment 2"));

    // Окно закрыто: больше никаких скачиваний и сообщений, хотя лента продолжает жить.
    let extra = harness.messages_within(Duration::from_millis(600));
    assert!(extra.is_empty(), "unexpected extra messages: {extra:?}");
    assert_eq!(transport.fetched(), vec![2, 3, 4, 1]);
    assert!(
        harness.wakes.load(Ordering::SeqCst) >= 4,
        "wake after each message"
    );
}

#[test]
fn seek_wakes_the_idle_feed_and_fetches_around_the_new_position() {
    let transport = vod_transport();
    let harness = start(Arc::clone(&transport), config(), 25);
    harness.collect_until(|messages| event_segments(messages).len() >= 4);

    // Лента простаивает (окно закрыто); прыжок должен разбудить её раньше обычного
    // ожидания (сегменты по 10 с -> простой около 5 с).
    let jumped_at = Instant::now();
    harness.position.jump_to(Duration::from_secs(55));
    let messages = harness.collect_until(|messages| !event_segments(messages).is_empty());
    assert!(
        jumped_at.elapsed() < Duration::from_secs(2),
        "jump must wake the feed, took {:?}",
        jumped_at.elapsed()
    );
    // Окно [45, 75]: из необработанных только сегмент 5 (сегмент 4 уже загружен).
    assert_eq!(event_segments(&messages), vec![5]);

    // Прыжок назад: окно [-5, 25] -> сегменты 0, 1, 2. Сегменты 1 и 2 при прыжке вперёд вышли
    // из окна ленты и забыты, поэтому подтягиваются снова вместе с новым сегментом 0.
    harness.position.jump_to(Duration::from_secs(5));
    let messages = harness.collect_until(|messages| event_segments(messages).len() >= 3);
    assert_eq!(event_segments(&messages), vec![0, 1, 2]);
    assert_eq!(transport.fetched(), vec![2, 3, 4, 1, 5, 0, 1, 2]);
}

#[test]
fn segment_that_left_the_window_is_fetched_and_emitted_again_when_the_position_returns() {
    // Приложение вытесняет старые реплики по сроку хранения, а лента раньше помнила обработанные
    // сегменты вечно: перемотка назад за срок хранения ничего не показывала.
    let transport = vod_transport();
    let harness = start(Arc::clone(&transport), config(), 25);
    harness.collect_until(|messages| event_segments(messages).len() >= 4);

    harness.position.jump_to(Duration::from_secs(55));
    harness.collect_until(|messages| event_segments(messages).contains(&5));
    transport.clear_fetch_log();

    harness.position.jump_to(Duration::from_secs(25));
    let messages = harness.collect_until(|messages| event_segments(messages).len() >= 3);
    // Сегмент 4 остался в окне [45, 75] и не забыт; 2, 3 и 1 вышли и приходят заново.
    assert_eq!(event_segments(&messages), vec![2, 3, 1]);
    assert_eq!(transport.fetched(), vec![2, 3, 1]);
}

#[test]
fn dropping_the_feed_returns_immediately_while_a_request_hangs() {
    // Сегмент «зависает» до отпускания защёлки: имитирует запрос, который не прервать.
    let latch = Arc::new((Mutex::new(false), Condvar::new()));
    let in_request = Arc::new(AtomicBool::new(false));
    let script_latch = Arc::clone(&latch);
    let script_flag = Arc::clone(&in_request);
    let transport = FakeTransport::new(
        |_| Ok(index(0, 6, StreamIndexRefresh::Never)),
        move |key| {
            script_flag.store(true, Ordering::SeqCst);
            let (lock, condvar) = &*script_latch;
            let mut released = lock.lock().expect("latch");
            while !*released {
                released = condvar.wait(released).expect("latch wait");
            }
            Ok(ready_segment(key.value(), None))
        },
    );
    let harness = start(transport, config(), 0);
    let deadline = Instant::now() + WAIT;
    while !in_request.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "feed never started a request");
        std::thread::sleep(Duration::from_millis(2));
    }

    let started = Instant::now();
    drop(harness);
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "drop blocked for {:?}",
        started.elapsed()
    );

    // Освобождаем зависший запрос, чтобы фоновый поток доработал и вышел.
    let (lock, condvar) = &*latch;
    *lock.lock().expect("latch") = true;
    condvar.notify_all();
}
