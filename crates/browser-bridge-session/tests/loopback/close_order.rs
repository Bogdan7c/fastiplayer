//! Порядок «очередь исходящих → кадр закрытия» для любого закрытия плеером
//! (решение 127, общая правка 04b).
//!
//! Каждый тест ставит сообщения в очередь и тут же просит закрыть, пока поток
//! соединения спит в чтении (`parked_reader_policy`). До 04b поток проверял
//! просьбу закрыть РАНЬШЕ сброса очереди, и страница получала только код:
//! эти тесты на старом коде падают стабильно.

use browser_bridge_protocol::{PlayerSessionMessage, SessionCloseReason};
use browser_bridge_session::{CloseOutcome, SessionEndReason, SessionLifecycleEvent};

use crate::support::{
    Env, attach, let_the_connection_thread_park, parked_reader_policy, read_texts_until_close,
    session,
};

/// Три команды плеера и их запись на проводе — в порядке постановки.
const QUEUED: [(PlayerSessionMessage, &str); 3] = [
    (PlayerSessionMessage::Play {}, r#"{"type":"play"}"#),
    (PlayerSessionMessage::Pause {}, r#"{"type":"pause"}"#),
    (
        PlayerSessionMessage::NextEpisode {},
        r#"{"type":"next_episode"}"#,
    ),
];

fn queue_three_messages(env: &Env) {
    for (message, _) in &QUEUED {
        env.control.send_to_page(message).unwrap();
    }
}

fn expected_texts() -> Vec<String> {
    QUEUED.iter().map(|(_, json)| (*json).to_owned()).collect()
}

#[test]
fn close_session_writes_every_queued_message_before_the_close_frame() {
    let env = Env::start_with(parked_reader_policy());
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    let_the_connection_thread_park();
    queue_three_messages(&env);
    assert_eq!(
        env.control.close_session(
            &session("a", opened.instance.0),
            SessionCloseReason::PlayerClosed
        ),
        CloseOutcome::Closed
    );
    let (texts, code) = read_texts_until_close(&mut socket);
    assert_eq!(
        texts,
        expected_texts(),
        "очередь дописана целиком и по порядку"
    );
    assert_eq!(code, SessionCloseReason::PlayerClosed.close_code());
    env.wait_lifecycle("конец сеанса плеером", |event| {
        event
            == &SessionLifecycleEvent::Ended {
                session: session("a", opened.instance.0),
                reason: SessionEndReason::ClosedByPlayer(SessionCloseReason::PlayerClosed),
            }
    });
}

#[test]
fn close_active_session_writes_every_queued_message_before_the_close_frame() {
    let env = Env::start_with(parked_reader_policy());
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    let_the_connection_thread_park();
    queue_three_messages(&env);
    env.control
        .close_active_session(SessionCloseReason::PlayerClosed);
    let (texts, code) = read_texts_until_close(&mut socket);
    assert_eq!(texts, expected_texts());
    assert_eq!(code, SessionCloseReason::PlayerClosed.close_code());
}

#[test]
fn server_shutdown_writes_every_queued_message_before_4006() {
    let env = Env::start_with(parked_reader_policy());
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    let_the_connection_thread_park();
    queue_three_messages(&env);
    let report = env.server.shutdown();
    assert_eq!(report.detached_threads, 0);
    let (texts, code) = read_texts_until_close(&mut socket);
    assert_eq!(texts, expected_texts());
    assert_eq!(code, SessionCloseReason::PlayerShuttingDown.close_code());
}

/// Нарушение протокола страницей закрывает соединение кодом 4003 тем же путём:
/// то, что плеер успел поставить в очередь до нарушения, тоже уходит первым.
#[test]
fn protocol_violation_writes_the_queued_messages_before_4003() {
    let env = Env::start_with(parked_reader_policy());
    let opened = env.open("a");
    let mut socket = attach(env.port(), "a", &opened.token);
    let_the_connection_thread_park();
    queue_three_messages(&env);
    // Мусор вместо JSON: поток проснётся на нём, раньше чем дойдёт до сброса очереди.
    crate::support::send_text(&mut socket, "not json");
    let (texts, code) = read_texts_until_close(&mut socket);
    assert_eq!(texts, expected_texts());
    assert_eq!(code, SessionCloseReason::ProtocolViolation.close_code());
}
