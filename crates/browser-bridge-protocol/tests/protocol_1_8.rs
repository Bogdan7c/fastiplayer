//! Договоры протокола 1.8 (сессия 15c2 — исход выбора на странице), которых не покрывает
//! общий проход по `manifest.json`:
//! - ровно одно новое сообщение страница → плеер `variant_outcome` — версия в
//!   `message-versions.json`; команды 1.0 `select_variant` и `next_episode` не менялись;
//! - оно пишется ровно задокументированным JSON и читается обратно;
//! - лишнее поле, чужой вид просьбы и незнакомый исход — отказ;
//! - при общей версии 1.7 и ниже сторож плеера входящий исход отвергает (старый экстеншен
//!   его и не шлёт), а при 1.8 принимает.
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-8.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::session::{VariantKind, VariantOutcome, VariantRequest};
use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, GateDecision, MessageErrorKind, PageSessionMessage, ProtocolVersion,
    StreamGate, StreamViolation, VariantId, decode_message, encode_message,
    negotiate_protocol_version,
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

/// Имена сообщений, появившихся ровно в 1.8.
fn introduced_in_1_8(table: &BTreeMap<String, String>) -> BTreeSet<&str> {
    table
        .iter()
        .filter(|(_, version)| version.as_str() == "1.8")
        .map(|(name, _)| name.as_str())
        .collect()
}

fn select(menu: VariantKind, variant_id: &str) -> VariantRequest {
    VariantRequest::SelectVariant {
        menu,
        variant_id: VariantId::new(variant_id).unwrap(),
    }
}

/// Сообщения 1.8 и их точный JSON на проводе — по одному на каждую просьбу и каждый исход.
fn documented_messages() -> [(PageSessionMessage, &'static str); 5] {
    [
        (
            PageSessionMessage::VariantOutcome {
                request: select(VariantKind::Audio, "dub-b"),
                outcome: VariantOutcome::NoSuchEpisode,
            },
            r#"{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":"dub-b"},"outcome":"no_such_episode"}"#,
        ),
        (
            PageSessionMessage::VariantOutcome {
                request: select(VariantKind::Quality, "q1_a7"),
                outcome: VariantOutcome::Done,
            },
            r#"{"type":"variant_outcome","request":{"kind":"select_variant","menu":"quality","variant_id":"q1_a7"},"outcome":"done"}"#,
        ),
        (
            PageSessionMessage::VariantOutcome {
                request: select(VariantKind::Audio, "dub-c"),
                outcome: VariantOutcome::PersonOnly,
            },
            r#"{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":"dub-c"},"outcome":"person_only"}"#,
        ),
        (
            PageSessionMessage::VariantOutcome {
                request: VariantRequest::NextEpisode {},
                outcome: VariantOutcome::Failed,
            },
            r#"{"type":"variant_outcome","request":{"kind":"next_episode"},"outcome":"failed"}"#,
        ),
        (
            PageSessionMessage::VariantOutcome {
                request: VariantRequest::NextEpisode {},
                outcome: VariantOutcome::Done,
            },
            r#"{"type":"variant_outcome","request":{"kind":"next_episode"},"outcome":"done"}"#,
        ),
    ]
}

#[test]
fn protocol_1_8_adds_exactly_the_variant_outcome_message() {
    let table = read_message_versions();
    assert_eq!(
        introduced_in_1_8(&table.page_session),
        BTreeSet::from(["variant_outcome"])
    );
    assert!(introduced_in_1_8(&table.player_session).is_empty());
    // Команды, на которые отвечает исход, по-прежнему из 1.0.
    for name in ["select_variant", "next_episode"] {
        assert_eq!(table.player_session[name], "1.0");
    }
    for (message, _) in documented_messages() {
        assert_eq!(message.introduced_in(), ProtocolVersion::V1_8);
        assert_eq!(message.required_version(), ProtocolVersion::V1_8);
    }
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::V1_8);
}

#[test]
fn variant_outcome_encodes_to_the_exact_documented_json_and_parses_back() {
    for (message, json) in documented_messages() {
        assert_eq!(encode_message(&message).unwrap(), json);
        assert_eq!(
            decode_message::<PageSessionMessage>(json.as_bytes()).unwrap(),
            message
        );
    }
}

#[test]
fn variant_outcome_rejects_extra_fields_foreign_requests_and_unknown_outcomes() {
    let cases: [(&[u8], MessageErrorKind); 6] = [
        (
            br#"{"type":"variant_outcome","request":{"kind":"next_episode"},"outcome":"done","label":"x"}"#,
            MessageErrorKind::UnknownField,
        ),
        // У Next нет меню и пункта: чужое поле в просьбе — отказ, а не молчаливый пропуск.
        (
            br#"{"type":"variant_outcome","request":{"kind":"next_episode","menu":"audio"},"outcome":"done"}"#,
            MessageErrorKind::UnknownField,
        ),
        (
            br#"{"type":"variant_outcome","request":{"kind":"skip_ad"},"outcome":"done"}"#,
            MessageErrorKind::UnknownValue,
        ),
        (
            br#"{"type":"variant_outcome","request":{"kind":"next_episode"},"outcome":"ignored"}"#,
            MessageErrorKind::UnknownValue,
        ),
        (
            br#"{"type":"variant_outcome","request":{"kind":"select_variant","variant_id":"dub-b"},"outcome":"done"}"#,
            MessageErrorKind::MissingField,
        ),
        (
            br#"{"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":""},"outcome":"done"}"#,
            MessageErrorKind::TextEmpty,
        ),
    ];
    for (json, kind) in cases {
        assert_eq!(
            decode_message::<PageSessionMessage>(json)
                .unwrap_err()
                .kind(),
            kind,
            "{}",
            String::from_utf8_lossy(json)
        );
    }
}

#[test]
fn against_a_1_7_extension_the_agreed_version_is_1_7_and_the_outcome_is_not_included() {
    // Экстеншен сверяет `introduced_in` с общей версией ДО отправки: при 1.7 исход не уходит.
    let outcome_introduced_in = documented_messages()[0].0.introduced_in();
    for (player, extension) in [
        (ProtocolVersion::V1_8, ProtocolVersion::V1_7),
        (ProtocolVersion::V1_7, ProtocolVersion::V1_8),
    ] {
        let agreed = negotiate_protocol_version(player, extension).unwrap();
        assert_eq!(agreed, ProtocolVersion::V1_7);
        assert!(!agreed.includes(outcome_introduced_in));
    }
    let agreed = negotiate_protocol_version(ProtocolVersion::V1_8, ProtocolVersion::V1_8).unwrap();
    assert!(agreed.includes(outcome_introduced_in));
}

#[test]
fn the_gate_rejects_the_outcome_when_1_7_was_agreed_and_accepts_it_at_1_8() {
    for (message, _) in documented_messages() {
        let mut older = StreamGate::for_agreed_version(ProtocolVersion::V1_7);
        assert_eq!(
            older.apply_message(&message),
            Err(StreamViolation::MessageNotInAgreedVersion {
                agreed: ProtocolVersion::V1_7,
                introduced_in: ProtocolVersion::V1_8,
            }),
            "{message:?}"
        );
        let mut current = StreamGate::for_agreed_version(ProtocolVersion::V1_8);
        assert_eq!(current.apply_message(&message), Ok(GateDecision::Accept));
    }
}
