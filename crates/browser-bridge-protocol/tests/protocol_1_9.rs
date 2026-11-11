//! Договоры протокола 1.9 (сессия 16a — смена серии на той же странице), которых не покрывает
//! общий проход по `manifest.json`:
//! - ровно два новых сообщения страница → плеер: `awaiting_next_episode` и
//!   `page_title_changed` — версии в `message-versions.json`;
//! - они пишутся ровно задокументированным JSON и читаются обратно;
//! - заголовок проверяется тем же правилом, что `title` в `open_session` (тип `PageTitle`):
//!   что отвергнуто там, отвергнуто и здесь с той же причиной;
//! - при общей версии 1.8 и ниже сторож плеера оба слова отвергает (старый экстеншен их и не
//!   шлёт), при 1.9 принимает; пока страница ушла (`page_left`), они — нарушение;
//! - код закрытия 4011 `next_episode_not_started` переводится в обе стороны, а экстеншен
//!   вправе им закрыть связь только при общей версии 1.9.
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-9.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::session::{
    NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN, NextEpisodePatience, StreamScope, TabGoneReason,
};
use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, GateDecision, MessageErrorKind, PageSessionMessage, PageTitle,
    ProtocolVersion, SessionCloseReason, StreamGate, StreamViolation, decode_message,
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

/// Имена сообщений, появившихся ровно в 1.9.
fn introduced_in_1_9(table: &BTreeMap<String, String>) -> BTreeSet<&str> {
    table
        .iter()
        .filter(|(_, version)| version.as_str() == "1.9")
        .map(|(name, _)| name.as_str())
        .collect()
}

/// Сообщения 1.9 и их точный JSON на проводе.
fn documented_messages() -> [(PageSessionMessage, &'static str); 4] {
    [
        (
            PageSessionMessage::AwaitingNextEpisode {
                patience_ms: NextEpisodePatience::from_millis(60_000),
            },
            r#"{"type":"awaiting_next_episode","patience_ms":60000}"#,
        ),
        (
            PageSessionMessage::AwaitingNextEpisode {
                patience_ms: NextEpisodePatience::from_millis(u32::MAX),
            },
            r#"{"type":"awaiting_next_episode","patience_ms":4294967295}"#,
        ),
        (
            PageSessionMessage::PageTitleChanged {
                title: PageTitle::new("Серія 4 — Озвучка 🎬").unwrap(),
            },
            r#"{"type":"page_title_changed","title":"Серія 4 — Озвучка 🎬"}"#,
        ),
        // Страница без `<title>`: пустой заголовок допустим, как в `open_session`.
        (
            PageSessionMessage::PageTitleChanged {
                title: PageTitle::new("").unwrap(),
            },
            r#"{"type":"page_title_changed","title":""}"#,
        ),
    ]
}

#[test]
fn protocol_1_9_adds_exactly_the_two_episode_change_messages() {
    let table = read_message_versions();
    assert_eq!(
        introduced_in_1_9(&table.page_session),
        BTreeSet::from(["awaiting_next_episode", "page_title_changed"])
    );
    assert!(introduced_in_1_9(&table.player_session).is_empty());
    for (message, _) in documented_messages() {
        assert_eq!(message.introduced_in(), ProtocolVersion::V1_9);
        assert_eq!(message.required_version(), ProtocolVersion::V1_9);
        // Оба слова не про поток: поколение их не касается.
        assert_eq!(message.stream_scope(), StreamScope::Unbound);
    }
    // С 1.10 crate говорит на более новой версии, а 1.9 в неё входит.
    assert!(CURRENT_PROTOCOL_VERSION.includes(ProtocolVersion::V1_9));
}

#[test]
fn episode_change_messages_encode_to_the_exact_documented_json_and_parse_back() {
    for (message, json) in documented_messages() {
        assert_eq!(encode_message(&message).unwrap(), json);
        assert_eq!(
            decode_message::<PageSessionMessage>(json.as_bytes()).unwrap(),
            message
        );
    }
    // Срок доходит до плеера тем же числом миллисекунд.
    let decoded = decode_message::<PageSessionMessage>(
        br#"{"type":"awaiting_next_episode","patience_ms":60000}"#,
    )
    .unwrap();
    let PageSessionMessage::AwaitingNextEpisode { patience_ms } = decoded else {
        panic!("ожидалось awaiting_next_episode: {decoded:?}");
    };
    assert_eq!(
        patience_ms.as_duration(),
        std::time::Duration::from_secs(60)
    );
}

#[test]
fn episode_change_messages_reject_extra_fields_and_foreign_values() {
    let cases: [(&[u8], MessageErrorKind); 7] = [
        (
            br#"{"type":"awaiting_next_episode","patience_ms":60000,"title":"x"}"#,
            MessageErrorKind::UnknownField,
        ),
        (
            br#"{"type":"awaiting_next_episode"}"#,
            MessageErrorKind::MissingField,
        ),
        (
            br#"{"type":"awaiting_next_episode","patience_ms":"60000"}"#,
            MessageErrorKind::WrongValueType,
        ),
        (
            br#"{"type":"awaiting_next_episode","patience_ms":-1}"#,
            MessageErrorKind::NumberOutOfRange,
        ),
        (
            br#"{"type":"page_title_changed","title":"x","domain":"example.com"}"#,
            MessageErrorKind::UnknownField,
        ),
        (
            br#"{"type":"page_title_changed"}"#,
            MessageErrorKind::MissingField,
        ),
        (
            br#"{"type":"page_title_changed","title":4}"#,
            MessageErrorKind::WrongValueType,
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
fn the_changed_title_follows_exactly_the_open_session_title_rule() {
    // Одно правило на оба места: `PageTitle`. Для каждого заголовка итог `page_title_changed`
    // совпадает с итогом самой проверки `PageTitle::new` — и при приёме, и при отказе.
    let at_limit = "ї".repeat(1024 / 2);
    let over_limit = format!("{at_limit}x");
    let titles = [
        "Серія 4".to_owned(),
        String::new(),
        at_limit,
        over_limit,
        "movie\u{202e}4pm.exe".to_owned(),
        "a\u{0}b".to_owned(),
        "Серія 4\nОзвучка".to_owned(),
    ];
    for title in titles {
        let json = serde_json::json!({"type": "page_title_changed", "title": title}).to_string();
        let parsed = decode_message::<PageSessionMessage>(json.as_bytes());
        match PageTitle::new(title.clone()) {
            Ok(expected) => assert_eq!(
                parsed.unwrap(),
                PageSessionMessage::PageTitleChanged { title: expected }
            ),
            Err(violation) => assert_eq!(
                parsed.unwrap_err().kind(),
                MessageErrorKind::from(violation),
                "{title:?}"
            ),
        }
    }
}

#[test]
fn against_a_1_8_extension_the_agreed_version_is_1_8_and_the_new_words_are_not_included() {
    // Экстеншен сверяет `introduced_in` с общей версией ДО отправки: при 1.8 слова не уходят,
    // а связь при несостоявшейся серии закрывается прежним 4009, а не 4011.
    for (player, extension) in [
        (ProtocolVersion::V1_9, ProtocolVersion::V1_8),
        (ProtocolVersion::V1_8, ProtocolVersion::V1_9),
    ] {
        let agreed = negotiate_protocol_version(player, extension).unwrap();
        assert_eq!(agreed, ProtocolVersion::V1_8);
        for (message, _) in documented_messages() {
            assert!(!agreed.includes(message.introduced_in()));
        }
        assert!(!agreed.includes(NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN));
    }
    let agreed = negotiate_protocol_version(ProtocolVersion::V1_9, ProtocolVersion::V1_9).unwrap();
    assert!(agreed.includes(NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN));
}

#[test]
fn the_gate_rejects_the_new_words_when_1_8_was_agreed_and_accepts_them_at_1_9() {
    for (message, _) in documented_messages() {
        let mut older = StreamGate::for_agreed_version(ProtocolVersion::V1_8);
        assert_eq!(
            older.apply_message(&message),
            Err(StreamViolation::MessageNotInAgreedVersion {
                agreed: ProtocolVersion::V1_8,
                introduced_in: ProtocolVersion::V1_9,
            }),
            "{message:?}"
        );
        let mut current = StreamGate::for_agreed_version(ProtocolVersion::V1_9);
        assert_eq!(current.apply_message(&message), Ok(GateDecision::Accept));
    }
}

#[test]
fn the_new_words_are_a_violation_while_the_page_is_away() {
    // Пока страница ушла (`page_left`), принимаются только «вернулась», «не вернулась» и
    // `tab_gone`: экстеншен не вправе слать ни ожидание серии, ни новый заголовок.
    for (message, _) in documented_messages() {
        let mut gate = StreamGate::for_agreed_version(ProtocolVersion::V1_9);
        let page_left = PageSessionMessage::PageLeft {
            reason: TabGoneReason::NavigatedAway,
        };
        assert_eq!(gate.apply_message(&page_left), Ok(GateDecision::Accept));
        assert_eq!(
            gate.apply_message(&message),
            Err(StreamViolation::PageIsAway),
            "{message:?}"
        );
    }
}

#[test]
fn the_next_episode_not_started_closes_with_4011_and_older_codes_did_not_move() {
    assert_eq!(SessionCloseReason::NextEpisodeNotStarted.close_code(), 4011);
    assert_eq!(
        SessionCloseReason::from_close_code(4011),
        Some(SessionCloseReason::NextEpisodeNotStarted)
    );
    // Прежний код, которым экстеншен закрывает связь со старым плеером, не сдвинулся.
    assert_eq!(SessionCloseReason::PageTookVideoBack.close_code(), 4009);
    assert_eq!(SessionCloseReason::from_close_code(4012), None);
}
