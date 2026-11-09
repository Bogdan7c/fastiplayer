//! Договоры протокола 1.6 (сессия 13c, решение 278 — «показать страницу»), которых не
//! покрывает общий проход по `manifest.json`:
//! - одно новое сообщение `show_page` (плеер → страница) — версия в `message-versions.json`;
//! - оно пишется ровно задокументированным JSON, читается обратно и не принимает полей;
//! - при общей версии 1.5 и ниже плеер его не шлёт (версия сообщения выше общей).
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-6.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, MessageErrorKind, PlayerSessionMessage, ProtocolVersion,
    decode_message, encode_message, negotiate_protocol_version,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

fn read_message_versions() -> MessageVersions {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("message-versions.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn protocol_1_6_adds_exactly_one_player_message_and_no_page_message() {
    let table = read_message_versions();
    let newer_player: BTreeSet<&str> = table
        .player_session
        .iter()
        .filter(|(_, version)| version.as_str() == "1.6")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(newer_player, BTreeSet::from(["show_page"]));
    assert!(
        table
            .page_session
            .values()
            .all(|version| version.as_str() != "1.6"),
        "странице в 1.6 новых сообщений нет"
    );
    assert_eq!(
        PlayerSessionMessage::ShowPage {}.introduced_in(),
        ProtocolVersion::V1_6
    );
    assert!(CURRENT_PROTOCOL_VERSION.includes(ProtocolVersion::V1_6));
}

#[test]
fn show_page_encodes_to_the_exact_documented_json_and_parses_back() {
    let message = PlayerSessionMessage::ShowPage {};
    let json = encode_message(&message).unwrap();
    assert_eq!(json, r#"{"type":"show_page"}"#);
    assert_eq!(
        decode_message::<PlayerSessionMessage>(json.as_bytes()).unwrap(),
        message
    );
}

#[test]
fn show_page_accepts_no_fields() {
    assert_eq!(
        decode_message::<PlayerSessionMessage>(br#"{"type":"show_page","tab":1}"#)
            .unwrap_err()
            .kind(),
        MessageErrorKind::UnknownField
    );
}

#[test]
fn against_a_1_5_extension_the_agreed_version_is_1_5_and_show_page_is_not_included() {
    // Плеер сверяет `introduced_in` с общей версией ДО отправки: при 1.5 сообщение не уходит.
    for (player, extension) in [
        (ProtocolVersion::V1_6, ProtocolVersion::V1_5),
        (ProtocolVersion::V1_5, ProtocolVersion::V1_6),
    ] {
        let agreed = negotiate_protocol_version(player, extension).unwrap();
        assert_eq!(agreed, ProtocolVersion::V1_5);
        assert!(!agreed.includes(PlayerSessionMessage::ShowPage {}.introduced_in()));
    }
    let agreed = negotiate_protocol_version(ProtocolVersion::V1_6, ProtocolVersion::V1_6).unwrap();
    assert!(agreed.includes(PlayerSessionMessage::ShowPage {}.introduced_in()));
}
