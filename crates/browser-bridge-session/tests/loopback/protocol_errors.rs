//! Отказы сервера: каждый код закрытия и то, что отказ не портит чужое состояние.

use std::time::{Duration, Instant};

use browser_bridge_protocol::{
    AppendSequence, DataFrameHeader, SessionCloseReason, SessionToken, SourceBufferBytesHeader,
    SourceBufferIndex, StreamGeneration, encode_data_frame,
};
use browser_bridge_session::{BrowserSessionPolicy, SendToPageError};

use crate::support::{
    Env, attach, attach_json, connect, read_close_code, read_text, send_binary, send_text,
    test_policy, try_connect,
};

fn code(reason: SessionCloseReason) -> u16 {
    reason.close_code()
}

fn data_frame(generation: u32, source_buffer: u16, sequence: u64, payload: &[u8]) -> Vec<u8> {
    encode_data_frame(
        &DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
            generation: StreamGeneration::new(generation).unwrap(),
            source_buffer: SourceBufferIndex::new(source_buffer).unwrap(),
            sequence: AppendSequence(sequence),
        }),
        payload,
    )
    .unwrap()
}

#[test]
fn wrong_token_is_closed_with_4004_and_the_real_token_still_works_afterwards() {
    let env = Env::start();
    let opened = env.open("a");
    let mut intruder = connect(env.port());
    let wrong = SessionToken::new("x".repeat(32)).unwrap();
    send_text(&mut intruder, &attach_json("a", &wrong));
    assert_eq!(
        read_close_code(&mut intruder),
        code(SessionCloseReason::InvalidToken)
    );
    // Отказ не испортил сеанс: настоящий пропуск подходит.
    attach(env.port(), "a", &opened.token);
}

#[test]
fn unknown_session_key_is_closed_with_4005() {
    let env = Env::start();
    let opened = env.open("a");
    let mut socket = connect(env.port());
    send_text(&mut socket, &attach_json("no-such-session", &opened.token));
    assert_eq!(
        read_close_code(&mut socket),
        code(SessionCloseReason::UnknownSession)
    );
}

#[test]
fn token_is_one_time_a_replayed_attach_gets_4004() {
    let env = Env::start();
    let opened = env.open("a");
    let _first = attach(env.port(), "a", &opened.token);
    let mut replay = connect(env.port());
    send_text(&mut replay, &attach_json("a", &opened.token));
    assert_eq!(
        read_close_code(&mut replay),
        code(SessionCloseReason::InvalidToken)
    );
}

#[test]
fn silent_connection_is_closed_with_4007_after_the_attach_timeout() {
    let policy = BrowserSessionPolicy {
        attach_timeout: Duration::from_millis(150),
        ..test_policy()
    };
    let env = Env::start_with(policy);
    let mut socket = connect(env.port());
    assert_eq!(
        read_close_code(&mut socket),
        code(SessionCloseReason::AttachTimeout)
    );
}

#[test]
fn second_connection_with_the_next_token_gets_4008_while_the_first_stays_alive() {
    let env = Env::start();
    let first_token = env.open("a").token;
    let mut first = attach(env.port(), "a", &first_token);
    // Повторный open_session при живом соединении выдаёт следующий пропуск.
    let next_token = env.open("a").token;
    let mut second = connect(env.port());
    send_text(&mut second, &attach_json("a", &next_token));
    assert_eq!(
        read_close_code(&mut second),
        code(SessionCloseReason::AlreadyAttached)
    );
    // Первое соединение живо: по нему по-прежнему идут сообщения плеера.
    env.control
        .send_to_page(&browser_bridge_protocol::PlayerSessionMessage::Play {})
        .unwrap();
    assert_eq!(read_text(&mut first), r#"{"type":"play"}"#);
}

#[test]
fn first_message_binary_is_closed_with_4003() {
    let env = Env::start();
    env.open("a");
    let mut socket = connect(env.port());
    send_binary(&mut socket, data_frame(1, 0, 0, b"x"));
    assert_eq!(
        read_close_code(&mut socket),
        code(SessionCloseReason::ProtocolViolation)
    );
}

#[test]
fn first_message_garbage_or_not_attach_is_closed_with_4003() {
    let env = Env::start();
    env.open("a");
    for first_message in [
        "это не JSON",
        r#"{"type":"generation_started","generation":1}"#,
    ] {
        let mut socket = connect(env.port());
        send_text(&mut socket, first_message);
        assert_eq!(
            read_close_code(&mut socket),
            code(SessionCloseReason::ProtocolViolation),
            "{first_message}"
        );
    }
}

/// Что делает нарушитель после успешного attach.
type Misbehavior = Box<dyn Fn(&mut tungstenite::WebSocket<std::net::TcpStream>, &SessionToken)>;

#[test]
fn garbage_after_attach_second_attach_and_sequence_gap_close_with_4003_and_end_the_session() {
    let cases: [(&str, Misbehavior); 3] = [
        ("мусор", Box::new(|socket, _| send_text(socket, "{мусор"))),
        (
            "вторая attach",
            Box::new(|socket, token| send_text(socket, &attach_json("a", token))),
        ),
        (
            "пропуск номера рамки",
            Box::new(|socket, _| {
                send_text(socket, r#"{"type":"generation_started","generation":1}"#);
                send_text(
                    socket,
                    r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4"}"#,
                );
                send_binary(socket, data_frame(1, 0, 5, b"gap"));
            }),
        ),
    ];
    for (name, misbehave) in cases {
        let env = Env::start();
        let token = env.open("a").token;
        let mut socket = attach(env.port(), "a", &token);
        misbehave(&mut socket, &token);
        assert_eq!(
            read_close_code(&mut socket),
            code(SessionCloseReason::ProtocolViolation),
            "{name}"
        );
        env.wait_lifecycle(name, |event| {
            matches!(
                event,
                browser_bridge_session::SessionLifecycleEvent::Ended {
                    reason: browser_bridge_session::SessionEndReason::ClosedByPlayer(
                        SessionCloseReason::ProtocolViolation
                    ),
                    ..
                }
            )
        });
        assert!(
            env.bytes.received.get().source_buffers.is_empty(),
            "{name}: рамка с дырой не принята"
        );
    }
}

#[test]
fn oversized_binary_message_is_closed_with_4003() {
    let env = Env::start();
    let token = env.open("a").token;
    let mut socket = attach(env.port(), "a", &token);
    // Больше заголовка + 1 МиБ груза: предел размера сообщения WebSocket.
    send_binary(&mut socket, vec![1_u8; 2 * 1024 * 1024]);
    assert_eq!(
        read_close_code(&mut socket),
        code(SessionCloseReason::ProtocolViolation)
    );
}

#[test]
fn tab_gone_is_forwarded_then_closed_with_4002() {
    let env = Env::start();
    let token = env.open("a").token;
    let mut socket = attach(env.port(), "a", &token);
    send_text(
        &mut socket,
        r#"{"type":"tab_gone","reason":"navigated_away"}"#,
    );
    assert_eq!(
        read_close_code(&mut socket),
        code(SessionCloseReason::TabGone)
    );
    let seen = env.events.seen.get();
    assert!(
        seen.page_messages.iter().any(|message| matches!(
            message,
            browser_bridge_protocol::PageSessionMessage::TabGone { .. }
        )),
        "приёмник должен узнать причину ухода вкладки"
    );
}

#[test]
fn another_key_replaces_the_session_old_connection_gets_4001_new_one_attaches() {
    let env = Env::start();
    let old_token = env.open("old").token;
    let mut old = attach(env.port(), "old", &old_token);
    let replacing = env.open("new");
    assert_eq!(replacing.replaced, Some(crate::support::session("old", 1)));
    assert_eq!(
        read_close_code(&mut old),
        code(SessionCloseReason::ReplacedByAnotherTab)
    );
    let _new = attach(env.port(), "new", &replacing.token);
    // Приёмнику байт сообщили, что старый сеанс кончился.
    env.bytes
        .received
        .wait_for("session_ended старого", |received| {
            received.ended.contains(&crate::support::session("old", 1))
        });
}

#[test]
fn shutdown_closes_attached_and_unattached_connections_with_4006_promptly() {
    let env = Env::start();
    let token = env.open("a").token;
    let mut attached = attach(env.port(), "a", &token);
    let mut waiting = connect(env.port());
    let started = Instant::now();
    let report = env.server.shutdown();
    assert!(started.elapsed() < Duration::from_secs(5), "shutdown завис");
    assert_eq!(report.detached_threads, 0);
    assert_eq!(
        read_close_code(&mut attached),
        code(SessionCloseReason::PlayerShuttingDown)
    );
    assert_eq!(
        read_close_code(&mut waiting),
        code(SessionCloseReason::PlayerShuttingDown)
    );
}

#[test]
fn shutdown_is_idempotent_through_drop_and_refuses_new_work_afterwards() {
    let env = Env::start();
    let control = env.control.clone();
    let port = env.port();
    drop(env);
    let refused = control.open_session(&crate::support::open_request("late"), Instant::now());
    assert!(matches!(
        refused,
        Err(browser_bridge_session::OpenSessionError::ShuttingDown)
    ));
    assert!(try_connect(port).is_err(), "порт после остановки закрыт");
}

#[test]
fn silent_connections_cannot_lock_out_the_real_client_the_oldest_is_evicted_with_4007() {
    let policy = BrowserSessionPolicy {
        max_unattached_connections: 2,
        ..test_policy()
    };
    let env = Env::start_with(policy);
    let token = env.open("a").token;
    // Два залипших локальных подключения занимают все места без attach.
    let mut oldest_silent = connect(env.port());
    let mut newer_silent = connect(env.port());
    // Настоящий экстеншен приходит третьим и всё равно проходит attach.
    let mut real = attach(env.port(), "a", &token);
    // Вытеснен самый старый молчун: он получает 4007, как не приславший attach вовремя.
    assert_eq!(
        read_close_code(&mut oldest_silent),
        code(SessionCloseReason::AttachTimeout)
    );
    // Новее молчун не тронут: по нему до сих пор можно пройти attach (пропуск уже
    // использован, поэтому честный отказ 4004, а не обрыв по вытеснению).
    send_text(&mut newer_silent, &attach_json("a", &token));
    assert_eq!(
        read_close_code(&mut newer_silent),
        code(SessionCloseReason::InvalidToken)
    );
    // Настоящее соединение живо.
    env.control
        .send_to_page(&browser_bridge_protocol::PlayerSessionMessage::Play {})
        .unwrap();
    assert_eq!(read_text(&mut real), r#"{"type":"play"}"#);
}

#[test]
fn oversized_text_before_attach_is_closed_with_4003() {
    // До attach действует малый предел сообщения (8 КиБ). Закрываем именно 4003
    // (нарушение протокола — наш код приложения), а не 1009 из RFC: клиенту
    // протокола нужна одна таблица кодов 4000–4008, без особого случая.
    let env = Env::start();
    env.open("a");
    let mut socket = connect(env.port());
    let padding = " ".repeat(64 * 1024);
    send_text(&mut socket, &format!(r#"{{"type":"attach"{padding}}}"#));
    assert_eq!(
        read_close_code(&mut socket),
        code(SessionCloseReason::ProtocolViolation)
    );
}

#[test]
fn large_frames_are_accepted_after_attach_limit_is_raised() {
    let env = Env::start();
    let token = env.open("a").token;
    let mut socket = attach(env.port(), "a", &token);
    send_text(
        &mut socket,
        r#"{"type":"generation_started","generation":1}"#,
    );
    send_text(
        &mut socket,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4"}"#,
    );
    let payload = vec![7_u8; 512 * 1024];
    send_binary(&mut socket, data_frame(1, 0, 0, &payload));
    let received = env
        .bytes
        .received
        .wait_for("мегабайтный кусок", |received| {
            received.frames >= 1
        });
    assert_eq!(received.source_buffers[&(1, 0)], payload);
}

#[test]
fn send_to_page_without_an_attached_session_reports_it() {
    let env = Env::start();
    let message = browser_bridge_protocol::PlayerSessionMessage::Pause {};
    assert_eq!(
        env.control.send_to_page(&message),
        Err(SendToPageError::NoAttachedSession)
    );
    env.open("a");
    assert_eq!(
        env.control.send_to_page(&message),
        Err(SendToPageError::NoAttachedSession),
        "сеанс выдан, но ещё не подключён"
    );
}

#[test]
fn page_messages_are_counted_in_the_report_when_the_event_sink_is_full() {
    let env = Env::start_full(test_policy(), true);
    let token = env.open("a").token;
    let mut socket = attach(env.port(), "a", &token);
    send_text(
        &mut socket,
        r#"{"type":"generation_started","generation":1}"#,
    );
    send_text(&mut socket, r#"{"type":"tab_gone","reason":"closed"}"#);
    read_close_code(&mut socket);
    let report = env.server.shutdown();
    assert_eq!(
        report.dropped_page_messages, 2,
        "generation_started и tab_gone"
    );
}
