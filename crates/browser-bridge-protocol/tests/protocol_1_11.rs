//! Договоры протокола 1.11 (мост 16d1, решения 325–327, бэклог п. 41 — окно записи
//! `recording_window`), которых не покрывает общий проход по `manifest.json`:
//! - сообщение новое и одно (`message-versions.json`), полей у старых сообщений 1.11 не меняет;
//! - оно пишется ровно задокументированным JSON и читается обратно тем же значением;
//! - инвариант `start ≤ end ≤ конца записи` держится при разборе и при записи, с точной причиной;
//! - строгость формы (лишнее/пропавшее поле, `null`, метка) не ослаблена;
//! - оно требует общей версии 1.11: пир 1.10 его не принимает, и плеер до отправки это видит.
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-11.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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

/// Версия из текста таблицы — числами: `"1.11"` как строка меньше `"1.9"`.
fn version_of(text: &str) -> ProtocolVersion {
    let (major, minor) = text.split_once('.').unwrap();
    ProtocolVersion {
        major: major.parse().unwrap(),
        minor: minor.parse().unwrap(),
    }
}

fn at(micros: u64) -> MediaTime {
    MediaTime::from_micros(micros).unwrap()
}

fn recording_window(generation: u32, end_us: u64, seekable: LiveSeekable) -> PlayerSessionMessage {
    PlayerSessionMessage::RecordingWindow {
        generation: StreamGeneration::new(generation).unwrap(),
        end_us: at(end_us),
        seekable,
    }
}

fn range(start_us: u64, end_us: u64) -> LiveSeekable {
    LiveSeekable::Range {
        start_us: at(start_us),
        end_us: at(end_us),
    }
}

/// Сообщения и их точный JSON на проводе.
fn documented_windows() -> [(PlayerSessionMessage, &'static str); 3] {
    [
        (
            recording_window(3, 61_000_000, range(0, 61_000_000)),
            r#"{"type":"recording_window","generation":3,"end_us":61000000,"seekable":{"kind":"range","start_us":0,"end_us":61000000}}"#,
        ),
        (
            recording_window(1, 5, LiveSeekable::None {}),
            r#"{"type":"recording_window","generation":1,"end_us":5,"seekable":{"kind":"none"}}"#,
        ),
        // Окно в одну точку ровно на конце записи — край договора включительно.
        (
            recording_window(2, 9, range(9, 9)),
            r#"{"type":"recording_window","generation":2,"end_us":9,"seekable":{"kind":"range","start_us":9,"end_us":9}}"#,
        ),
    ]
}

#[test]
fn protocol_1_11_adds_exactly_the_recording_window_message() {
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/message-versions.json");
    let messages: MessageVersions =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let in_1_11: BTreeSet<&str> = messages
        .page_session
        .iter()
        .chain(&messages.player_session)
        .filter(|(_, version)| version_of(version) == ProtocolVersion::V1_11)
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(in_1_11, BTreeSet::from(["recording_window"]));
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::V1_11);
}

#[test]
fn recording_windows_encode_to_the_exact_documented_json_and_parse_back() {
    for (message, json) in documented_windows() {
        assert_eq!(encode_message(&message).unwrap(), json);
        assert_eq!(
            decode_message::<PlayerSessionMessage>(json.as_bytes()).unwrap(),
            message
        );
    }
}

#[test]
fn a_window_inside_the_recording_is_accepted_at_its_borders() {
    for seekable in [
        range(0, 0),
        range(0, 100),
        range(100, 100),
        LiveSeekable::None {},
    ] {
        let json = encode_message(&recording_window(1, 100, seekable)).unwrap();
        assert_eq!(
            decode_message::<PlayerSessionMessage>(json.as_bytes()).unwrap(),
            recording_window(1, 100, seekable)
        );
    }
}

#[test]
fn an_inconsistent_window_is_rejected_on_write_and_on_read_with_the_exact_reason() {
    // (окно, конец записи, причина): вывернутое окно и окно за концом записи.
    let cases = [
        (range(51, 50), 100, MessageErrorKind::EmptyRange),
        (range(0, 101), 100, MessageErrorKind::NumberOutOfRange),
    ];
    for (seekable, end_us, kind) in cases {
        let message = recording_window(1, end_us, seekable);
        // Отправитель не может записать противоречивое окно.
        assert_eq!(encode_message(&message).unwrap_err().kind(), kind);
        // Получатель не принимает то же окно, пришедшее по проводу.
        let json = match seekable {
            LiveSeekable::Range {
                start_us,
                end_us: window_end,
            } => format!(
                r#"{{"type":"recording_window","generation":1,"end_us":{end_us},"seekable":{{"kind":"range","start_us":{},"end_us":{}}}}}"#,
                start_us.as_micros(),
                window_end.as_micros()
            ),
            LiveSeekable::None {} => unreachable!("у «нет окна» нечего нарушать"),
        };
        assert_eq!(
            decode_message::<PlayerSessionMessage>(json.as_bytes())
                .unwrap_err()
                .kind(),
            kind,
            "{json}"
        );
    }
}

#[test]
fn the_shape_of_recording_window_stays_strict() {
    let cases = [
        (
            r#"{"type":"recording_window","generation":1,"seekable":{"kind":"none"}}"#,
            MessageErrorKind::MissingField,
        ),
        (
            r#"{"type":"recording_window","generation":1,"end_us":5}"#,
            MessageErrorKind::MissingField,
        ),
        (
            r#"{"type":"recording_window","generation":1,"end_us":5,"seekable":null}"#,
            MessageErrorKind::WrongValueType,
        ),
        (
            r#"{"type":"recording_window","generation":1,"end_us":5,"seekable":{"kind":"dvr"}}"#,
            MessageErrorKind::UnknownValue,
        ),
        (
            r#"{"type":"recording_window","generation":1,"end_us":5,"seekable":{"kind":"none"},"edge_us":5}"#,
            MessageErrorKind::UnknownField,
        ),
        (
            r#"{"type":"recording_window","generation":0,"end_us":5,"seekable":{"kind":"none"}}"#,
            MessageErrorKind::NumberOutOfRange,
        ),
        (
            r#"{"type":"recording_window","generation":1,"end_us":5.5,"seekable":{"kind":"none"}}"#,
            MessageErrorKind::WrongValueType,
        ),
    ];
    for (json, kind) in cases {
        assert_eq!(
            decode_message::<PlayerSessionMessage>(json.as_bytes())
                .unwrap_err()
                .kind(),
            kind,
            "{json}"
        );
    }
}

#[test]
fn recording_window_requires_1_11_and_a_1_10_peer_agrees_on_a_version_without_it() {
    for (message, _) in documented_windows() {
        assert_eq!(message.introduced_in(), ProtocolVersion::V1_11);
        assert_eq!(message.required_version(), ProtocolVersion::V1_11);
    }
    // Плеер сверяет общую версию ДО отправки: с экстеншеном 1.10 общая 1.10, и окно не уходит.
    for (player, extension) in [
        (ProtocolVersion::V1_11, ProtocolVersion::V1_10),
        (ProtocolVersion::V1_10, ProtocolVersion::V1_11),
    ] {
        let agreed = negotiate_protocol_version(player, extension).unwrap();
        assert_eq!(agreed, ProtocolVersion::V1_10);
        assert!(!agreed.includes(ProtocolVersion::V1_11));
    }
    let agreed =
        negotiate_protocol_version(ProtocolVersion::V1_11, ProtocolVersion::V1_11).unwrap();
    assert!(agreed.includes(ProtocolVersion::V1_11));
}
