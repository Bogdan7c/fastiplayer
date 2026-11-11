//! Договоры протокола 1.7 (сессия 14a — варианты страницы в окне плеера), которых не
//! покрывает общий проход по `manifest.json`:
//! - три новых сообщения: `page_navigation`, `subtitle_preference` (страница → плеер) и
//!   `subtitle_choice` (плеер → страница) — версии в `message-versions.json`;
//! - каждое пишется ровно задокументированным JSON и читается обратно;
//! - при общей версии 1.6 и ниже плеер их не шлёт, а сторож плеера входящие отвергает;
//! - сообщения 1.0 про варианты (`variant_menu_changed`, `select_variant`, `next_episode`)
//!   по-прежнему из 1.0.
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-7.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::session::{
    NextEpisodeAvailability, PageSubtitleTrackId, SubtitleChoice,
};
use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, GateDecision, MessageErrorKind, PageSessionMessage,
    PlayerSessionMessage, ProtocolVersion, StreamGate, StreamViolation, decode_message,
    encode_message, negotiate_protocol_version,
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

fn newer_than_1_6(table: &BTreeMap<String, String>) -> BTreeSet<&str> {
    table
        .iter()
        .filter(|(_, version)| version.as_str() == "1.7")
        .map(|(name, _)| name.as_str())
        .collect()
}

fn page_track(number: u32) -> SubtitleChoice {
    SubtitleChoice::PageTrack {
        track: PageSubtitleTrackId::new(number).unwrap(),
    }
}

fn page_messages() -> [PageSessionMessage; 3] {
    [
        PageSessionMessage::PageNavigation {
            next_episode: NextEpisodeAvailability::Available,
        },
        PageSessionMessage::SubtitlePreference {
            choice: page_track(3),
        },
        PageSessionMessage::SubtitlePreference {
            choice: SubtitleChoice::Off {},
        },
    ]
}

#[test]
fn protocol_1_7_adds_exactly_the_documented_messages() {
    let table = read_message_versions();
    assert_eq!(
        newer_than_1_6(&table.page_session),
        BTreeSet::from(["page_navigation", "subtitle_preference"])
    );
    assert_eq!(
        newer_than_1_6(&table.player_session),
        BTreeSet::from(["subtitle_choice"])
    );
    // Сообщения 1.0 про варианты не менялись.
    assert_eq!(table.page_session["variant_menu_changed"], "1.0");
    for name in ["select_variant", "next_episode"] {
        assert_eq!(table.player_session[name], "1.0");
    }
    for message in page_messages() {
        assert_eq!(message.introduced_in(), ProtocolVersion::V1_7);
        assert_eq!(message.required_version(), ProtocolVersion::V1_7);
    }
    assert_eq!(
        PlayerSessionMessage::SubtitleChoice {
            choice: SubtitleChoice::Off {}
        }
        .introduced_in(),
        ProtocolVersion::V1_7
    );
    // С 1.8 crate говорит на более новой версии, а 1.7 в неё входит.
    assert!(CURRENT_PROTOCOL_VERSION.includes(ProtocolVersion::V1_7));
}

#[test]
fn page_messages_encode_to_the_exact_documented_json_and_parse_back() {
    let expected = [
        r#"{"type":"page_navigation","next_episode":"available"}"#,
        r#"{"type":"subtitle_preference","choice":{"kind":"page_track","track":3}}"#,
        r#"{"type":"subtitle_preference","choice":{"kind":"off"}}"#,
    ];
    for (message, json) in page_messages().into_iter().zip(expected) {
        assert_eq!(encode_message(&message).unwrap(), json);
        assert_eq!(
            decode_message::<PageSessionMessage>(json.as_bytes()).unwrap(),
            message
        );
    }
    assert_eq!(
        decode_message::<PageSessionMessage>(
            br#"{"type":"page_navigation","next_episode":"unavailable"}"#
        )
        .unwrap(),
        PageSessionMessage::PageNavigation {
            next_episode: NextEpisodeAvailability::Unavailable
        }
    );
}

#[test]
fn subtitle_choice_encodes_to_the_exact_documented_json_and_parses_back() {
    for (choice, json) in [
        (
            page_track(7),
            r#"{"type":"subtitle_choice","choice":{"kind":"page_track","track":7}}"#,
        ),
        (
            SubtitleChoice::Off {},
            r#"{"type":"subtitle_choice","choice":{"kind":"off"}}"#,
        ),
    ] {
        let message = PlayerSessionMessage::SubtitleChoice { choice };
        assert_eq!(encode_message(&message).unwrap(), json);
        assert_eq!(
            decode_message::<PlayerSessionMessage>(json.as_bytes()).unwrap(),
            message
        );
    }
}

#[test]
fn the_new_messages_reject_unknown_fields_zero_tracks_and_foreign_kinds() {
    let page_cases: [(&[u8], MessageErrorKind); 5] = [
        (
            br#"{"type":"page_navigation","next_episode":"available","url":"x"}"#,
            MessageErrorKind::UnknownField,
        ),
        (
            br#"{"type":"subtitle_preference","choice":{"kind":"off","track":1}}"#,
            MessageErrorKind::UnknownField,
        ),
        (
            br#"{"type":"subtitle_preference","choice":{"kind":"page_track","track":0}}"#,
            MessageErrorKind::NumberOutOfRange,
        ),
        (
            br#"{"type":"subtitle_preference","choice":{"kind":"own_file"}}"#,
            MessageErrorKind::UnknownValue,
        ),
        (
            br#"{"type":"page_navigation","next_episode":"maybe"}"#,
            MessageErrorKind::UnknownValue,
        ),
    ];
    for (json, kind) in page_cases {
        assert_eq!(
            decode_message::<PageSessionMessage>(json)
                .unwrap_err()
                .kind(),
            kind,
            "{}",
            String::from_utf8_lossy(json)
        );
    }
    assert_eq!(
        decode_message::<PlayerSessionMessage>(
            br#"{"type":"subtitle_choice","choice":{"kind":"page_track","track":0}}"#
        )
        .unwrap_err()
        .kind(),
        MessageErrorKind::NumberOutOfRange
    );
    assert_eq!(
        decode_message::<PlayerSessionMessage>(
            br#"{"type":"subtitle_choice","choice":{"kind":"off"},"auto":true}"#
        )
        .unwrap_err()
        .kind(),
        MessageErrorKind::UnknownField
    );
}

#[test]
fn against_a_1_6_extension_the_agreed_version_is_1_6_and_the_new_messages_are_not_included() {
    // Плеер сверяет `introduced_in` с общей версией ДО отправки: при 1.6 сообщение не уходит.
    for (player, extension) in [
        (ProtocolVersion::V1_7, ProtocolVersion::V1_6),
        (ProtocolVersion::V1_6, ProtocolVersion::V1_7),
    ] {
        let agreed = negotiate_protocol_version(player, extension).unwrap();
        assert_eq!(agreed, ProtocolVersion::V1_6);
        assert!(
            !agreed.includes(
                PlayerSessionMessage::SubtitleChoice {
                    choice: SubtitleChoice::Off {}
                }
                .introduced_in()
            )
        );
    }
    let agreed = negotiate_protocol_version(ProtocolVersion::V1_7, ProtocolVersion::V1_7).unwrap();
    assert!(
        agreed.includes(
            PlayerSessionMessage::SubtitleChoice {
                choice: SubtitleChoice::Off {}
            }
            .introduced_in()
        )
    );
}

#[test]
fn the_gate_rejects_1_7_page_messages_when_1_6_was_agreed_and_accepts_them_at_1_7() {
    for message in page_messages() {
        let mut older = StreamGate::for_agreed_version(ProtocolVersion::V1_6);
        assert_eq!(
            older.apply_message(&message),
            Err(StreamViolation::MessageNotInAgreedVersion {
                agreed: ProtocolVersion::V1_6,
                introduced_in: ProtocolVersion::V1_7,
            }),
            "{message:?}"
        );
        let mut current = StreamGate::for_agreed_version(ProtocolVersion::V1_7);
        assert_eq!(current.apply_message(&message), Ok(GateDecision::Accept));
    }
}
