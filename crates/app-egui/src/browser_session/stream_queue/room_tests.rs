//! Тесты противодавления очереди: «места нет» над отметкой до забора, забор отпускает,
//! удержание без забора дольше предела — переполнение 4003 (решение 98), остановка
//! очереди отпускает поток соединения.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use browser_bridge_protocol::{AppendSequence, SessionCloseReason, SessionKey, SourceBufferIndex};
use browser_bridge_session::{
    ChunkSource, IntakeRoom, SessionByteSink, SessionChunk, SessionInstance, SessionRef,
    SourceBufferChunk,
};

use super::super::{OrderedSessionQueue, QueueLimits, QueueTake, SessionInput};
use crate::browser_session::inbox::{BrowserSessionInbox, BrowserSessionWake, InboxCapacity};
use crate::browser_session::intake_notice::IntakeNotice;

#[derive(Default)]
struct CountingWake(AtomicUsize);

impl BrowserSessionWake for CountingWake {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Очередь с отметкой «придержать» в два элемента и пределом удержания `hold_limit`.
fn queue_holding_at_two(
    hold_limit: Duration,
) -> (Arc<OrderedSessionQueue>, Arc<BrowserSessionInbox>) {
    let inbox = Arc::new(BrowserSessionInbox::new(
        InboxCapacity::default(),
        Arc::new(CountingWake::default()),
    ));
    let limits = QueueLimits {
        hold_above_items: 2,
        hold_without_progress_limit: hold_limit,
        ..QueueLimits::default()
    };
    (
        Arc::new(OrderedSessionQueue::new(limits, Arc::clone(&inbox))),
        inbox,
    )
}

fn session(instance: u64) -> SessionRef {
    SessionRef {
        key: SessionKey::new("tab").expect("valid key"),
        instance: SessionInstance(instance),
    }
}

fn accept_bytes(queue: &OrderedSessionQueue, session: &SessionRef, sequence: u64) {
    queue.accept(SessionChunk {
        session,
        source: ChunkSource::SourceBuffer(SourceBufferChunk {
            generation: browser_bridge_protocol::StreamGeneration::FIRST,
            source_buffer: SourceBufferIndex::new(0).expect("valid index"),
            sequence: AppendSequence(sequence),
            payload: b"frame",
        }),
    });
}

fn close_requests(inbox: &BrowserSessionInbox) -> Vec<(SessionRef, SessionCloseReason)> {
    inbox
        .take_batch()
        .intake_notices
        .into_iter()
        .filter_map(|notice| match notice {
            IntakeNotice::CloseSession { session, reason } => Some((session, reason)),
            _ => None,
        })
        .collect()
}

/// Ждёт места как поток соединения: с терпением `patience`; возвращает ответ и время.
fn ask(
    queue: &OrderedSessionQueue,
    session: &SessionRef,
    patience: Duration,
) -> (IntakeRoom, Duration) {
    let started = Instant::now();
    let room = SessionByteSink::wait_for_room(queue, session, patience);
    (room, started.elapsed())
}

#[test]
fn room_is_available_below_the_hold_mark_without_waiting() {
    let (queue, _inbox) = queue_holding_at_two(Duration::from_secs(10));
    let page = session(1);
    accept_bytes(&queue, &page, 0);
    let (room, waited) = ask(&queue, &page, Duration::from_secs(1));
    assert_eq!(room, IntakeRoom::Available);
    assert!(
        waited < Duration::from_millis(500),
        "no wait below the mark: {waited:?}"
    );
}

#[test]
fn room_is_exhausted_above_the_hold_mark_until_the_queue_is_taken() {
    let (queue, _inbox) = queue_holding_at_two(Duration::from_secs(10));
    let page = session(1);
    accept_bytes(&queue, &page, 0);
    accept_bytes(&queue, &page, 1);
    // Над отметкой: терпение выходит, места нет; очередь цела.
    let patience = Duration::from_millis(30);
    let (room, waited) = ask(&queue, &page, patience);
    assert_eq!(room, IntakeRoom::Exhausted);
    assert!(
        waited >= patience,
        "the question waits out its patience: {waited:?}"
    );
    assert!(!queue.is_empty_for_tests());

    // Поток приёма забирает очередь — ожидание кончается раньше терпения.
    let taker = {
        let queue = Arc::clone(&queue);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            match queue.wait_batch() {
                QueueTake::Batch(inputs) => inputs.len(),
                QueueTake::Shutdown => panic!("the queue is not shut down"),
            }
        })
    };
    let (room, waited) = ask(&queue, &page, Duration::from_secs(5));
    assert_eq!(room, IntakeRoom::Available);
    assert!(
        waited < Duration::from_secs(4),
        "released by the take, not by patience: {waited:?}"
    );
    assert_eq!(taker.join().expect("taker"), 2);
    assert_eq!(queue.counters().overflow_closes, 0);
}

#[test]
fn a_hold_without_any_take_becomes_an_overflow_close() {
    let hold_limit = Duration::from_millis(60);
    let (queue, inbox) = queue_holding_at_two(hold_limit);
    let page = session(7);
    accept_bytes(&queue, &page, 0);
    accept_bytes(&queue, &page, 1);
    // Никто не забирает: после предела очередь считает это переполнением и отпускает
    // поток соединения (ему дочитывать до закрытия).
    let (room, waited) = ask(&queue, &page, Duration::from_secs(5));
    assert_eq!(room, IntakeRoom::Available);
    assert!(
        waited >= hold_limit && waited < Duration::from_secs(4),
        "{waited:?}"
    );
    assert_eq!(
        close_requests(&inbox),
        vec![(page.clone(), SessionCloseReason::ProtocolViolation)]
    );
    let counters = queue.counters();
    assert_eq!(counters.overflow_closes, 1);
    assert_eq!(counters.holds_without_progress, 1);
    // Данные экземпляра вычищены, дальнейшие — выбрасываются со счётом.
    assert!(queue.is_empty_for_tests());
    assert_eq!(counters.dropped_overflow_items, 2);
    accept_bytes(&queue, &page, 2);
    assert_eq!(queue.counters().dropped_overflow_items, 3);
    // Переполненный экземпляр больше не придерживается.
    let (room, waited) = ask(&queue, &page, Duration::from_secs(1));
    assert_eq!(room, IntakeRoom::Available);
    assert!(waited < Duration::from_millis(500));
}

#[test]
fn a_take_before_the_limit_restarts_the_hold_clock() {
    let (queue, inbox) = queue_holding_at_two(Duration::from_millis(80));
    let page = session(1);
    accept_bytes(&queue, &page, 0);
    accept_bytes(&queue, &page, 1);
    assert_eq!(
        ask(&queue, &page, Duration::from_millis(50)).0,
        IntakeRoom::Exhausted
    );
    // Забор за 50 мс: удержание кончилось, предел не достигнут.
    assert!(matches!(queue.wait_batch(), QueueTake::Batch(inputs) if inputs.len() == 2));
    accept_bytes(&queue, &page, 2);
    accept_bytes(&queue, &page, 3);
    // Новое удержание считается с нуля: ещё 50 мс — всё ещё не переполнение.
    assert_eq!(
        ask(&queue, &page, Duration::from_millis(50)).0,
        IntakeRoom::Exhausted
    );
    assert_eq!(close_requests(&inbox), vec![]);
    assert_eq!(queue.counters().overflow_closes, 0);
}

#[test]
fn shutdown_releases_a_held_connection_thread() {
    let (queue, _inbox) = queue_holding_at_two(Duration::from_secs(10));
    let page = session(1);
    accept_bytes(&queue, &page, 0);
    accept_bytes(&queue, &page, 1);
    let asker = {
        let queue = Arc::clone(&queue);
        let page = page.clone();
        std::thread::spawn(move || ask(&queue, &page, Duration::from_secs(5)))
    };
    std::thread::sleep(Duration::from_millis(30));
    queue.shut_down();
    let (room, waited) = asker.join().expect("asker");
    assert_eq!(room, IntakeRoom::Available);
    assert!(
        waited < Duration::from_secs(4),
        "released by shutdown: {waited:?}"
    );
}

#[test]
fn the_byte_mark_holds_too() {
    let inbox = Arc::new(BrowserSessionInbox::new(
        InboxCapacity::default(),
        Arc::new(CountingWake::default()),
    ));
    let queue = OrderedSessionQueue::new(
        QueueLimits {
            hold_above_payload_bytes: 8,
            ..QueueLimits::default()
        },
        inbox,
    );
    let page = session(1);
    accept_bytes(&queue, &page, 0);
    assert_eq!(
        ask(&queue, &page, Duration::from_millis(10)).0,
        IntakeRoom::Available
    );
    accept_bytes(&queue, &page, 1);
    assert_eq!(
        ask(&queue, &page, Duration::from_millis(10)).0,
        IntakeRoom::Exhausted
    );
    assert!(matches!(
        queue.wait_batch(),
        QueueTake::Batch(inputs) if matches!(inputs.as_slice(), [SessionInput::Bytes(..), SessionInput::Bytes(..)])
    ));
}
