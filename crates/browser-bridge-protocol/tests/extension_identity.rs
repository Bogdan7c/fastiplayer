//! Договор «кто наш экстеншен» (сессия 05): константы Rust против общей фикстуры.
//!
//! Экстеншен (сессия 07) берёт публичный ключ и ID Firefox из
//! `fixtures/extension-identity.json`, посредник — из констант
//! `browser_bridge_protocol::extension_identity`. Тест не даёт им разойтись и
//! проверяет, что ID Chrome действительно выводится из ключа, — иначе распакованный
//! экстеншен получил бы другой ID, и посредник его не пустил бы.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::path::Path;

use browser_bridge_protocol::extension_identity::{
    ALLOWED_EXTENSIONS, BrowserFamily, CHROMIUM_DEVELOPMENT_EXTENSION_ID, ExtensionChannel,
    FIREFOX_EXTENSION_ID, NATIVE_HOST_NAME,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn identity_fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/extension-identity.json");
    serde_json::from_slice(&std::fs::read(path).expect("identity fixture")).expect("valid JSON")
}

/// Стандартный base64 (RFC 4648 с `=`), как Chrome хранит `key` в манифесте.
/// Свой декодер на десяток строк — чтобы не тащить crate ради одного теста.
fn decode_standard_base64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits: u32 = 0;
    let mut bit_count = 0;
    let mut bytes = Vec::new();
    for symbol in text.trim_end_matches('=').bytes() {
        let value = ALPHABET
            .iter()
            .position(|candidate| *candidate == symbol)
            .unwrap_or_else(|| panic!("not a base64 symbol: {symbol}"));
        bits = (bits << 6) | u32::try_from(value).unwrap();
        bit_count += 6;
        if bit_count >= 8 {
            bit_count -= 8;
            bytes.push(u8::try_from((bits >> bit_count) & 0xff).unwrap());
        }
    }
    bytes
}

/// ID Chrome из публичного ключа: SHA-256, первые 16 байт, цифры 0–f → буквы a–p.
fn chromium_id_from_key(public_key_der: &[u8]) -> String {
    Sha256::digest(public_key_der)[..16]
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .map(|nibble| char::from(b'a' + nibble))
        .collect()
}

#[test]
fn chromium_development_id_is_derived_from_the_manifest_key() {
    let fixture = identity_fixture();
    let key = fixture["chromium_development"]["manifest_key"]
        .as_str()
        .expect("manifest_key");
    let derived = chromium_id_from_key(&decode_standard_base64(key));
    assert_eq!(derived, CHROMIUM_DEVELOPMENT_EXTENSION_ID);
    assert_eq!(
        fixture["chromium_development"]["extension_id"].as_str(),
        Some(CHROMIUM_DEVELOPMENT_EXTENSION_ID)
    );
}

#[test]
fn firefox_id_and_host_name_match_the_fixture() {
    let fixture = identity_fixture();
    assert_eq!(
        fixture["firefox"]["gecko_id"].as_str(),
        Some(FIREFOX_EXTENSION_ID)
    );
    assert_eq!(fixture["native_host_name"].as_str(), Some(NATIVE_HOST_NAME));
}

#[test]
fn every_development_id_of_the_list_is_in_the_fixture() {
    // Сборка разработчика есть у обоих браузеров, и в списке нет «лишних» ID
    // разработчика, о которых не знает экстеншен.
    let development: Vec<_> = ALLOWED_EXTENSIONS
        .iter()
        .filter(|allowed| allowed.channel == ExtensionChannel::Development)
        .map(|allowed| (allowed.browser, allowed.extension_id))
        .collect();
    assert_eq!(
        development,
        vec![
            (BrowserFamily::Chromium, CHROMIUM_DEVELOPMENT_EXTENSION_ID),
            (BrowserFamily::Firefox, FIREFOX_EXTENSION_ID),
        ]
    );
}

#[test]
fn the_derivation_matches_an_independent_example() {
    // Самопроверка формулы на примере, посчитанном независимо (Python `hashlib`):
    // SHA-256 пустой строки начинается с e3b0c442… → «odlameec…». Без этой
    // проверки ошибка формулы прошла бы незамеченной, если бы константу
    // посчитали той же ошибочной формулой.
    assert_eq!(
        chromium_id_from_key(b""),
        "odlameecjipmbmbejkplpemijjgpljce"
    );
}
