//! Договоры протокола 1.4 (сессия 11b1, решения 240–241 — «возврат вкладки»), которых не
//! покрывает общий проход по `manifest.json`:
//! - четыре новых сообщения: `page_left`, `page_returned`, `page_reopen_failed` (страница →
//!   плеер) и `reopen_page` (плеер → страница) — версия каждого в `message-versions.json`;
//! - версии значений `PageReopenFailure` — таблица `fixtures/value-versions.json`;
//! - при общей версии 1.3 и ниже новых сообщений нет ни у отправителя, ни у сторожа;
//! - каждое сообщение пишется ровно задокументированным JSON и читается обратно;
//! - живой сценарий: уход, `reopen_page`, возврат, новое поколение; и сценарий отказа.
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-4.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::session::{PageReopenFailure, StreamScope, TabGoneReason};
use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, GateDecision, MediaTime, MessageErrorKind, PageSessionMessage,
    PlayerSessionMessage, ProtocolVersion, SessionCloseReason, StreamGate, StreamGeneration,
    StreamViolation, decode_message, encode_message, negotiate_protocol_version,
};
use serde::Deserialize;

/// Все отказы возврата. Сопоставление ниже исчерпывающее: новое значение без записи здесь
/// не соберётся, а без записи в таблице фикстуры — не пройдёт тест.
const EVERY_REOPEN_FAILURE: [PageReopenFailure; 4] = [
    PageReopenFailure::NoBrowserWindow,
    PageReopenFailure::PageDidNotLoad,
    PageReopenFailure::NotTheSameVideo,
    PageReopenFailure::VideoDidNotStart,
];

/// Метка отказа на проводе.
const fn wire_label(failure: PageReopenFailure) -> &'static str {
    match failure {
        PageReopenFailure::NoBrowserWindow => "no_browser_window",
        PageReopenFailure::PageDidNotLoad => "page_did_not_load",
        PageReopenFailure::NotTheSameVideo => "not_the_same_video",
        PageReopenFailure::VideoDidNotStart => "video_did_not_start",
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

/// Запись таблицы значений, нужная этому тесту.
#[derive(Debug, Deserialize)]
struct ValueVersions {
    #[serde(rename = "page_reopen_failed.reason")]
    page_reopen_failed_reason: BTreeMap<String, String>,
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Версия из записи фикстуры `"1.4"`.
fn version_of(text: &str) -> ProtocolVersion {
    let (major, minor) = text.split_once('.').unwrap();
    ProtocolVersion {
        major: major.parse().unwrap(),
        minor: minor.parse().unwrap(),
    }
}

fn page_left(reason: TabGoneReason) -> PageSessionMessage {
    PageSessionMessage::PageLeft { reason }
}

fn reopen_failed(reason: PageReopenFailure) -> PageSessionMessage {
    PageSessionMessage::PageReopenFailed { reason }
}

fn reopen_page(micros: u64) -> PlayerSessionMessage {
    PlayerSessionMessage::ReopenPage {
        position_us: MediaTime::from_micros(micros).unwrap(),
    }
}

fn started(number: u32) -> PageSessionMessage {
    PageSessionMessage::GenerationStarted {
        generation: StreamGeneration::new(number).unwrap(),
    }
}

fn decode_page(json: &str) -> PageSessionMessage {
    decode_message::<PageSessionMessage>(json.as_bytes()).unwrap()
}

#[test]
fn protocol_1_4_adds_exactly_the_four_documented_message_types() {
    let table: MessageVersions = read_json("message-versions.json");
    let newer_page: BTreeSet<&str> = table
        .page_session
        .iter()
        .filter(|(_, version)| version.as_str() == "1.4")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        newer_page,
        BTreeSet::from(["page_left", "page_returned", "page_reopen_failed"])
    );
    let newer_player: BTreeSet<&str> = table
        .player_session
        .iter()
        .filter(|(_, version)| version.as_str() == "1.4")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(newer_player, BTreeSet::from(["reopen_page"]));
    // `tab_gone` не менялся.
    assert_eq!(table.page_session["tab_gone"], "1.0");
}

#[test]
fn every_message_knows_the_version_the_shared_table_lists() {
    let table: MessageVersions = read_json("message-versions.json");
    let samples = [
        (
            "page_left",
            page_left(TabGoneReason::Closed).introduced_in(),
        ),
        (
            "page_returned",
            PageSessionMessage::PageReturned {}.introduced_in(),
        ),
        (
            "page_reopen_failed",
            reopen_failed(PageReopenFailure::NoBrowserWindow).introduced_in(),
        ),
    ];
    for (name, introduced_in) in samples {
        assert_eq!(
            introduced_in,
            version_of(&table.page_session[name]),
            "{name}"
        );
        assert_eq!(introduced_in, ProtocolVersion::V1_4, "{name}");
    }
    assert_eq!(
        reopen_page(1).introduced_in(),
        version_of(&table.player_session["reopen_page"])
    );
    assert_eq!(reopen_page(1).introduced_in(), ProtocolVersion::V1_4);
}

#[test]
fn every_reopen_failure_has_exactly_the_version_listed_in_the_shared_value_table() {
    let table: ValueVersions = read_json("value-versions.json");
    let listed: BTreeSet<&str> = table
        .page_reopen_failed_reason
        .keys()
        .map(String::as_str)
        .collect();
    let known: BTreeSet<&str> = EVERY_REOPEN_FAILURE.into_iter().map(wire_label).collect();
    assert_eq!(listed, known, "таблица полна по значениям");
    for failure in EVERY_REOPEN_FAILURE {
        assert_eq!(
            failure.introduced_in(),
            version_of(&table.page_reopen_failed_reason[wire_label(failure)]),
            "{failure:?}"
        );
        assert_eq!(failure.introduced_in(), ProtocolVersion::V1_4);
    }
}

#[test]
fn the_required_version_of_every_new_page_message_is_1_4_whatever_the_reason() {
    for reason in [
        TabGoneReason::Closed,
        TabGoneReason::NavigatedAway,
        TabGoneReason::BrowserClosing,
        TabGoneReason::PagePlayerFailed,
    ] {
        assert_eq!(
            page_left(reason).required_version(),
            ProtocolVersion::V1_4,
            "{reason:?}"
        );
    }
    assert_eq!(
        PageSessionMessage::PageReturned {}.required_version(),
        ProtocolVersion::V1_4
    );
    for failure in EVERY_REOPEN_FAILURE {
        assert_eq!(
            reopen_failed(failure).required_version(),
            ProtocolVersion::V1_4,
            "{failure:?}"
        );
    }
}

#[test]
fn all_new_page_messages_are_not_bound_to_a_stream_generation() {
    for message in [
        page_left(TabGoneReason::Closed),
        PageSessionMessage::PageReturned {},
        reopen_failed(PageReopenFailure::PageDidNotLoad),
    ] {
        assert_eq!(message.stream_scope(), StreamScope::Unbound, "{message:?}");
    }
}

#[test]
fn a_1_3_session_never_gets_the_new_messages_and_the_gate_rejects_them() {
    // Новый плеер со старым экстеншеном: общая версия — 1.3.
    let agreed =
        negotiate_protocol_version(CURRENT_PROTOCOL_VERSION, ProtocolVersion::V1_3).unwrap();
    assert_eq!(agreed, ProtocolVersion::V1_3);
    assert!(!agreed.includes(reopen_page(1).introduced_in()));
    for message in [
        page_left(TabGoneReason::Closed),
        PageSessionMessage::PageReturned {},
        reopen_failed(PageReopenFailure::NoBrowserWindow),
    ] {
        let mut gate = StreamGate::for_agreed_version(agreed);
        assert_eq!(
            gate.apply_message(&message),
            Err(StreamViolation::MessageNotInAgreedVersion {
                agreed: ProtocolVersion::V1_3,
                introduced_in: ProtocolVersion::V1_4,
            }),
            "{message:?}"
        );
    }
    // `tab_gone` в 1.3 остаётся способом кончить сеанс.
    let mut gate = StreamGate::for_agreed_version(agreed);
    assert_eq!(
        gate.apply_message(&PageSessionMessage::TabGone {
            reason: TabGoneReason::Closed
        }),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn negotiating_1_4_against_1_3_gives_1_3_in_both_roles() {
    assert_eq!(
        negotiate_protocol_version(ProtocolVersion::V1_4, ProtocolVersion::V1_3),
        Ok(ProtocolVersion::V1_3)
    );
    assert_eq!(
        negotiate_protocol_version(ProtocolVersion::V1_3, ProtocolVersion::V1_4),
        Ok(ProtocolVersion::V1_3)
    );
    assert_eq!(
        negotiate_protocol_version(ProtocolVersion::V1_4, ProtocolVersion::V1_4),
        Ok(ProtocolVersion::V1_4)
    );
}

#[test]
fn the_new_messages_encode_to_the_exact_documented_json_and_parse_back() {
    let page_cases = [
        (
            page_left(TabGoneReason::Closed),
            r#"{"type":"page_left","reason":"closed"}"#,
        ),
        (
            page_left(TabGoneReason::PagePlayerFailed),
            r#"{"type":"page_left","reason":"page_player_failed"}"#,
        ),
        (
            PageSessionMessage::PageReturned {},
            r#"{"type":"page_returned"}"#,
        ),
        (
            reopen_failed(PageReopenFailure::NoBrowserWindow),
            r#"{"type":"page_reopen_failed","reason":"no_browser_window"}"#,
        ),
        (
            reopen_failed(PageReopenFailure::VideoDidNotStart),
            r#"{"type":"page_reopen_failed","reason":"video_did_not_start"}"#,
        ),
    ];
    for (message, json) in page_cases {
        assert_eq!(encode_message(&message).unwrap(), json);
        assert_eq!(decode_page(json), message);
    }
    let player = reopen_page(123);
    let json = r#"{"type":"reopen_page","position_us":123}"#;
    assert_eq!(encode_message(&player).unwrap(), json);
    assert_eq!(
        decode_message::<PlayerSessionMessage>(json.as_bytes()).unwrap(),
        player
    );
}

#[test]
fn reopen_failure_labels_are_strict_and_page_returned_takes_no_fields() {
    for near_miss in [
        "No_Browser_Window",
        "no-browser-window",
        " no_browser_window",
        "teleported",
    ] {
        let text = format!(r#"{{"type":"page_reopen_failed","reason":"{near_miss}"}}"#);
        assert_eq!(
            decode_message::<PageSessionMessage>(text.as_bytes())
                .unwrap_err()
                .kind(),
            MessageErrorKind::UnknownValue,
            "{near_miss:?}"
        );
    }
    assert_eq!(
        decode_message::<PageSessionMessage>(br#"{"type":"page_returned","reason":"closed"}"#)
            .unwrap_err()
            .kind(),
        MessageErrorKind::UnknownField
    );
    assert_eq!(
        decode_message::<PlayerSessionMessage>(br#"{"type":"reopen_page"}"#)
            .unwrap_err()
            .kind(),
        MessageErrorKind::MissingField
    );
}

#[test]
fn the_page_leaves_the_viewer_presses_play_and_the_same_session_continues_with_a_new_generation() {
    // Сценарий целиком на сторожу плеера: уход -> (плеер шлёт reopen_page) -> возврат -> поколение 2.
    let mut gate = StreamGate::new();
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.apply_message(&page_left(TabGoneReason::Closed)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(gate.admit_range_bytes(), Err(StreamViolation::PageIsAway));
    assert_eq!(
        gate.apply_message(&PageSessionMessage::PageReturned {}),
        Ok(GateDecision::Accept)
    );
    assert_eq!(gate.admit_range_bytes(), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.current_generation(), StreamGeneration::new(2).ok());
}

#[test]
fn a_failed_return_leaves_nothing_to_say_and_closing_uses_the_unchanged_code_4002() {
    let mut gate = StreamGate::new();
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.apply_message(&page_left(TabGoneReason::NavigatedAway)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.apply_message(&reopen_failed(PageReopenFailure::NotTheSameVideo)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.apply_message(&PageSessionMessage::PageReturned {}),
        Err(StreamViolation::PageIsAway)
    );
    // Коды закрытия в 1.4 не менялись: после отказа экстеншен закрывает 4002.
    assert_eq!(SessionCloseReason::TabGone.close_code(), 4002);
    assert_eq!(
        SessionCloseReason::from_close_code(4002),
        Some(SessionCloseReason::TabGone)
    );
}
