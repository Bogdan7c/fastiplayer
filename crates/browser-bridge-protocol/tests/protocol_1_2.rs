//! Договоры протокола 1.2 (сессия 10a3, решение 232 — HLS live), которых не
//! покрывает общий проход по `manifest.json`:
//! - в 1.2 нового ровно одно сообщение — `live_window` (`fixtures/message-versions.json`);
//! - собеседник с общей версией 1.1 и ниже `live_window` не получает
//!   (`ProtocolVersion::includes`, правило 5 `crate::version`);
//! - инварианты окна `start ≤ end ≤ edge` проверяются и при записи, а не только
//!   при разборе: плеер не может отправить вывернутое окно;
//! - договорные числа (частота окна).
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-2.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use browser_bridge_protocol::limits::MIN_LIVE_WINDOW_INTERVAL_MS;
use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, LiveSeekable, MediaTime, MessageErrorKind, PlayerSessionMessage,
    ProtocolVersion, StreamGeneration, decode_message, encode_message, negotiate_protocol_version,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

fn at(micros: u64) -> MediaTime {
    MediaTime::from_micros(micros).unwrap()
}

/// Окно из примера договора (бриф 10a3): поколение 3, край 61 с, окно 40–60,5 с.
fn window_from_the_contract(seekable: LiveSeekable) -> PlayerSessionMessage {
    PlayerSessionMessage::LiveWindow {
        generation: StreamGeneration::new(3).unwrap(),
        edge_us: at(61_000_000),
        seekable,
    }
}

fn contract_range() -> LiveSeekable {
    LiveSeekable::Range {
        start_us: at(40_000_000),
        end_us: at(60_500_000),
    }
}

#[test]
fn live_window_is_the_only_message_new_in_protocol_1_2() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/message-versions.json");
    let table: MessageVersions =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let newer: BTreeSet<&str> = table
        .page_session
        .iter()
        .chain(&table.player_session)
        .filter(|(_, version)| version.as_str() == "1.2")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(newer, BTreeSet::from(["live_window"]));
}

#[test]
fn live_window_reaches_only_a_peer_that_agreed_on_1_2() {
    let message = window_from_the_contract(LiveSeekable::None {});
    assert_eq!(message.introduced_in(), ProtocolVersion::V1_2);
    // Новый плеер со старым экстеншеном: общая версия — старая, и окно ей не входит.
    for older_extension in [ProtocolVersion::V1_1, ProtocolVersion::V1_0] {
        let agreed = negotiate_protocol_version(CURRENT_PROTOCOL_VERSION, older_extension).unwrap();
        assert_eq!(agreed, older_extension);
        assert!(!agreed.includes(message.introduced_in()), "{agreed:?}");
    }
    let agreed =
        negotiate_protocol_version(CURRENT_PROTOCOL_VERSION, ProtocolVersion::V1_2).unwrap();
    assert!(agreed.includes(message.introduced_in()));
}

#[test]
fn contract_windows_encode_to_the_exact_documented_json_and_parse_back() {
    let cases = [
        (
            window_from_the_contract(contract_range()),
            r#"{"type":"live_window","generation":3,"edge_us":61000000,"seekable":{"kind":"range","start_us":40000000,"end_us":60500000}}"#,
        ),
        (
            window_from_the_contract(LiveSeekable::None {}),
            r#"{"type":"live_window","generation":3,"edge_us":61000000,"seekable":{"kind":"none"}}"#,
        ),
    ];
    for (message, json) in cases {
        assert_eq!(encode_message(&message).unwrap(), json);
        assert_eq!(
            decode_message::<PlayerSessionMessage>(json.as_bytes()).unwrap(),
            message
        );
    }
}

#[test]
fn the_player_cannot_write_a_window_that_breaks_start_end_edge() {
    let inverted = window_from_the_contract(LiveSeekable::Range {
        start_us: at(50_000_001),
        end_us: at(50_000_000),
    });
    assert_eq!(
        encode_message(&inverted).unwrap_err().kind(),
        MessageErrorKind::EmptyRange
    );
    let beyond_edge = window_from_the_contract(LiveSeekable::Range {
        start_us: at(40_000_000),
        end_us: at(61_000_001),
    });
    assert_eq!(
        encode_message(&beyond_edge).unwrap_err().kind(),
        MessageErrorKind::NumberOutOfRange
    );
    // Граница включительно: конец ровно на краю — честное окно.
    let at_edge = window_from_the_contract(LiveSeekable::Range {
        start_us: at(61_000_000),
        end_us: at(61_000_000),
    });
    assert!(encode_message(&at_edge).is_ok());
}

#[test]
fn live_window_rate_limit_is_the_documented_contract_number() {
    // Число закреплено: его же зеркалит экстеншен (`limits.ts`).
    assert_eq!(MIN_LIVE_WINDOW_INTERVAL_MS, 1_000);
}
