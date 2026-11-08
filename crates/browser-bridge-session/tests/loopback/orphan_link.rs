//! «Сирота» (протокол 1.4, сессия 11b1, решения 240–241): страница ушла, связь держится.
//!
//! Что проверяется по настоящему loopback:
//! - `page_left` не кончает сеанс и не закрывает сокет: конца нет, плеер по-прежнему шлёт
//!   странице (часы — пульс сироты, `reopen_page`);
//! - `page_returned` — тот же сеанс снова принимает поток: новое поколение доходит до
//!   приёмника, а байты поколения, ушедшего со страницей, — нет;
//! - пока страницы нет, любые байты (MSE и `<video src>`) — нарушение 4003;
//! - `page_reopen_failed` и закрытие 4002 экстеншеном — обычный конец «страница закрыла»;
//! - при общей версии 1.3 `page_left` — нарушение, а не сирота.

use browser_bridge_protocol::session::player::{ClockSequence, ClockState};
use browser_bridge_protocol::{
    ByteOffset, DataFrameHeader, MediaTime, PageSessionMessage, PlaybackRate, PlayerSessionMessage,
    ProtocolVersion, RangeBytesHeader, RangeRequestId, SessionCloseReason, encode_data_frame,
};
use browser_bridge_session::{SessionEndReason, SessionLifecycleEvent};
use tungstenite::WebSocket;

use crate::support::{
    Env, attach, data_frame_for, read_close_code, read_text, round_trip_ping, send_binary,
    send_text, session,
};

type Socket = WebSocket<std::net::TcpStream>;

const PAGE_LEFT: &str = r#"{"type":"page_left","reason":"closed"}"#;
const PAGE_RETURNED: &str = r#"{"type":"page_returned"}"#;
const REOPEN_FAILED: &str = r#"{"type":"page_reopen_failed","reason":"not_the_same_video"}"#;

/// Рамка вида 1 поколения `generation` с порядковым номером `sequence`.
fn frame(generation: u32, sequence: u64) -> Vec<u8> {
    let mut frame = data_frame_for(generation, 0);
    frame[16..24].copy_from_slice(&sequence.to_le_bytes());
    frame
}

/// Объявляет поколение и его единственный SourceBuffer.
fn start_generation(socket: &mut Socket, generation: u32) {
    send_text(
        socket,
        &format!(r#"{{"type":"generation_started","generation":{generation}}}"#),
    );
    send_text(
        socket,
        &format!(
            r#"{{"type":"source_buffer_added","generation":{generation},"source_buffer":0,"mime_type":"video/mp4"}}"#
        ),
    );
}

/// Тик часов плеера (пульс сироты).
fn clock_tick(sequence: u32) -> PlayerSessionMessage {
    PlayerSessionMessage::Clock {
        seq: ClockSequence(sequence),
        position_us: MediaTime::from_micros(12_000_000).unwrap(),
        state: ClockState::Paused,
        rate: PlaybackRate::NORMAL,
    }
}

/// Сеанс «orphan» на версии этого crate: поколение 1 с одной принятой рамкой, затем
/// страница ушла (`page_left`), и сервер это сообщение уже обработал.
fn orphan_after_one_frame(env: &Env) -> Socket {
    let token = env.open("orphan").token;
    let mut socket = attach(env.port(), "orphan", &token);
    start_generation(&mut socket, 1);
    send_binary(&mut socket, frame(1, 0));
    send_text(&mut socket, PAGE_LEFT);
    round_trip_ping(&mut socket, b"page-left");
    socket
}

fn page_left_was_published(env: &Env) -> bool {
    env.events
        .seen
        .get()
        .page_messages
        .iter()
        .any(|message| matches!(message, PageSessionMessage::PageLeft { .. }))
}

fn has_ended(env: &Env) -> bool {
    env.events
        .seen
        .get()
        .lifecycle
        .iter()
        .any(|event| matches!(event, SessionLifecycleEvent::Ended { .. }))
}

#[test]
fn page_left_keeps_the_session_and_the_player_still_reaches_the_page() {
    let env = Env::start();
    let mut socket = orphan_after_one_frame(&env);

    // Сообщение дошло до приложения, а конца сеанса нет: приёмник байт не закрыт.
    assert!(page_left_was_published(&env));
    assert!(!has_ended(&env));
    assert!(env.bytes.received.get().ended.is_empty());

    // Пульс и просьба вернуть страницу доходят до экстеншена по той же связи.
    env.control.send_to_page(&clock_tick(7)).unwrap();
    let reopen = PlayerSessionMessage::ReopenPage {
        position_us: MediaTime::from_micros(12_000_000).unwrap(),
    };
    env.control.send_to_page(&reopen).unwrap();
    assert_eq!(
        read_text(&mut socket),
        r#"{"type":"clock","seq":7,"position_us":12000000,"state":"paused","rate":1000}"#
    );
    assert_eq!(
        read_text(&mut socket),
        r#"{"type":"reopen_page","position_us":12000000}"#
    );
}

#[test]
fn page_returned_lets_a_new_generation_in_and_drops_the_one_that_left() {
    let env = Env::start();
    let mut socket = orphan_after_one_frame(&env);
    send_text(&mut socket, PAGE_RETURNED);
    // Байты ушедшего поколения после возврата — устаревшие: молча выброшены.
    send_binary(&mut socket, frame(1, 1));
    start_generation(&mut socket, 2);
    send_binary(&mut socket, frame(2, 0));
    round_trip_ping(&mut socket, b"returned");

    let received = env.bytes.received.get();
    assert_eq!(
        received.frames, 2,
        "поколение 1 до ухода и поколение 2 после"
    );
    assert_eq!(received.source_buffers[&(1, 0)].len(), 1024);
    assert_eq!(received.source_buffers[&(2, 0)].len(), 1024);
    assert!(!has_ended(&env));
    assert!(
        env.events
            .seen
            .get()
            .page_messages
            .contains(&PageSessionMessage::PageReturned {})
    );
}

#[test]
fn bytes_while_the_page_is_away_close_the_session_with_4003() {
    let env = Env::start();
    let mut socket = orphan_after_one_frame(&env);
    send_binary(&mut socket, frame(1, 1));
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    env.wait_lifecycle("Ended(4003) в сироте", |event| {
        matches!(
            event,
            SessionLifecycleEvent::Ended {
                reason: SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation),
                ..
            }
        )
    });
    // Рамка не дошла до приёмника: в нём только рамка до ухода страницы.
    assert_eq!(env.bytes.received.get().frames, 1);
}

#[test]
fn file_bytes_while_the_page_is_away_also_close_the_session_with_4003() {
    let env = Env::start();
    let mut socket = orphan_after_one_frame(&env);
    let range_frame = encode_data_frame(
        &DataFrameHeader::RangeBytes(RangeBytesHeader {
            request_id: RangeRequestId(1),
            offset: ByteOffset::new(0).unwrap(),
        }),
        b"file bytes",
    )
    .unwrap();
    send_binary(&mut socket, range_frame);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    assert!(env.bytes.received.get().ranges.is_empty());
}

#[test]
fn reopen_failure_then_close_4002_ends_the_session_as_closed_by_the_page() {
    let env = Env::start();
    let mut socket = orphan_after_one_frame(&env);
    send_text(&mut socket, REOPEN_FAILED);
    round_trip_ping(&mut socket, b"refused");
    assert!(
        env.events
            .seen
            .get()
            .page_messages
            .iter()
            .any(|message| matches!(message, PageSessionMessage::PageReopenFailed { .. }))
    );
    assert!(
        !has_ended(&env),
        "отказ сам сеанс не кончает: кончает закрытие"
    );
    socket
        .close(Some(tungstenite::protocol::CloseFrame {
            code: SessionCloseReason::TabGone.close_code().into(),
            reason: "".into(),
        }))
        .unwrap();
    env.wait_lifecycle("Ended(ClosedByPage 4002)", |event| {
        event
            == &SessionLifecycleEvent::Ended {
                session: session("orphan", 1),
                reason: SessionEndReason::ClosedByPage { code: Some(4002) },
            }
    });
}

#[test]
fn page_left_at_agreed_1_3_is_a_violation_not_an_orphan() {
    let env = Env::start();
    let token = env.open_agreed("old", ProtocolVersion::V1_3).token;
    let mut socket = attach(env.port(), "old", &token);
    send_text(&mut socket, PAGE_LEFT);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    assert!(!page_left_was_published(&env));
}
