//! Общая версия протокола доходит от `hello` до сторожа соединения (сессия 10a1).
//!
//! Путь целиком, как вживую: «стойка входа» (`EntryDesk`) принимает `hello` и
//! `open_session` → реестр запоминает версию → соединение после `attach` создаёт
//! сторожа поколений с этой версией. При общей 1.0 сообщения протокола 1.1
//! (`hls_source_added`, `resource_fetch_finished`) закрывают сеанс кодом 4003 и не
//! доходят до приложения; при 1.1 — принимаются.

use std::time::Instant;

use browser_bridge_protocol::{
    ByteOffset, CURRENT_PROTOCOL_VERSION, CapabilityToken, DataFrameHeader,
    ExtensionControlMessage, Hello, PageSessionMessage, PlayerControlMessage, ProtocolVersion,
    RangeBytesHeader, RangeRequestId, SessionCloseReason, SessionGrant, SoftwareVersion,
    decode_message, encode_data_frame, encode_message,
};
use browser_bridge_session::{
    EntryCaller, EntryDesk, SessionEndReason, SessionLifecycleEvent, SessionRef,
};
use tungstenite::WebSocket;

use crate::support::{
    Env, attach, read_close_code, round_trip_ping, send_binary, send_text, test_policy,
};

type Socket = WebSocket<std::net::TcpStream>;

const HELLO_EXTENSION_1_0: &str =
    include_str!("../../../browser-bridge-protocol/fixtures/control/valid/hello-extension.json");
const OPEN_WINDOW: &str = include_str!(
    "../../../browser-bridge-protocol/fixtures/control/valid/open-session-window.json"
);
/// Ключ сеанса из фикстуры `open-session-window`.
const FIXTURE_SESSION_KEY: &str = "tab-12-video-3";

const GENERATION_1: &str = r#"{"type":"generation_started","generation":1}"#;
const HLS_SOURCE_1: &str = r#"{"type":"hls_source_added","generation":1,"root":0}"#;
const FILE_SOURCE_1: &str = r#"{"type":"file_source_added","generation":1}"#;
const RESOURCE_COMPLETED_1: &str = r#"{"type":"resource_fetch_finished","request_id":1,"outcome":{"kind":"completed","size_bytes":8}}"#;
const PLAYLIST_START: &[u8] = b"#EXTM3U\n";

/// `hello` экстеншена на версии `agreed` (фикстура с подменой `minor`).
fn extension_hello(version: ProtocolVersion) -> String {
    let ExtensionControlMessage::Hello(mut hello) =
        decode_message(HELLO_EXTENSION_1_0.as_bytes()).unwrap()
    else {
        panic!("фикстура должна быть hello");
    };
    hello.protocol = version;
    encode_message(&ExtensionControlMessage::Hello(hello)).unwrap()
}

/// Стойка входа на сервере теста: плеер говорит на версии этого crate.
fn desk_of(env: &Env) -> EntryDesk {
    let player_hello = Hello {
        protocol: CURRENT_PROTOCOL_VERSION,
        software_version: SoftwareVersion::new("0.2.0").unwrap(),
        capabilities: Vec::<CapabilityToken>::new(),
    };
    EntryDesk::new(player_hello, env.control.clone(), &test_policy())
}

/// Звонок `hello` → ответ стойки (общая версия по её словам).
fn say_hello(
    desk: &mut EntryDesk,
    caller: &EntryCaller,
    version: ProtocolVersion,
) -> ProtocolVersion {
    let outcome = desk.handle(caller, extension_hello(version).as_bytes(), Instant::now());
    match decode_message::<PlayerControlMessage>(outcome.reply_json.as_bytes()).unwrap() {
        PlayerControlMessage::HelloAccepted(accepted) => accepted.agreed_protocol,
        other => panic!("ожидали hello_accepted: {other:?}"),
    }
}

/// Звонок `open_session` → пропуск.
fn open_window(desk: &mut EntryDesk, caller: &EntryCaller) -> SessionGrant {
    let outcome = desk.handle(caller, OPEN_WINDOW.as_bytes(), Instant::now());
    match decode_message::<PlayerControlMessage>(outcome.reply_json.as_bytes()).unwrap() {
        PlayerControlMessage::SessionGranted(grant) => grant,
        other => panic!("ожидали session_granted: {other:?}"),
    }
}

/// Весь вход как у экстеншена: `hello` версии `version`, `open_session`, `attach`.
fn attached_after_hello(env: &Env, version: ProtocolVersion) -> Socket {
    let mut desk = desk_of(env);
    let caller = EntryCaller::new(":1.42").unwrap();
    say_hello(&mut desk, &caller, version);
    let grant = open_window(&mut desk, &caller);
    attach(env.port(), FIXTURE_SESSION_KEY, &grant.token)
}

fn session_ref() -> SessionRef {
    crate::support::session(FIXTURE_SESSION_KEY, 1)
}

/// Ждёт событие `Attached` первого экземпляра и возвращает его версию.
fn attached_version(env: &Env) -> ProtocolVersion {
    let seen = env.events.seen.wait_for("Attached", |seen| {
        seen.lifecycle
            .iter()
            .any(|event| matches!(event, SessionLifecycleEvent::Attached { .. }))
    });
    seen.lifecycle
        .iter()
        .find_map(|event| match event {
            SessionLifecycleEvent::Attached {
                session,
                agreed_protocol,
            } if *session == session_ref() => Some(*agreed_protocol),
            _ => None,
        })
        .unwrap()
}

/// Ждёт конец сеанса нарушением протокола.
fn wait_for_violation_end(env: &Env) {
    env.wait_lifecycle("Ended(4003)", |event| {
        matches!(
            event,
            SessionLifecycleEvent::Ended {
                reason: SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation),
                ..
            }
        )
    });
}

/// Типы сообщений страницы, которые дошли до приложения.
fn delivered_types(env: &Env) -> Vec<&'static str> {
    env.events
        .seen
        .get()
        .page_messages
        .iter()
        .map(|message| match message {
            PageSessionMessage::GenerationStarted { .. } => "generation_started",
            PageSessionMessage::HlsSourceAdded { .. } => "hls_source_added",
            PageSessionMessage::FileSourceAdded { .. } => "file_source_added",
            PageSessionMessage::ResourceFetchFinished { .. } => "resource_fetch_finished",
            _ => "other",
        })
        .collect()
}

/// Рамка вида 2 — первый кусок тела ответа на запрос 1 (смещение внутри тела с 0).
fn body_frame_of_request_1() -> Vec<u8> {
    encode_data_frame(
        &DataFrameHeader::RangeBytes(RangeBytesHeader {
            request_id: RangeRequestId(1),
            offset: ByteOffset::new(0).unwrap(),
        }),
        PLAYLIST_START,
    )
    .unwrap()
}

#[test]
fn after_hello_1_0_hls_source_added_closes_the_session_with_4003_and_never_reaches_the_app() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_0);
    assert_eq!(attached_version(&env), ProtocolVersion::V1_0);
    send_text(&mut socket, GENERATION_1);
    send_text(&mut socket, HLS_SOURCE_1);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    wait_for_violation_end(&env);
    // До нарушения всё шло как обычно; само HLS-сообщение приложению не отдано.
    assert_eq!(delivered_types(&env), ["generation_started"]);
}

#[test]
fn after_hello_1_0_resource_fetch_finished_is_a_violation_too() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_0);
    send_text(&mut socket, RESOURCE_COMPLETED_1);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    wait_for_violation_end(&env);
    assert!(delivered_types(&env).is_empty());
}

#[test]
fn after_hello_1_1_hls_generation_and_resource_bodies_reach_the_app_in_wire_order() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_1);
    assert_eq!(attached_version(&env), ProtocolVersion::V1_1);
    send_text(&mut socket, GENERATION_1);
    send_text(&mut socket, HLS_SOURCE_1);
    send_binary(&mut socket, body_frame_of_request_1());
    send_text(&mut socket, RESOURCE_COMPLETED_1);
    // Сервер всё прочитал и соединение живо: pong пришёл, а не кадр закрытия.
    round_trip_ping(&mut socket, b"hls-accepted");
    let seen = env.events.seen.wait_for("три сообщения", |seen| {
        seen.page_messages.len() == 3
    });
    assert_eq!(seen.page_messages.len(), 3);
    assert_eq!(
        delivered_types(&env),
        [
            "generation_started",
            "hls_source_added",
            "resource_fetch_finished"
        ]
    );
    // Тело ответа дошло до приёмника байтов рамкой вида 2 по номеру запроса.
    let received = env.bytes.received.get();
    assert_eq!(
        received.ranges.get(&1).map(Vec::as_slice),
        Some(PLAYLIST_START)
    );
    assert!(received.source_buffers.is_empty());
}

#[test]
fn on_the_wire_an_hls_generation_refuses_source_buffers() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_1);
    send_text(&mut socket, GENERATION_1);
    send_text(&mut socket, HLS_SOURCE_1);
    send_text(
        &mut socket,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4"}"#,
    );
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    wait_for_violation_end(&env);
    assert_eq!(
        delivered_types(&env),
        ["generation_started", "hls_source_added"]
    );
}

#[test]
fn after_hello_1_0_the_file_source_path_works_as_before() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_0);
    send_text(&mut socket, GENERATION_1);
    send_text(&mut socket, FILE_SOURCE_1);
    send_binary(&mut socket, body_frame_of_request_1());
    round_trip_ping(&mut socket, b"file-accepted");
    env.events.seen.wait_for("два сообщения", |seen| {
        seen.page_messages.len() == 2
    });
    assert_eq!(
        delivered_types(&env),
        ["generation_started", "file_source_added"]
    );
    assert_eq!(
        env.bytes.received.get().ranges.get(&1).map(Vec::as_slice),
        Some(PLAYLIST_START)
    );
}

#[test]
fn the_last_hello_of_the_caller_decides_the_version_of_its_next_session() {
    let env = Env::start();
    let mut desk = desk_of(&env);
    let caller = EntryCaller::new(":1.43").unwrap();
    assert_eq!(
        say_hello(&mut desk, &caller, ProtocolVersion::V1_1),
        ProtocolVersion::V1_1
    );
    // Экстеншен откатили на старую версию и он поздоровался заново.
    assert_eq!(
        say_hello(&mut desk, &caller, ProtocolVersion::V1_0),
        ProtocolVersion::V1_0
    );
    let grant = open_window(&mut desk, &caller);
    let mut socket = attach(env.port(), FIXTURE_SESSION_KEY, &grant.token);
    assert_eq!(attached_version(&env), ProtocolVersion::V1_0);
    send_text(&mut socket, GENERATION_1);
    send_text(&mut socket, HLS_SOURCE_1);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
}

#[test]
fn a_newer_extension_minor_is_held_to_the_players_version() {
    // Экстеншен из будущего (1.7) с этим плеером говорит на 1.1: HLS принимается.
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion { major: 1, minor: 7 });
    assert_eq!(attached_version(&env), CURRENT_PROTOCOL_VERSION);
    send_text(&mut socket, GENERATION_1);
    send_text(&mut socket, HLS_SOURCE_1);
    round_trip_ping(&mut socket, b"future-extension");
    env.events.seen.wait_for("два сообщения", |seen| {
        seen.page_messages.len() == 2
    });
    assert_eq!(
        delivered_types(&env),
        ["generation_started", "hls_source_added"]
    );
}
