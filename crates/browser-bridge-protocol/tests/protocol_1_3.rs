//! Договоры протокола 1.3 (сессия 11a, решение 237 — плеер сайта сломался во время
//! передачи), которых не покрывает общий проход по `manifest.json`:
//! - в 1.3 нет новых сообщений (`fixtures/message-versions.json`), новое — одно
//!   ЗНАЧЕНИЕ старого: `tab_gone{reason: "page_player_failed"}`;
//! - версия каждого значения — общая таблица `fixtures/value-versions.json`, полная
//!   по значениям (`TabGoneReason::introduced_in`);
//! - собеседник с общей версией 1.2 и ниже нового значения не получает, а сторож
//!   плеера, получив его, считает это нарушением (`PageSessionMessage::required_version`);
//! - новое значение пишется ровно задокументированным JSON и читается обратно;
//! - код закрытия 4010 `viewer_returned_to_page` (решение 236) не версионируется.
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-3.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::session::TabGoneReason;
use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, GateDecision, MessageErrorKind, PageSessionMessage, ProtocolVersion,
    SessionCloseReason, StreamGate, StreamViolation, decode_message, encode_message,
    negotiate_protocol_version,
};
use serde::Deserialize;

/// Все причины ухода вкладки. Сопоставление ниже исчерпывающее: новая причина без
/// записи здесь не соберётся, а без записи в таблице фикстуры — не пройдёт тест.
const EVERY_TAB_GONE_REASON: [TabGoneReason; 4] = [
    TabGoneReason::Closed,
    TabGoneReason::NavigatedAway,
    TabGoneReason::BrowserClosing,
    TabGoneReason::PagePlayerFailed,
];

/// Метка причины на проводе.
const fn wire_label(reason: TabGoneReason) -> &'static str {
    match reason {
        TabGoneReason::Closed => "closed",
        TabGoneReason::NavigatedAway => "navigated_away",
        TabGoneReason::BrowserClosing => "browser_closing",
        TabGoneReason::PagePlayerFailed => "page_player_failed",
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

/// Таблица версий значений: «сообщение.поле» → значение → версия.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValueVersions {
    #[serde(rename = "tab_gone.reason")]
    tab_gone_reason: BTreeMap<String, String>,
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let text = std::fs::read_to_string(fixtures_root().join(name)).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Версия из записи фикстуры `"1.3"`.
fn version_of(text: &str) -> ProtocolVersion {
    let (major, minor) = text.split_once('.').unwrap();
    ProtocolVersion {
        major: major.parse().unwrap(),
        minor: minor.parse().unwrap(),
    }
}

fn tab_gone(reason: TabGoneReason) -> PageSessionMessage {
    PageSessionMessage::TabGone { reason }
}

#[test]
fn protocol_1_3_adds_no_message_types() {
    let table: MessageVersions = read_json("message-versions.json");
    let newer: BTreeSet<&str> = table
        .page_session
        .iter()
        .chain(&table.player_session)
        .filter(|(_, version)| version.as_str() == "1.3")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(newer, BTreeSet::new());
    // Тип `tab_gone` — по-прежнему из 1.0: новое только значение.
    assert_eq!(table.page_session["tab_gone"], "1.0");
}

#[test]
fn every_tab_gone_reason_has_exactly_the_version_listed_in_the_shared_value_table() {
    let table: ValueVersions = read_json("value-versions.json");
    let listed: BTreeSet<&str> = table.tab_gone_reason.keys().map(String::as_str).collect();
    let known: BTreeSet<&str> = EVERY_TAB_GONE_REASON.into_iter().map(wire_label).collect();
    assert_eq!(listed, known, "таблица полна по значениям");
    for reason in EVERY_TAB_GONE_REASON {
        assert_eq!(
            reason.introduced_in(),
            version_of(&table.tab_gone_reason[wire_label(reason)]),
            "{reason:?}"
        );
    }
    // Новое в 1.3 — ровно одно значение.
    let newer: BTreeSet<&str> = table
        .tab_gone_reason
        .iter()
        .filter(|(_, version)| version.as_str() == "1.3")
        .map(|(value, _)| value.as_str())
        .collect();
    assert_eq!(newer, BTreeSet::from(["page_player_failed"]));
}

#[test]
fn page_player_failed_reaches_only_a_peer_that_agreed_on_1_3() {
    let message = tab_gone(TabGoneReason::PagePlayerFailed);
    assert_eq!(message.introduced_in(), ProtocolVersion::V1_0, "тип старый");
    assert_eq!(
        message.required_version(),
        ProtocolVersion::V1_3,
        "значение новое"
    );
    // Новый плеер со старым экстеншеном: общая версия — старая, значение в неё не входит.
    for older_extension in [
        ProtocolVersion::V1_2,
        ProtocolVersion::V1_1,
        ProtocolVersion::V1_0,
    ] {
        let agreed = negotiate_protocol_version(CURRENT_PROTOCOL_VERSION, older_extension).unwrap();
        assert_eq!(agreed, older_extension);
        assert!(!agreed.includes(message.required_version()), "{agreed:?}");
        // Старые причины — в любой общей версии.
        for reason in [
            TabGoneReason::Closed,
            TabGoneReason::NavigatedAway,
            TabGoneReason::BrowserClosing,
        ] {
            assert!(agreed.includes(tab_gone(reason).required_version()));
        }
    }
    let agreed =
        negotiate_protocol_version(CURRENT_PROTOCOL_VERSION, ProtocolVersion::V1_3).unwrap();
    assert!(agreed.includes(message.required_version()));
}

#[test]
fn the_player_gate_rejects_page_player_failed_on_a_1_2_session() {
    // «Договорились — значит строго» и для значений: при общей 1.2 экстеншен обязан
    // сказать `navigated_away`; новое значение — ошибка отправителя.
    let mut gate = StreamGate::for_agreed_version(ProtocolVersion::V1_2);
    assert_eq!(
        gate.apply_message(&tab_gone(TabGoneReason::PagePlayerFailed)),
        Err(StreamViolation::MessageNotInAgreedVersion {
            agreed: ProtocolVersion::V1_2,
            introduced_in: ProtocolVersion::V1_3,
        })
    );
    assert_eq!(
        gate.apply_message(&tab_gone(TabGoneReason::NavigatedAway)),
        Ok(GateDecision::Accept)
    );
    let mut current = StreamGate::for_agreed_version(ProtocolVersion::V1_3);
    assert_eq!(
        current.apply_message(&tab_gone(TabGoneReason::PagePlayerFailed)),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn page_player_failed_encodes_to_the_exact_documented_json_and_parses_back() {
    let json = r#"{"type":"tab_gone","reason":"page_player_failed"}"#;
    let message = tab_gone(TabGoneReason::PagePlayerFailed);
    assert_eq!(encode_message(&message).unwrap(), json);
    assert_eq!(
        decode_message::<PageSessionMessage>(json.as_bytes()).unwrap(),
        message
    );
    // Метка строгая: без нормализации регистра и разделителей.
    for near_miss in [
        "Page_Player_Failed",
        "page-player-failed",
        "page_player_failed ",
    ] {
        let text = format!(r#"{{"type":"tab_gone","reason":"{near_miss}"}}"#);
        assert_eq!(
            decode_message::<PageSessionMessage>(text.as_bytes())
                .unwrap_err()
                .kind(),
            MessageErrorKind::UnknownValue,
            "{near_miss:?}"
        );
    }
}

#[test]
fn the_viewer_returning_to_the_page_closes_with_4010_in_any_version() {
    // Коды закрытия не версионируются (решение 236, как 4009): число закреплено, его же
    // зеркалит экстеншен (`close-reason.ts`).
    assert_eq!(SessionCloseReason::ViewerReturnedToPage.close_code(), 4010);
    assert_eq!(
        SessionCloseReason::from_close_code(4010),
        Some(SessionCloseReason::ViewerReturnedToPage)
    );
}
