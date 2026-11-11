use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_protocol::decode_message;
use browser_bridge_protocol::session::{AdPod, AdSkip, PageAdState, TabGoneReason};
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
        ..InboxCapacity::default()
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
        ..InboxCapacity::default()
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
            agreed_protocol: browser_bridge_protocol::CURRENT_PROTOCOL_VERSION,
        });
    }
    inbox.publish_lifecycle(SessionLifecycleEvent::Ended {
        session: key.clone(),
        reason: SessionEndReason::TabGone {
            reason: TabGoneReason::NavigatedAway,
        },
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
            reason: SessionEndReason::TabGone {
                reason: TabGoneReason::NavigatedAway,
            },
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

// ---- уведомления потока приёма байтов (сессия 03b) ---------------------------

fn close_notice(text: &str) -> IntakeNotice {
    IntakeNotice::CloseSession {
        session: session(text),
        reason: SessionCloseReason::ProtocolViolation,
    }
}

fn pause_intent(text: &str) -> IntakeNotice {
    IntakeNotice::PageIntent {
        session: session(text),
        intent: crate::browser_session::intake_notice::PageIntent::Pause,
    }
}

fn reject_notice(text: &str) -> IntakeNotice {
    IntakeNotice::Reject {
        session: session(text),
        generation: browser_bridge_protocol::StreamGeneration::FIRST,
        reason: browser_bridge_protocol::session::StreamRejection::ProtectedContent,
    }
}

/// Уведомление в сравнимой форме (у `IntakeNotice` нет `PartialEq` из-за стороны открытия).
fn describe(notice: &IntakeNotice) -> String {
    format!("{notice:?}")
}

#[test]
fn intake_notices_keep_their_order_bytes_hints_coalesce_and_the_loop_wakes_once() {
    let (inbox, wake) = inbox_with(InboxCapacity::default());
    let delivery = inbox.post_intake_notices([
        close_notice("a"),
        IntakeNotice::BytesAppended {
            session: session("a"),
        },
        pause_intent("a"),
        IntakeNotice::BytesAppended {
            session: session("b"),
        },
        reject_notice("b"),
    ]);

    assert_eq!(delivery.queued, 5);
    assert_eq!(wake.0.load(Ordering::SeqCst), 1, "one wake per batch");
    let batch = inbox.take_batch();
    assert_eq!(
        batch
            .intake_notices
            .iter()
            .map(describe)
            .collect::<Vec<_>>(),
        vec![
            describe(&close_notice("a")),
            describe(&pause_intent("a")),
            describe(&reject_notice("b")),
        ]
    );
    // Слитая подсказка помнит последний сеанс.
    assert_eq!(batch.bytes_appended, Some(session("b")));
    assert!(inbox.take_batch().is_empty());
}

#[test]
fn a_full_notice_queue_drops_only_page_intents() {
    let (inbox, _wake) = inbox_with(InboxCapacity {
        intake_notices: 1,
        ..InboxCapacity::default()
    });
    let delivery =
        inbox.post_intake_notices([close_notice("a"), pause_intent("a"), reject_notice("a")]);

    assert_eq!(
        delivery,
        NoticeDelivery {
            queued: 2,
            dropped_hints: 1,
            superseded_page_seeks: 0,
            dropped_after_close: 0,
        }
    );
    assert_eq!(
        inbox
            .take_batch()
            .intake_notices
            .iter()
            .map(describe)
            .collect::<Vec<_>>(),
        vec![describe(&close_notice("a")), describe(&reject_notice("a"))]
    );
}

fn page_seek(text: &str, target_us: u64) -> IntakeNotice {
    IntakeNotice::PageIntent {
        session: session(text),
        intent: crate::browser_session::intake_notice::PageIntent::FollowPageSeek(
            browser_bridge_protocol::MediaTime::from_micros(target_us).expect("time"),
        ),
    }
}

fn seek_acknowledged(text: &str) -> IntakeNotice {
    IntakeNotice::PlayerSeekAcknowledged {
        session: session(text),
        seek_id: browser_bridge_protocol::SeekId(7),
        target: browser_bridge_protocol::MediaTime::ZERO,
    }
}

/// Квитанцию перемотки страница может прислать сколько угодно раз (её номер не
/// сверяется): при полном ящике она теряется, а не растит ящик.
#[test]
fn a_full_notice_queue_drops_player_seek_acknowledgements() {
    let (inbox, _wake) = inbox_with(InboxCapacity {
        intake_notices: 1,
        ..InboxCapacity::default()
    });
    let delivery = inbox.post_intake_notices(
        std::iter::once(close_notice("a")).chain((0..100).map(|_| seek_acknowledged("a"))),
    );

    assert_eq!(delivery.queued, 1);
    assert_eq!(delivery.dropped_hints, 100);
    assert_eq!(
        inbox
            .take_batch()
            .intake_notices
            .iter()
            .map(describe)
            .collect::<Vec<_>>(),
        vec![describe(&close_notice("a"))]
    );
}

/// Перемотку страницей источник уже исполнил: при полном ящике она всё равно
/// доходит до цикла, а поток перемоток не растит ящик — новая вытесняет прежнюю
/// неисполненную перемотку того же сеанса (перемотки другого сеанса не трогаются).
#[test]
fn a_full_notice_queue_still_delivers_the_latest_page_seek_of_each_session() {
    let (inbox, _wake) = inbox_with(InboxCapacity {
        intake_notices: 1,
        ..InboxCapacity::default()
    });
    let delivery = inbox.post_intake_notices([
        close_notice("a"),
        page_seek("b", 1_000),
        pause_intent("a"),
        page_seek("a", 1_000),
        page_seek("a", 2_000),
        page_seek("a", 3_000),
    ]);

    assert_eq!(
        delivery,
        NoticeDelivery {
            queued: 5,
            dropped_hints: 1,
            superseded_page_seeks: 2,
            dropped_after_close: 0,
        }
    );
    assert_eq!(
        inbox
            .take_batch()
            .intake_notices
            .iter()
            .map(describe)
            .collect::<Vec<_>>(),
        vec![
            describe(&close_notice("a")),
            describe(&page_seek("b", 1_000)),
            describe(&page_seek("a", 3_000)),
        ]
    );
}

#[test]
fn notices_after_close_are_dropped_and_reported_without_waking() {
    let (inbox, wake) = inbox_with(InboxCapacity::default());
    inbox.post_intake_notices([close_notice("a")]);
    inbox.close();
    let woken_before = wake.0.load(Ordering::SeqCst);

    let delivery = inbox.post_intake_notices([
        reject_notice("a"),
        IntakeNotice::BytesAppended {
            session: session("a"),
        },
    ]);

    assert_eq!(delivery.dropped_after_close, 2);
    assert_eq!(delivery.queued, 0);
    assert_eq!(wake.0.load(Ordering::SeqCst), woken_before);
    // Закрытие выбросило и то, что лежало до него.
    assert!(inbox.take_batch().is_empty());
}

fn ad_state_notice(text: &str, state: PageAdState) -> IntakeNotice {
    IntakeNotice::AdState {
        session: session(text),
        state,
    }
}

/// Состояние рекламы терять нельзя (потерянное «рекламы нет» оставило бы плашку навсегда),
/// но важно только последнее: при полном ящике новое слово вытесняет прежнее того же
/// сеанса, чужой сеанс не трогается, а остальные уведомления остаются.
#[test]
fn a_full_notice_queue_keeps_only_the_latest_ad_state_of_each_session() {
    let (inbox, _wake) = inbox_with(InboxCapacity {
        intake_notices: 1,
        ..InboxCapacity::default()
    });
    let available = PageAdState::Showing {
        pod: AdPod::Unknown {},
        skip: AdSkip::Available {},
    };
    inbox.post_intake_notices([
        close_notice("a"),
        ad_state_notice("b", available),
        ad_state_notice("a", available),
        ad_state_notice("a", PageAdState::None {}),
    ]);

    assert_eq!(
        inbox
            .take_batch()
            .intake_notices
            .iter()
            .map(describe)
            .collect::<Vec<_>>(),
        vec![
            describe(&close_notice("a")),
            describe(&ad_state_notice("b", available)),
            describe(&ad_state_notice("a", PageAdState::None {})),
        ],
        "the stale word of session a is gone, the final `none` is delivered"
    );
}

#[test]
fn a_finished_page_subtitle_document_is_never_dropped_from_a_full_queue() {
    let (inbox, _wake) = inbox_with(InboxCapacity {
        intake_notices: 1,
        ..InboxCapacity::default()
    });
    let document = |track: u32| IntakeNotice::PageSubtitleReady {
        session: session("a"),
        document: Box::new(crate::page_subtitles::PageSubtitleDocument {
            track: crate::page_subtitles::PageSubtitleTrackNumber::new(track),
            language: crate::page_subtitles::PageSubtitleLanguage::NotNamed,
            label: crate::page_subtitles::PageSubtitleLabel::NotGiven,
            format_hint: crate::page_subtitles::PageSubtitleFormatHint::Srt,
            display: crate::page_subtitles::PageSubtitleDisplay::NotShowingOnPage,
            source: crate::page_subtitles::PageSubtitleSource::PageDownload,
            bytes: std::sync::Arc::from(b"1".as_slice()),
        }),
    };
    // Страница второй раз документ не пришлёт: оба ложатся даже сверх ёмкости.
    let delivery = inbox.post_intake_notices([document(1), document(2)]);
    assert_eq!(delivery.queued, 2);
    assert_eq!(delivery.dropped_hints, 0);
    assert_eq!(inbox.take_batch().intake_notices.len(), 2);
}

#[test]
fn a_newer_variant_menu_replaces_the_pending_one_of_its_kind_only() {
    use crate::browser_session::page_variants::PageVariantUpdate;
    use browser_bridge_protocol::session::{NextEpisodeAvailability, VariantKind, VariantMenu};

    let (inbox, _wake) = inbox_with(InboxCapacity {
        intake_notices: 1,
        ..InboxCapacity::default()
    });
    let menu_notice = |kind: VariantKind| IntakeNotice::PageVariants {
        session: session("a"),
        update: PageVariantUpdate::Menu(VariantMenu {
            kind,
            options: Vec::new(),
        }),
    };
    let navigation = |availability| IntakeNotice::PageVariants {
        session: session("a"),
        update: PageVariantUpdate::Navigation(availability),
    };
    // Ящик полон после первого меню: следующее меню того же вида вытесняет его, другой вид
    // и навигация ложатся рядом, навигация вытесняет только навигацию.
    let delivery = inbox.post_intake_notices([
        menu_notice(VariantKind::Audio),
        navigation(NextEpisodeAvailability::Available),
        menu_notice(VariantKind::Quality),
        menu_notice(VariantKind::Audio),
        navigation(NextEpisodeAvailability::Unavailable),
    ]);
    assert_eq!(delivery.queued, 5);
    let slots: Vec<_> = inbox
        .take_batch()
        .intake_notices
        .iter()
        .map(|notice| match notice {
            IntakeNotice::PageVariants {
                update: PageVariantUpdate::Menu(menu),
                ..
            } => format!("menu {:?}", menu.kind),
            IntakeNotice::PageVariants {
                update: PageVariantUpdate::Navigation(availability),
                ..
            } => format!("navigation {availability:?}"),
            other => panic!("unexpected notice {other:?}"),
        })
        .collect();
    assert_eq!(
        slots,
        ["menu Quality", "menu Audio", "navigation Unavailable"],
        "the newest word of every place is delivered, the stale ones are gone"
    );
}

#[test]
fn a_variant_outcome_has_its_own_place_and_is_replaced_only_by_a_newer_one_of_its_subject() {
    use crate::browser_session::page_variants::{PageVariantOutcomeWord, PageVariantUpdate};
    use browser_bridge_protocol::session::{
        VariantKind, VariantMenu, VariantOutcome, VariantRequest,
    };
    use browser_bridge_protocol::text::VariantId;

    let (inbox, _wake) = inbox_with(InboxCapacity {
        intake_notices: 1,
        ..InboxCapacity::default()
    });
    let outcome = |request: VariantRequest, outcome: VariantOutcome| IntakeNotice::PageVariants {
        session: session("a"),
        update: PageVariantUpdate::Outcome(PageVariantOutcomeWord { request, outcome }),
    };
    let select = |menu: VariantKind, variant: &str| VariantRequest::SelectVariant {
        menu,
        variant_id: VariantId::new(variant).expect("id"),
    };
    let audio_menu = IntakeNotice::PageVariants {
        session: session("a"),
        update: PageVariantUpdate::Menu(VariantMenu {
            kind: VariantKind::Audio,
            options: Vec::new(),
        }),
    };
    // Ящик полон: меню того же вида НЕ вытесняет исход озвучки, исход качества — тоже, а
    // новый исход озвучки заменяет прежний неисполненный.
    inbox.post_intake_notices([
        outcome(select(VariantKind::Audio, "old"), VariantOutcome::Failed),
        audio_menu,
        outcome(select(VariantKind::Quality, "720"), VariantOutcome::Done),
        outcome(
            select(VariantKind::Audio, "new"),
            VariantOutcome::PersonOnly,
        ),
        outcome(VariantRequest::NextEpisode {}, VariantOutcome::Failed),
    ]);
    let words: Vec<_> = inbox
        .take_batch()
        .intake_notices
        .iter()
        .map(|notice| match notice {
            IntakeNotice::PageVariants {
                update: PageVariantUpdate::Menu(menu),
                ..
            } => format!("menu {:?}", menu.kind),
            IntakeNotice::PageVariants {
                update: PageVariantUpdate::Outcome(word),
                ..
            } => format!("outcome {:?} {:?}", word.request, word.outcome),
            other => panic!("unexpected notice {other:?}"),
        })
        .collect();
    assert_eq!(words.len(), 4, "{words:?}");
    assert!(words[0].starts_with("menu Audio"), "{words:?}");
    assert!(
        words[1].contains("Quality") && words[1].contains("Done"),
        "{words:?}"
    );
    assert!(
        words[2].contains("\"new\"") && words[2].contains("PersonOnly"),
        "the newer audio outcome replaced the older one: {words:?}"
    );
    assert!(words[3].contains("NextEpisode"), "{words:?}");
}
