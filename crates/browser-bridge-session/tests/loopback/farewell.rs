//! Прощание (решение 127): последнее сообщение плеера вкладке перед закрытием.
//!
//! - замена вкладки: старое соединение в «коридоре прощания» ждёт
//!   `bid_farewell_to_replaced` до `farewell_grace`, потом 4001;
//! - закрытие окна: `close_session_with_farewell` на активном экземпляре.

use std::time::{Duration, Instant};

use browser_bridge_protocol::{PlayerSessionMessage, SessionCloseReason};
use browser_bridge_session::{
    ActiveFarewellOutcome, BrowserSessionPolicy, ReplacedFarewellOutcome, SendToPageError,
    SessionEndReason, SessionLifecycleEvent,
};

use crate::support::{
    Env, RETURN_TO_PAGE_JSON, attach, data_frame_for, let_the_connection_thread_park,
    parked_reader_policy, read_text, read_texts_until_close, return_to_page, round_trip_ping,
    send_binary, send_text, session, test_policy,
};

const REPLACED: u16 = 4001;

fn policy_with_grace(farewell_grace: Duration) -> BrowserSessionPolicy {
    BrowserSessionPolicy {
        farewell_grace,
        ..test_policy()
    }
}

fn frame_with_sequence(sequence: u64) -> Vec<u8> {
    let mut frame = data_frame_for(1, 0);
    frame[16..24].copy_from_slice(&sequence.to_le_bytes());
    frame
}

fn start_stream(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) {
    send_text(socket, r#"{"type":"generation_started","generation":1}"#);
    send_text(
        socket,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4"}"#,
    );
}

fn ended_by_player(
    target: browser_bridge_session::SessionRef,
    reason: SessionCloseReason,
) -> impl Fn(&SessionLifecycleEvent) -> bool {
    move |event| {
        event
            == &SessionLifecycleEvent::Ended {
                session: target.clone(),
                reason: SessionEndReason::ClosedByPlayer(reason),
            }
    }
}

// ---- замена вкладки -------------------------------------------------------

#[test]
fn replaced_tab_gets_the_farewell_then_4001_and_nothing_it_sends_reaches_the_sinks() {
    let env = Env::start_with(policy_with_grace(Duration::from_secs(10)));
    let old_token = env.open("old").token;
    let mut old = attach(env.port(), "old", &old_token);
    start_stream(&mut old);
    send_binary(&mut old, frame_with_sequence(0));
    env.bytes.received.wait_for(
        "первая рамка старой вкладки",
        |received| received.frames == 1,
    );

    let replacing = env.open("new");
    let old_ref = replacing
        .replaced
        .clone()
        .expect("замена называет старый экземпляр");
    // Конец старого сеанса объявлен сразу, как и до 04b.
    env.wait_lifecycle(
        "Ended старого — сразу при замене",
        ended_by_player(old_ref.clone(), SessionCloseReason::ReplacedByAnotherTab),
    );
    assert!(env.bytes.received.get().ended.contains(&old_ref));

    // Коридор: старая вкладка ещё шлёт — сервер читает и выбрасывает.
    send_binary(&mut old, frame_with_sequence(1));
    send_text(&mut old, r#"{"type":"playback_state","state":"playing"}"#);
    send_text(&mut old, "garbage that would otherwise be a 4003");
    round_trip_ping(&mut old, b"corridor");
    let page_messages_before = env.events.seen.get().page_messages.len();

    // Новая вкладка подключается; прощание старой ей не достаётся.
    let mut new = attach(env.port(), "new", &replacing.token);
    env.control
        .send_to_page(&PlayerSessionMessage::Play {})
        .unwrap();
    assert_eq!(read_text(&mut new), r#"{"type":"play"}"#);

    assert_eq!(
        env.control
            .bid_farewell_to_replaced(&old_ref, &return_to_page()),
        Ok(ReplacedFarewellOutcome::FarewellQueued)
    );
    let (texts, code) = read_texts_until_close(&mut old);
    assert_eq!(texts, vec![RETURN_TO_PAGE_JSON.to_owned()]);
    assert_eq!(code, REPLACED);

    let received = env.bytes.received.get();
    assert_eq!(
        received.frames, 1,
        "рамки из коридора не дошли до приёмника"
    );
    assert_eq!(received.late_chunks, 0);
    assert_eq!(
        env.events.seen.get().page_messages.len(),
        page_messages_before,
        "сообщения из коридора не дошли до приёмника событий"
    );
    // Новое соединение живо и не получило ничего лишнего.
    env.control
        .send_to_page(&PlayerSessionMessage::Pause {})
        .unwrap();
    assert_eq!(read_text(&mut new), r#"{"type":"pause"}"#);
    // Второе прощание тому же экземпляру — коридор уже закрыт.
    assert_eq!(
        env.control
            .bid_farewell_to_replaced(&old_ref, &return_to_page()),
        Ok(ReplacedFarewellOutcome::CorridorClosed)
    );
}

/// Плеер промолчал: сервер закрывает 4001 сам, но не раньше срока коридора.
/// На старом коде (4001 сразу при замене) тест падает по нижней границе времени.
#[test]
fn replaced_tab_without_a_farewell_is_closed_with_4001_after_the_grace() {
    let grace = Duration::from_millis(400);
    let env = Env::start_with(policy_with_grace(grace));
    let old_token = env.open("old").token;
    let mut old = attach(env.port(), "old", &old_token);
    let replaced_at = Instant::now();
    let replacing = env.open("new");
    assert!(replacing.replaced.is_some());
    let (texts, code) = read_texts_until_close(&mut old);
    let waited = replaced_at.elapsed();
    assert!(texts.is_empty(), "без прощания текстов нет: {texts:?}");
    assert_eq!(code, REPLACED);
    assert!(waited >= grace, "закрыто раньше срока коридора: {waited:?}");
}

#[test]
fn farewell_after_the_corridor_expired_is_reported_and_nothing_is_sent() {
    let env = Env::start_with(policy_with_grace(Duration::from_millis(50)));
    let old_token = env.open("old").token;
    let mut old = attach(env.port(), "old", &old_token);
    let replacing = env.open("new");
    let old_ref = replacing.replaced.clone().unwrap();
    let (texts, code) = read_texts_until_close(&mut old);
    assert!(texts.is_empty());
    assert_eq!(code, REPLACED);

    let mut new = attach(env.port(), "new", &replacing.token);
    assert_eq!(
        env.control
            .bid_farewell_to_replaced(&old_ref, &return_to_page()),
        Ok(ReplacedFarewellOutcome::CorridorClosed)
    );
    // Опоздавшее прощание не ушло и новой вкладке: первое, что она видит, — play.
    env.control
        .send_to_page(&PlayerSessionMessage::Play {})
        .unwrap();
    assert_eq!(read_text(&mut new), r#"{"type":"play"}"#);
}

#[test]
fn farewell_to_the_active_or_an_unknown_instance_is_not_replaced_and_changes_nothing() {
    let env = Env::start();
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    let active = session("a", opened.instance.0);
    assert_eq!(
        env.control
            .bid_farewell_to_replaced(&active, &return_to_page()),
        Ok(ReplacedFarewellOutcome::NotReplaced),
        "активный сеанс — не заменённый"
    );
    assert_eq!(
        env.control
            .bid_farewell_to_replaced(&session("never", 99), &return_to_page()),
        Ok(ReplacedFarewellOutcome::NotReplaced)
    );
    // Активный сеанс не тронут: подключён, первое сообщение на проводе — play.
    env.control
        .send_to_page(&PlayerSessionMessage::Play {})
        .unwrap();
    assert_eq!(read_text(&mut socket), r#"{"type":"play"}"#);
    let ended_any = env
        .events
        .seen
        .get()
        .lifecycle
        .iter()
        .any(|event| matches!(event, SessionLifecycleEvent::Ended { .. }));
    assert!(!ended_any, "ни один сеанс не заканчивался");
}

#[test]
fn farewell_to_a_replaced_instance_that_never_attached_reports_no_connection() {
    let env = Env::start();
    env.open("old");
    let replacing = env.open("new");
    let old_ref = replacing.replaced.clone().unwrap();
    assert_eq!(
        env.control
            .bid_farewell_to_replaced(&old_ref, &return_to_page()),
        Ok(ReplacedFarewellOutcome::NoConnection)
    );
}

/// Выключение во время коридора: 4001 (сеанс кончился заменой), а принятое
/// прощание всё равно уходит первым.
#[test]
fn shutdown_during_a_corridor_writes_the_bid_farewell_then_4001() {
    let env = Env::start_with(policy_with_grace(Duration::from_secs(60)));
    let old_token = env.open("old").token;
    let mut old = attach(env.port(), "old", &old_token);
    let replacing = env.open("new");
    let old_ref = replacing.replaced.clone().unwrap();
    assert_eq!(
        env.control
            .bid_farewell_to_replaced(&old_ref, &return_to_page()),
        Ok(ReplacedFarewellOutcome::FarewellQueued)
    );
    let report = env.server.shutdown();
    assert_eq!(report.detached_threads, 0);
    let (texts, code) = read_texts_until_close(&mut old);
    assert_eq!(texts, vec![RETURN_TO_PAGE_JSON.to_owned()]);
    assert_eq!(code, REPLACED);
}

#[test]
fn shutdown_during_an_unbid_corridor_closes_it_with_4001_without_waiting_for_the_grace() {
    let env = Env::start_with(policy_with_grace(Duration::from_secs(60)));
    let old_token = env.open("old").token;
    let mut old = attach(env.port(), "old", &old_token);
    env.open("new");
    let started = Instant::now();
    let report = env.server.shutdown();
    assert_eq!(
        report.detached_threads, 0,
        "поток коридора не остался висеть"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "shutdown ждал срока коридора"
    );
    let (texts, code) = read_texts_until_close(&mut old);
    assert!(texts.is_empty());
    assert_eq!(code, REPLACED);
}

/// Поток замен не копит соединения: сверх предела самый старый коридор
/// закрывается сразу (задолго до своего срока), а его экземпляр забыт.
#[test]
fn corridors_beyond_the_bound_close_the_oldest_immediately() {
    let policy = BrowserSessionPolicy {
        farewell_grace: Duration::from_secs(60),
        max_farewell_corridors: 2,
        ..test_policy()
    };
    let env = Env::start_with(policy);
    let mut tabs = Vec::new();
    for key in ["s1", "s2", "s3"] {
        let opened = env.open(key);
        tabs.push((
            opened.replaced.clone(),
            attach(env.port(), key, &opened.token),
        ));
    }
    // s1 и s2 в коридорах (предел 2). s4 вытесняет s3 и переполняет таблицу.
    let fourth = env.open("s4");
    let s1 = session("s1", 1);
    let (_, mut s1_socket) = tabs.remove(0);
    // Срок коридора 60 с, ожидание чтения 20 с: код пришёл только из-за предела.
    let (texts, code) = read_texts_until_close(&mut s1_socket);
    assert!(texts.is_empty());
    assert_eq!(code, REPLACED);
    assert_eq!(
        env.control.bid_farewell_to_replaced(&s1, &return_to_page()),
        Ok(ReplacedFarewellOutcome::NotReplaced),
        "вытесненный экземпляр забыт"
    );
    // Более новые коридоры живы и принимают прощание.
    assert_eq!(fourth.replaced, Some(session("s3", 3)));
    for (index, key) in [(2_u64, "s2"), (3, "s3")] {
        assert_eq!(
            env.control
                .bid_farewell_to_replaced(&session(key, index), &return_to_page()),
            Ok(ReplacedFarewellOutcome::FarewellQueued),
            "коридор {key}"
        );
    }
    for (_, mut socket) in tabs {
        let (texts, code) = read_texts_until_close(&mut socket);
        assert_eq!(texts, vec![RETURN_TO_PAGE_JSON.to_owned()]);
        assert_eq!(code, REPLACED);
    }
    let report = env.server.shutdown();
    assert_eq!(
        report.detached_threads, 0,
        "ни один поток соединения не утёк"
    );
}

// ---- закрытие окна (активный экземпляр) -----------------------------------

#[test]
fn closing_the_active_session_with_a_farewell_writes_queue_then_farewell_then_4000() {
    let env = Env::start_with(parked_reader_policy());
    let opened = env.open("a");
    let active = session("a", opened.instance.0);
    let mut socket = attach(env.port(), "a", &opened.token);
    let_the_connection_thread_park();
    env.control
        .send_to_page(&PlayerSessionMessage::Pause {})
        .unwrap();
    assert_eq!(
        env.control.close_session_with_farewell(
            &active,
            &return_to_page(),
            SessionCloseReason::PlayerClosed
        ),
        Ok(ActiveFarewellOutcome::FarewellQueued)
    );
    let (texts, code) = read_texts_until_close(&mut socket);
    assert_eq!(
        texts,
        vec![
            r#"{"type":"pause"}"#.to_owned(),
            RETURN_TO_PAGE_JSON.to_owned()
        ]
    );
    assert_eq!(code, SessionCloseReason::PlayerClosed.close_code());
    env.wait_lifecycle(
        "конец сеанса — закрыт плеером",
        ended_by_player(active.clone(), SessionCloseReason::PlayerClosed),
    );
    assert!(env.bytes.received.get().ended.contains(&active));
    assert_eq!(
        env.control.send_to_page(&PlayerSessionMessage::Play {}),
        Err(SendToPageError::NoAttachedSession)
    );
    assert_eq!(
        env.control.close_session_with_farewell(
            &active,
            &return_to_page(),
            SessionCloseReason::PlayerClosed
        ),
        Ok(ActiveFarewellOutcome::NotActive),
        "повтор — экземпляр уже не активен"
    );
}

/// Прощание не боится полной очереди исходящих: у него своё место.
#[test]
fn farewell_is_accepted_even_when_the_outgoing_queue_is_full() {
    let policy = BrowserSessionPolicy {
        outgoing_queue_capacity: 2,
        ..parked_reader_policy()
    };
    let env = Env::start_with(policy);
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    let_the_connection_thread_park();
    for _ in 0..2 {
        env.control
            .send_to_page(&PlayerSessionMessage::Play {})
            .unwrap();
    }
    assert_eq!(
        env.control.send_to_page(&PlayerSessionMessage::Play {}),
        Err(SendToPageError::QueueFull)
    );
    assert_eq!(
        env.control.close_session_with_farewell(
            &session("a", opened.instance.0),
            &return_to_page(),
            SessionCloseReason::PlayerClosed
        ),
        Ok(ActiveFarewellOutcome::FarewellQueued)
    );
    let (texts, code) = read_texts_until_close(&mut socket);
    assert_eq!(texts.len(), 3);
    assert_eq!(texts.last().map(String::as_str), Some(RETURN_TO_PAGE_JSON));
    assert_eq!(code, SessionCloseReason::PlayerClosed.close_code());
}

#[test]
fn closing_a_stale_instance_with_a_farewell_changes_nothing() {
    let env = Env::start_with(policy_with_grace(Duration::from_millis(100)));
    let old = env.open("a");
    let mut old_socket = attach(env.port(), "a", &old.token);
    let new = env.open("b");
    let mut new_socket = attach(env.port(), "b", &new.token);
    assert_eq!(
        env.control.close_session_with_farewell(
            &session("a", old.instance.0),
            &return_to_page(),
            SessionCloseReason::PlayerClosed
        ),
        Ok(ActiveFarewellOutcome::NotActive)
    );
    // Новый сеанс подключён; старой вкладке прощание тоже не ушло (только 4001 по сроку).
    env.control
        .send_to_page(&PlayerSessionMessage::Play {})
        .unwrap();
    assert_eq!(read_text(&mut new_socket), r#"{"type":"play"}"#);
    let (texts, code) = read_texts_until_close(&mut old_socket);
    assert!(texts.is_empty());
    assert_eq!(code, REPLACED);
}

#[test]
fn active_session_without_a_connection_is_closed_without_a_farewell() {
    let env = Env::start();
    let opened = env.open("a");
    let active = session("a", opened.instance.0);
    assert_eq!(
        env.control.close_session_with_farewell(
            &active,
            &return_to_page(),
            SessionCloseReason::PlayerClosed
        ),
        Ok(ActiveFarewellOutcome::ClosedWithoutConnection)
    );
    env.wait_lifecycle(
        "сеанс без соединения закончен",
        ended_by_player(active, SessionCloseReason::PlayerClosed),
    );
}

/// Порядок окна 04b: прощание + 4000 → сразу выход процесса (сервер — 4006 всем).
/// Страница всё равно получает прощание и 4000.
#[test]
fn active_farewell_survives_an_immediate_shutdown() {
    let env = Env::start();
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    assert_eq!(
        env.control.close_session_with_farewell(
            &session("a", opened.instance.0),
            &return_to_page(),
            SessionCloseReason::PlayerClosed
        ),
        Ok(ActiveFarewellOutcome::FarewellQueued)
    );
    let report = env.server.shutdown();
    assert_eq!(report.detached_threads, 0);
    let (texts, code) = read_texts_until_close(&mut socket);
    assert_eq!(texts, vec![RETURN_TO_PAGE_JSON.to_owned()]);
    assert_eq!(code, SessionCloseReason::PlayerClosed.close_code());
}
