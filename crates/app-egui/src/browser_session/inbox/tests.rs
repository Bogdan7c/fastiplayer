use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_protocol::decode_message;
use browser_bridge_session::SessionEndReason;
use desktop_integration::{BrowserEntryCaller, BrowserEntryReply, BrowserEntryRequest};

use super::*;

/// Считает пробуждения, чтобы видеть, что ящик будит цикл ровно на запись.
#[derive(Default)]
struct CountingWake(AtomicUsize);

impl BrowserSessionWake for CountingWake {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn inbox_with(capacity: InboxCapacity) -> (BrowserSessionInbox, Arc<CountingWake>) {
    let wake = Arc::new(CountingWake::default());
    (BrowserSessionInbox::new(capacity, wake.clone()), wake)
}

/// Доставка, ответ на которую уходит в канал (`Disconnected` = слот уничтожен без ответа).
fn delivery(message: &str) -> (BrowserEntryDelivery, mpsc::Receiver<String>) {
    let (reply_sender, reply_receiver) = mpsc::channel();
    let reply = BrowserEntryReply::from_callback(move |reply_text| {
        let _ = reply_sender.send(reply_text);
    });
    let request = BrowserEntryRequest {
        caller: BrowserEntryCaller::from_sender_name(":1.42".to_owned()),
        message: message.to_owned(),
    };
    (BrowserEntryDelivery::new(request, reply), reply_receiver)
}

fn session(text: &str) -> SessionRef {
    SessionRef {
        key: browser_bridge_protocol::SessionKey::new(text).expect("valid session key"),
        instance: browser_bridge_session::SessionInstance(1),
    }
}

/// Настоящие фикстуры протокола: формы сообщений не угадываются в тестах.
const TAB_GONE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../browser-bridge-protocol/fixtures/session/valid/page-tab-gone.json"
));

fn tab_gone() -> PageSessionMessage {
    decode_message(TAB_GONE.trim_end().as_bytes()).expect("tab_gone fixture")
}

#[test]
fn entry_calls_come_out_in_order_and_each_one_wakes_the_loop() {
    let (inbox, wake) = inbox_with(InboxCapacity::default());
    let (first, _first_reply) = delivery("first");
    let (second, _second_reply) = delivery("second");

    inbox.deliver(first).expect("room for the first call");
    inbox.deliver(second).expect("room for the second call");

    let messages: Vec<String> = inbox
        .take_batch()
        .entry_deliveries
        .into_iter()
        .map(|delivery| delivery.into_parts().0.message)
        .collect();
    assert_eq!(messages, ["first", "second"]);
    assert_eq!(wake.0.load(Ordering::SeqCst), 2);
    assert!(
        inbox.take_batch().is_empty(),
        "a batch is taken exactly once"
    );
}

#[test]
fn full_entry_queue_reports_full_and_keeps_what_it_already_holds() {
    let (inbox, wake) = inbox_with(InboxCapacity {
        entry_deliveries: 1,
        page_messages: 1,
    });
    let (kept, _kept_reply) = delivery("kept");
    let (rejected, rejected_reply) = delivery("rejected");

    inbox.deliver(kept).expect("room for one call");
    assert_eq!(inbox.deliver(rejected), Err(BrowserEntrySinkError::Full));

    // Отвергнутая доставка вернулась транспорту: её слот уже уничтожен без ответа.
    assert_eq!(
        rejected_reply.recv_timeout(Duration::from_secs(1)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    let batch = inbox.take_batch();
    assert_eq!(batch.entry_deliveries.len(), 1);
    assert_eq!(
        wake.0.load(Ordering::SeqCst),
        1,
        "a rejected call does not wake the loop"
    );
}

#[test]
fn closing_rejects_new_calls_and_releases_waiting_reply_slots_without_an_answer() {
    let (inbox, _wake) = inbox_with(InboxCapacity::default());
    let (waiting, waiting_reply) = delivery("waiting");
    inbox.deliver(waiting).expect("room before close");

    inbox.close();

    // Слот ждавшего звонка уничтожен: транспорт ответит звонящему `ShuttingDown`.
    assert_eq!(
        waiting_reply.recv_timeout(Duration::from_secs(1)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    let (late, _late_reply) = delivery("late");
    assert_eq!(inbox.deliver(late), Err(BrowserEntrySinkError::Closed));
    assert!(inbox.take_batch().is_empty());
}

#[test]
fn page_messages_are_bounded_but_lifecycle_events_are_never_dropped() {
    let (inbox, _wake) = inbox_with(InboxCapacity {
        entry_deliveries: 1,
        page_messages: 1,
    });
    let key = session("tab-1");

    assert_eq!(inbox.publish_page_message(&key, tab_gone()), Ok(()));
    assert_eq!(
        inbox.publish_page_message(&key, tab_gone()),
        Err(EventSinkFull)
    );
    for _ in 0..100 {
        inbox.publish_lifecycle(SessionLifecycleEvent::Attached {
            session: key.clone(),
        });
    }
    inbox.publish_lifecycle(SessionLifecycleEvent::Ended {
        session: key.clone(),
        reason: SessionEndReason::ClosedByPlayer(SessionCloseReason::TabGone),
    });

    let batch = inbox.take_batch();
    assert_eq!(
        batch.page_messages,
        vec![PageMessageEnvelope {
            session: key.clone(),
            message: tab_gone(),
        }]
    );
    assert_eq!(batch.lifecycle_events.len(), 101);
    assert_eq!(
        batch.lifecycle_events.last(),
        Some(&SessionLifecycleEvent::Ended {
            session: key,
            reason: SessionEndReason::ClosedByPlayer(SessionCloseReason::TabGone),
        })
    );
}

#[test]
fn parking_wake_returns_at_once_when_woken_before_waiting() {
    let wake = ParkingWake::default();
    wake.wake();
    let started = Instant::now();
    wake.wait(None);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn parking_wake_without_a_signal_returns_at_the_deadline() {
    let wake = ParkingWake::default();
    let deadline = Instant::now() + Duration::from_millis(30);
    wake.wait(Some(deadline));
    assert!(Instant::now() >= deadline);
}

#[test]
fn parking_wake_is_woken_from_another_thread() {
    let wake = Arc::new(ParkingWake::default());
    let waker = wake.clone();
    let signaller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        waker.wake();
    });
    let started = Instant::now();
    // Срок далеко: вернуться раньше может только пробуждение.
    wake.wait(Some(started + Duration::from_secs(30)));
    assert!(started.elapsed() < Duration::from_secs(10));
    signaller.join().expect("signaller thread");
}
