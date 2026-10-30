use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use browser_bridge_protocol::{RangeRequestId, SessionKey};
use browser_bridge_session::{CountingByteSink, RangeChunk, SessionEndReason, SourceBufferChunk};

use super::*;
use crate::browser_session::inbox::{BrowserSessionWake, InboxCapacity};

#[derive(Default)]
struct CountingWake(AtomicUsize);

impl BrowserSessionWake for CountingWake {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn inbox() -> Arc<BrowserSessionInbox> {
    Arc::new(BrowserSessionInbox::new(
        InboxCapacity::default(),
        Arc::new(CountingWake::default()),
    ))
}

fn queue_with(limits: QueueLimits) -> (OrderedSessionQueue, Arc<BrowserSessionInbox>) {
    let inbox = inbox();
    (OrderedSessionQueue::new(limits, Arc::clone(&inbox)), inbox)
}

fn session(key: &str, instance: u64) -> SessionRef {
    SessionRef {
        key: SessionKey::new(key).expect("valid key"),
        instance: SessionInstance(instance),
    }
}

const GENERATION: StreamGeneration = StreamGeneration::FIRST;

fn source_buffer(index: u16) -> SourceBufferIndex {
    SourceBufferIndex::new(index).expect("valid index")
}

fn end_of_stream() -> PageSessionMessage {
    PageSessionMessage::EndOfStream {
        generation: GENERATION,
    }
}

fn generation_started() -> PageSessionMessage {
    PageSessionMessage::GenerationStarted {
        generation: GENERATION,
    }
}

/// Кладёт байты SourceBuffer так, как это делает поток соединения сервера.
fn accept_bytes(sink: &dyn SessionByteSink, session: &SessionRef, sequence: u64, payload: &[u8]) {
    sink.accept(SessionChunk {
        session,
        source: ChunkSource::SourceBuffer(SourceBufferChunk {
            generation: GENERATION,
            source_buffer: source_buffer(0),
            sequence: AppendSequence(sequence),
            payload,
        }),
    });
}

fn owned(sequence: u64, payload: &[u8]) -> OwnedChunk {
    OwnedChunk {
        generation: GENERATION,
        source_buffer: source_buffer(0),
        sequence: AppendSequence(sequence),
        payload: payload.to_vec(),
    }
}

fn take(queue: &OrderedSessionQueue) -> Vec<SessionInput> {
    match queue.wait_batch() {
        QueueTake::Batch(inputs) => inputs,
        QueueTake::Shutdown => panic!("the queue was not shut down"),
    }
}

/// Уведомления «закрыть сеанс», которые очередь положила в ящик.
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

#[test]
fn interleaved_messages_and_bytes_come_out_in_wire_order() {
    let (queue, _inbox) = queue_with(QueueLimits::default());
    let page = session("a", 1);
    queue
        .publish_page_message(&page, generation_started())
        .expect("accepted");
    accept_bytes(&queue, &page, 0, b"first");
    queue
        .publish_page_message(&page, end_of_stream())
        .expect("accepted");
    accept_bytes(&queue, &page, 1, b"second");

    assert_eq!(
        take(&queue),
        vec![
            SessionInput::Message(page.clone(), generation_started()),
            SessionInput::Bytes(page.clone(), owned(0, b"first")),
            SessionInput::Message(page.clone(), end_of_stream()),
            SessionInput::Bytes(page, owned(1, b"second")),
        ]
    );
}

#[test]
fn session_end_is_queued_after_all_bytes_of_the_instance() {
    let (queue, _inbox) = queue_with(QueueLimits::default());
    let page = session("a", 1);
    accept_bytes(&queue, &page, 0, b"one");
    accept_bytes(&queue, &page, 1, b"two");
    queue.session_ended(&page);

    assert_eq!(
        take(&queue),
        vec![
            SessionInput::Bytes(page.clone(), owned(0, b"one")),
            SessionInput::Bytes(page.clone(), owned(1, b"two")),
            SessionInput::Ended(page),
        ]
    );
}

#[test]
fn item_overflow_closes_the_instance_once_drops_its_input_and_keeps_others() {
    let (queue, inbox) = queue_with(QueueLimits {
        max_payload_bytes: 1024,
        max_items: 3,
    });
    let other = session("other", 1);
    let flooding = session("flood", 2);
    queue
        .publish_page_message(&other, end_of_stream())
        .expect("accepted");
    accept_bytes(&queue, &flooding, 0, b"a");
    accept_bytes(&queue, &flooding, 1, b"b");
    // Четвёртый элемент переполняет очередь: сообщение не отвергается серверу.
    assert_eq!(
        queue.publish_page_message(&flooding, end_of_stream()),
        Ok(())
    );
    // Дальнейшие данные экземпляра выбрасываются, повторной просьбы нет.
    accept_bytes(&queue, &flooding, 2, b"c");
    assert_eq!(
        queue.publish_page_message(&flooding, end_of_stream()),
        Ok(())
    );

    assert_eq!(
        close_requests(&inbox),
        vec![(flooding.clone(), SessionCloseReason::ProtocolViolation)]
    );
    let counters = queue.counters();
    assert_eq!(counters.overflow_closes, 1);
    // 2 вычищенных из очереди + переполнивший + 2 после него.
    assert_eq!(counters.dropped_overflow_items, 5);
    // Конец экземпляра всё равно доходит, а данные другого сеанса целы.
    queue.session_ended(&flooding);
    assert_eq!(
        take(&queue),
        vec![
            SessionInput::Message(other, end_of_stream()),
            SessionInput::Ended(flooding),
        ]
    );
}

#[test]
fn byte_overflow_closes_the_instance_and_memory_stays_bounded() {
    let (queue, inbox) = queue_with(QueueLimits {
        max_payload_bytes: 10,
        max_items: 100,
    });
    let page = session("a", 1);
    accept_bytes(&queue, &page, 0, &[1; 6]);
    accept_bytes(&queue, &page, 1, &[2; 6]);
    for sequence in 2..50 {
        accept_bytes(&queue, &page, sequence, &[3; 6]);
    }

    assert_eq!(
        close_requests(&inbox),
        vec![(page.clone(), SessionCloseReason::ProtocolViolation)]
    );
    let counters = queue.counters();
    // Принятые сервером рамки считаются все — как у прежнего счётчика.
    assert_eq!(counters.received_frames, 50);
    assert_eq!(counters.received_bytes, 300);
    assert_eq!(counters.dropped_overflow_items, 50);
    // В очереди не осталось ничего от закрываемого экземпляра.
    queue.session_ended(&page);
    assert_eq!(take(&queue), vec![SessionInput::Ended(page.clone())]);
    // После конца новый экземпляр с тем же ключом принимается снова.
    let next = session("a", 2);
    accept_bytes(&queue, &next, 0, &[4; 6]);
    assert_eq!(
        take(&queue),
        vec![SessionInput::Bytes(
            next,
            OwnedChunk {
                payload: vec![4; 6],
                ..owned(0, b"")
            }
        )]
    );
}

#[test]
fn lifecycle_events_reach_the_inbox_even_when_the_queue_is_full() {
    let (queue, inbox) = queue_with(QueueLimits {
        max_payload_bytes: 1024,
        max_items: 1,
    });
    let page = session("a", 1);
    accept_bytes(&queue, &page, 0, b"x");
    accept_bytes(&queue, &page, 1, b"y");
    let ended = SessionLifecycleEvent::Ended {
        session: page.clone(),
        reason: SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation),
    };
    queue.publish_lifecycle(SessionLifecycleEvent::Attached {
        session: page.clone(),
    });
    queue.publish_lifecycle(ended.clone());

    let batch = inbox.take_batch();
    assert_eq!(
        batch.lifecycle_events,
        vec![SessionLifecycleEvent::Attached { session: page }, ended]
    );
}

#[test]
fn received_totals_match_the_old_counting_sink_including_range_bytes() {
    let (queue, _inbox) = queue_with(QueueLimits::default());
    let counting = CountingByteSink::new();
    let page = session("a", 1);
    let range = |sink: &dyn SessionByteSink| {
        sink.accept(SessionChunk {
            session: &page,
            source: ChunkSource::Range(RangeChunk {
                request_id: RangeRequestId(7),
                offset: 0,
                payload: &[9; 11],
            }),
        });
    };
    for sink in [&queue as &dyn SessionByteSink, &counting] {
        accept_bytes(sink, &page, 0, &[1; 5]);
        accept_bytes(sink, &page, 1, &[2; 3]);
        range(sink);
    }

    let snapshot = counting.snapshot();
    let (frames, bytes) = snapshot
        .source_buffers
        .values()
        .chain(snapshot.ranges.values())
        .fold((0, 0), |(frames, bytes), counters| {
            (frames + counters.frames, bytes + counters.bytes)
        });
    let counters = queue.counters();
    assert_eq!(
        (counters.received_frames, counters.received_bytes),
        (frames, bytes)
    );
    assert_eq!((frames, bytes), (3, 19));
    // Байты `<video src>` до 03c только считаются: в очередь они не попали.
    assert_eq!(counters.dropped_range_frames, 1);
    assert_eq!(
        take(&queue),
        vec![
            SessionInput::Bytes(page.clone(), owned(0, &[1; 5])),
            SessionInput::Bytes(page, owned(1, &[2; 3])),
        ]
    );
}

#[test]
fn shutdown_wakes_a_waiting_taker_and_later_input_is_dropped_and_counted() {
    let (queue, _inbox) = queue_with(QueueLimits::default());
    let queue = Arc::new(queue);
    let waiter = {
        let queue = Arc::clone(&queue);
        std::thread::spawn(move || matches!(queue.wait_batch(), QueueTake::Shutdown))
    };
    // Даём ожидающему уснуть (результат от этого не зависит: флаг остановки
    // проверяется и до ожидания).
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(20) {
        std::thread::yield_now();
    }
    queue.shut_down();
    assert!(waiter.join().expect("waiter thread"));
    accept_bytes(queue.as_ref(), &session("a", 1), 0, b"late");
    assert_eq!(queue.counters().dropped_after_shutdown, 1);
}

/// Сообщение страницы с длинным MIME (≈ 250 байтов JSON) — крупное, но законное.
fn large_message() -> PageSessionMessage {
    let mime = format!("video/mp4; codecs=\"{}\"", "a".repeat(200));
    PageSessionMessage::SourceBufferAdded {
        generation: GENERATION,
        source_buffer: source_buffer(0),
        mime_type: browser_bridge_protocol::MimeType::new(&mime).expect("valid MIME"),
    }
}

/// Потолок байтов считает и сообщения страницы: поток крупных сообщений при
/// вставшем разборе закрывает экземпляр, и в очереди держится не больше потолка.
#[test]
fn page_messages_count_against_the_byte_limit_and_memory_stays_bounded() {
    const LIMIT: usize = 16 * 1024;
    let (queue, inbox) = queue_with(QueueLimits {
        max_payload_bytes: LIMIT,
        max_items: 100_000,
    });
    let page = session("a", 1);
    let message_bytes = encode_message(&large_message()).expect("encodes").len();
    assert!(message_bytes > 200, "{message_bytes}");

    let mut peak_held = 0;
    for _ in 0..1_000 {
        queue
            .publish_page_message(&page, large_message())
            .expect("the queue never refuses");
        peak_held = peak_held.max(queue.held_bytes_for_tests());
    }

    assert_eq!(
        close_requests(&inbox),
        vec![(page.clone(), SessionCloseReason::ProtocolViolation)]
    );
    assert_eq!(queue.counters().overflow_closes, 1);
    assert!(peak_held <= LIMIT, "held {peak_held} bytes");
    // Принятые до переполнения вычищены, переполнившее и следующие — выброшены.
    assert_eq!(queue.counters().dropped_overflow_items, 1_000);
    assert_eq!(queue.held_bytes_for_tests(), 0);
}
