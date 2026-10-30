//! События жизни сеанса: обрыв, возврат, сроки, закрытие страницей и плеером.

use std::net::Shutdown;
use std::time::{Duration, Instant};

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_session::{BrowserSessionPolicy, SessionEndReason, SessionLifecycleEvent};
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;

use crate::support::{Env, attach, read_close_code, test_policy};

fn ended(text: &str, reason: SessionEndReason) -> impl Fn(&SessionLifecycleEvent) -> bool {
    let session = crate::support::session(text, 1);
    move |event| {
        event
            == &SessionLifecycleEvent::Ended {
                session: session.clone(),
                reason,
            }
    }
}

#[test]
fn attach_publishes_an_attached_event() {
    let env = Env::start();
    let token = env.open("a").token;
    let _socket = attach(env.port(), "a", &token);
    env.wait_lifecycle("Attached", |event| {
        event
            == &SessionLifecycleEvent::Attached {
                session: crate::support::session("a", 1),
            }
    });
}

#[test]
fn dropped_connection_without_close_frame_enters_grace_and_the_same_key_can_return() {
    let env = Env::start();
    let token = env.open("a").token;
    let socket = attach(env.port(), "a", &token);
    // Обрыв «как у умершего service worker»: TCP закрыт без кадра закрытия.
    socket.get_ref().shutdown(Shutdown::Both).unwrap();
    drop(socket);
    env.wait_lifecycle("ConnectionLost", |event| {
        event
            == &SessionLifecycleEvent::ConnectionLost {
                session: crate::support::session("a", 1),
            }
    });
    // Повторный open_session с тем же ключом даёт новый пропуск, сеанс не пересоздаётся.
    let reopened = env.open("a");
    assert_eq!(reopened.replaced, None);
    let _back = attach(env.port(), "a", &reopened.token);
    let seen = env.events.seen.get().lifecycle;
    let attached_count = seen
        .iter()
        .filter(|event| matches!(event, SessionLifecycleEvent::Attached { .. }))
        .count();
    assert_eq!(
        attached_count, 2,
        "подключались дважды: до обрыва и после возврата"
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, SessionLifecycleEvent::Ended { .. })),
        "сеанс не заканчивался"
    );
}

#[test]
fn grace_expiry_ends_the_session_with_its_reason() {
    let policy = BrowserSessionPolicy {
        reconnect_grace: Duration::from_millis(100),
        ..test_policy()
    };
    let env = Env::start_with(policy);
    let token = env.open("a").token;
    let socket = attach(env.port(), "a", &token);
    socket.get_ref().shutdown(Shutdown::Both).unwrap();
    drop(socket);
    env.wait_lifecycle(
        "конец после грейса",
        ended("a", SessionEndReason::ReconnectGraceExpired),
    );
    env.bytes.received.wait_for("session_ended", |received| {
        received.ended.contains(&crate::support::session("a", 1))
    });
}

#[test]
fn session_nobody_attached_to_ends_at_the_first_attach_deadline() {
    let policy = BrowserSessionPolicy {
        first_attach_deadline: Duration::from_millis(100),
        ..test_policy()
    };
    let env = Env::start_with(policy);
    env.open("a");
    env.wait_lifecycle(
        "срок первого attach",
        ended("a", SessionEndReason::FirstAttachDeadlineExpired),
    );
}

#[test]
fn page_close_frame_ends_the_session_immediately_without_grace() {
    let env = Env::start();
    let token = env.open("a").token;
    let mut socket = attach(env.port(), "a", &token);
    socket
        .close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "".into(),
        }))
        .unwrap();
    env.wait_lifecycle(
        "закрыто страницей",
        ended("a", SessionEndReason::ClosedByPage { code: Some(1000) }),
    );
}

#[test]
fn player_closing_the_active_session_sends_4000_and_publishes_the_end() {
    let env = Env::start();
    let token = env.open("a").token;
    let mut socket = attach(env.port(), "a", &token);
    env.control
        .close_active_session(SessionCloseReason::PlayerClosed);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::PlayerClosed.close_code()
    );
    env.wait_lifecycle(
        "закрыто плеером",
        ended(
            "a",
            SessionEndReason::ClosedByPlayer(SessionCloseReason::PlayerClosed),
        ),
    );
    // Сеанса больше нет: пропуск сгорел вместе с ним.
    assert_eq!(
        env.control
            .send_to_page(&browser_bridge_protocol::PlayerSessionMessage::Play {}),
        Err(browser_bridge_session::SendToPageError::NoAttachedSession)
    );
}

#[test]
fn queued_player_messages_reach_the_page_in_order() {
    let env = Env::start();
    let token = env.open("a").token;
    let mut socket = attach(env.port(), "a", &token);
    let started = Instant::now();
    for message in [
        browser_bridge_protocol::PlayerSessionMessage::Play {},
        browser_bridge_protocol::PlayerSessionMessage::Pause {},
        browser_bridge_protocol::PlayerSessionMessage::NextEpisode {},
    ] {
        env.control.send_to_page(&message).unwrap();
    }
    assert_eq!(crate::support::read_text(&mut socket), r#"{"type":"play"}"#);
    assert_eq!(
        crate::support::read_text(&mut socket),
        r#"{"type":"pause"}"#
    );
    assert_eq!(
        crate::support::read_text(&mut socket),
        r#"{"type":"next_episode"}"#
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn reopening_a_key_after_grace_expiry_is_a_new_instance_and_old_end_does_not_match_it() {
    let policy = BrowserSessionPolicy {
        reconnect_grace: Duration::from_millis(100),
        ..test_policy()
    };
    let env = Env::start_with(policy);
    let first = env.open("a");
    let socket = attach(env.port(), "a", &first.token);
    socket.get_ref().shutdown(Shutdown::Both).unwrap();
    drop(socket);
    env.wait_lifecycle(
        "конец экземпляра 1",
        ended("a", SessionEndReason::ReconnectGraceExpired),
    );
    let second = env.open("a");
    assert_ne!(second.instance, first.instance);
    assert_eq!(second.instance.0, first.instance.0 + 1);
    let _again = attach(env.port(), "a", &second.token);
    env.wait_lifecycle("Attached нового экземпляра", |event| {
        event
            == &SessionLifecycleEvent::Attached {
                session: crate::support::session("a", 2),
            }
    });
    // Событие конца старого экземпляра не равно событию конца нового.
    assert_ne!(
        crate::support::session("a", 1),
        crate::support::session("a", second.instance.0)
    );
}

#[test]
fn nothing_reaches_the_byte_sink_after_session_ended_for_a_replaced_connection() {
    let env = Env::start();
    let token = env.open("old").token;
    let mut old = attach(env.port(), "old", &token);
    send_text_pair(&mut old);
    let flooder = std::thread::spawn(move || {
        let frame = crate::support::data_frame_for(1, 0);
        for sequence in 0..4000_u64 {
            let mut frame = frame.clone();
            frame[16..24].copy_from_slice(&sequence.to_le_bytes());
            if old.send(tungstenite::Message::binary(frame)).is_err() {
                break;
            }
        }
    });
    env.bytes.received.wait_for("поток байт пошёл", |received| {
        received.frames >= 3
    });
    env.open("new");
    flooder.join().unwrap();
    let received = env.bytes.received.get();
    assert!(received.ended.contains(&crate::support::session("old", 1)));
    assert_eq!(received.late_chunks, 0, "байты пришли после session_ended");
}

fn send_text_pair(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) {
    crate::support::send_text(socket, r#"{"type":"generation_started","generation":1}"#);
    crate::support::send_text(
        socket,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4"}"#,
    );
}

#[test]
fn closing_a_session_by_its_instance_sends_the_given_code_and_ends_it() {
    let env = Env::start();
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    let outcome = env.control.close_session(
        &crate::support::session("a", opened.instance.0),
        SessionCloseReason::ProtocolViolation,
    );
    assert_eq!(outcome, browser_bridge_session::CloseOutcome::Closed);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    env.wait_lifecycle(
        "закрыто плеером по экземпляру",
        ended(
            "a",
            SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation),
        ),
    );
    // Повтор про тот же экземпляр — честное «уже не активен».
    assert_eq!(
        env.control.close_session(
            &crate::support::session("a", opened.instance.0),
            SessionCloseReason::ProtocolViolation,
        ),
        browser_bridge_session::CloseOutcome::NotActive
    );
}

#[test]
fn closing_a_stale_instance_after_replacement_leaves_the_new_session_open() {
    let env = Env::start();
    let old = env.open("a");
    let _old_socket = attach(env.port(), "a", &old.token);
    // Другая вкладка вытеснила сеанс «a» (тот закрыт кодом 4001).
    let new = env.open("b");
    assert_ne!(new.instance, old.instance);
    let mut new_socket = attach(env.port(), "b", &new.token);
    env.wait_lifecycle("Attached нового сеанса", |event| {
        event
            == &SessionLifecycleEvent::Attached {
                session: crate::support::session("b", new.instance.0),
            }
    });
    // Опоздавшая просьба закрыть старый экземпляр нового не трогает.
    let outcome = env.control.close_session(
        &crate::support::session("a", old.instance.0),
        SessionCloseReason::ProtocolViolation,
    );
    assert_eq!(outcome, browser_bridge_session::CloseOutcome::NotActive);
    env.control
        .send_to_page(&browser_bridge_protocol::PlayerSessionMessage::Play {})
        .expect("новый сеанс по-прежнему подключён");
    assert_eq!(
        crate::support::read_text(&mut new_socket),
        r#"{"type":"play"}"#
    );
    let ended_new = env.events.seen.get().lifecycle.into_iter().any(|event| {
        matches!(event, SessionLifecycleEvent::Ended { session, .. }
            if session == crate::support::session("b", new.instance.0))
    });
    assert!(!ended_new, "новый сеанс не заканчивался");
}
