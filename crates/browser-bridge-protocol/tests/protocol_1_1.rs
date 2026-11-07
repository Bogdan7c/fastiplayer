//! Договоры протокола 1.1 с TypeScript, которых нет в `manifest.json`:
//! - `fixtures/message-versions.json` — в какой версии появилось каждое сообщение
//!   сеанса (`introduced_in`, `crate::version`, правило 5);
//! - `fixtures/resource-addresses.json` — верные и неверные адреса-заглушки
//!   номерков ресурсов HLS (`crate::resource_address`).
//!
//! Те же файлы проверяют тесты экстеншена (`extensions/browser/tests/protocol/`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::{
    PageSessionMessage, PlayerSessionMessage, ProtocolMessage, ProtocolVersion, ResourceHandle,
    decode_message, format_resource_address, parse_resource_address,
};
use serde::Deserialize;
use serde_json::Value;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let text = std::fs::read_to_string(fixtures_root().join(name)).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Версия из записи фикстуры `"1.1"`.
fn version_of(text: &str) -> ProtocolVersion {
    let (major, minor) = text.split_once('.').unwrap();
    ProtocolVersion {
        major: major.parse().unwrap(),
        minor: minor.parse().unwrap(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

/// Валидные фикстуры одной схемы манифеста: файл → значение `type`.
fn valid_session_fixtures(schema: &str) -> Vec<(PathBuf, String)> {
    let manifest: Value = read_json("manifest.json");
    manifest["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["schema"] == schema && case["expect"]["result"] == "ok")
        .map(|case| {
            let path = fixtures_root().join(case["file"].as_str().unwrap());
            let message: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            (path, message["type"].as_str().unwrap().to_owned())
        })
        .collect()
}

fn known_types<M: ProtocolMessage>() -> BTreeSet<String> {
    M::KNOWN_TYPES
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

#[test]
fn every_session_message_type_has_exactly_the_version_listed_in_the_shared_table() {
    let table: MessageVersions = read_json("message-versions.json");
    assert_eq!(
        table.page_session.keys().cloned().collect::<BTreeSet<_>>(),
        known_types::<PageSessionMessage>()
    );
    assert_eq!(
        table
            .player_session
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        known_types::<PlayerSessionMessage>()
    );
    // Сверяем по настоящим сообщениям: `introduced_in` у каждой валидной фикстуры.
    for (path, message_type) in valid_session_fixtures("page_session") {
        let message: PageSessionMessage = decode_message(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            message.introduced_in(),
            version_of(&table.page_session[&message_type]),
            "{}",
            path.display()
        );
    }
    for (path, message_type) in valid_session_fixtures("player_session") {
        let message: PlayerSessionMessage = decode_message(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            message.introduced_in(),
            version_of(&table.player_session[&message_type]),
            "{}",
            path.display()
        );
    }
}

#[test]
fn only_the_hls_messages_are_new_in_protocol_1_1() {
    let table: MessageVersions = read_json("message-versions.json");
    let newer: BTreeSet<&str> = table
        .page_session
        .iter()
        .chain(&table.player_session)
        .filter(|(_, version)| version.as_str() == "1.1")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        newer,
        BTreeSet::from([
            "fetch_resource",
            "hls_source_added",
            "resource_fetch_finished"
        ])
    );
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceAddresses {
    valid: Vec<ValidAddress>,
    invalid: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidAddress {
    address: String,
    handle: u32,
}

#[test]
fn valid_stub_addresses_parse_to_their_handle_and_format_back_exactly() {
    let addresses: ResourceAddresses = read_json("resource-addresses.json");
    assert!(addresses.valid.len() >= 5, "мало верных адресов");
    for valid in &addresses.valid {
        assert_eq!(
            parse_resource_address(&valid.address),
            Some(ResourceHandle(valid.handle)),
            "{}",
            valid.address
        );
        assert_eq!(
            format_resource_address(ResourceHandle(valid.handle)),
            valid.address
        );
    }
}

#[test]
fn every_invalid_stub_address_is_rejected() {
    let addresses: ResourceAddresses = read_json("resource-addresses.json");
    assert!(addresses.invalid.len() >= 30, "мало неверных адресов");
    let accepted: Vec<&String> = addresses
        .invalid
        .iter()
        .filter(|address| parse_resource_address(address).is_some())
        .collect();
    assert!(accepted.is_empty(), "приняты неверные адреса: {accepted:?}");
}
