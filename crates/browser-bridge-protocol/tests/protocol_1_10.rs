//! Договоры протокола 1.10 (сессия 16a2 — номер поколения в тике часов, решение 311), которых
//! не покрывает общий проход по `manifest.json`:
//! - новых сообщений нет (`message-versions.json`), новое — одно необязательное поле
//!   `clock.generation` (`field-versions.json`); тип `clock` по-прежнему из 1.0;
//! - тик пишется ровно задокументированным JSON с полем и без него (без номера — ключа нет);
//! - `null`, строка, дробь, ноль, отрицательное и больше `u32` — отказ с точной причиной, а
//!   строгость формы (лишнее поле, повтор ключа) поле не ослабило;
//! - тик с номером требует общей версии 1.10 (`required_version`), без номера — 1.0;
//! - с экстеншеном 1.9 общая версия 1.9, и номер в тик не входит.
//!
//! Те же договоры проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-10.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::session::{CLOCK_GENERATION_INTRODUCED_IN, ClockSequence, ClockState};
use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, MediaTime, MessageErrorKind, PlaybackRate, PlayerSessionMessage,
    ProtocolVersion, StreamGeneration, decode_message, encode_message, negotiate_protocol_version,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

/// `fixtures/field-versions.json`: сторона → «сообщение.поле» → версия.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

fn read_fixture_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Версия из текста таблицы — числами: `"1.10"` как строка меньше `"1.9"`.
fn version_of(text: &str) -> ProtocolVersion {
    let (major, minor) = text.split_once('.').unwrap();
    ProtocolVersion {
        major: major.parse().unwrap(),
        minor: minor.parse().unwrap(),
    }
}

/// Тик часов с выбранным номером поколения (или без него).
fn clock_tick(generation: Option<u32>) -> PlayerSessionMessage {
    PlayerSessionMessage::Clock {
        seq: ClockSequence(42),
        position_us: MediaTime::from_micros(11_100_000).unwrap(),
        state: ClockState::Playing,
        rate: PlaybackRate::from_permille(1000).unwrap(),
        generation: generation.map(|number| StreamGeneration::new(number).unwrap()),
    }
}

/// Тики и их точный JSON на проводе: без номера (форма 1.9 и раньше), с номером, крайние номера.
fn documented_ticks() -> [(PlayerSessionMessage, &'static str); 4] {
    [
        (
            clock_tick(None),
            r#"{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000}"#,
        ),
        (
            clock_tick(Some(2)),
            r#"{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":2}"#,
        ),
        (
            clock_tick(Some(1)),
            r#"{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":1}"#,
        ),
        (
            clock_tick(Some(u32::MAX)),
            r#"{"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":4294967295}"#,
        ),
    ]
}

#[test]
fn protocol_1_10_adds_no_message_and_exactly_the_clock_generation_field() {
    let messages: MessageVersions = read_fixture_json("message-versions.json");
    let in_1_10: BTreeSet<&str> = messages
        .page_session
        .iter()
        .chain(&messages.player_session)
        .filter(|(_, version)| version_of(version) == ProtocolVersion::V1_10)
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(
        in_1_10.is_empty(),
        "в 1.10 нет новых сообщений: {in_1_10:?}"
    );
    // Тип `clock` старый: меняется только форма тика с полем.
    assert_eq!(
        version_of(&messages.player_session["clock"]),
        ProtocolVersion::V1_0
    );

    let fields: FieldVersions = read_fixture_json("field-versions.json");
    assert!(fields.page_session.is_empty());
    assert_eq!(
        fields
            .player_session
            .iter()
            .map(|(field, version)| (field.as_str(), version_of(version)))
            .collect::<Vec<_>>(),
        [("clock.generation", ProtocolVersion::V1_10)]
    );
    assert_eq!(CLOCK_GENERATION_INTRODUCED_IN, ProtocolVersion::V1_10);
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::V1_10);
}

#[test]
fn clock_ticks_encode_to_the_exact_documented_json_and_parse_back() {
    for (tick, json) in documented_ticks() {
        assert_eq!(encode_message(&tick).unwrap(), json);
        assert_eq!(
            decode_message::<PlayerSessionMessage>(json.as_bytes()).unwrap(),
            tick
        );
    }
    // Номер доходит до получателя тем же поколением.
    let decoded = decode_message::<PlayerSessionMessage>(
        br#"{"type":"clock","seq":7,"position_us":0,"state":"ended","rate":1000,"generation":3}"#,
    )
    .unwrap();
    let PlayerSessionMessage::Clock { generation, .. } = decoded else {
        panic!("ожидался clock: {decoded:?}");
    };
    assert_eq!(generation.map(StreamGeneration::get), Some(3));
}

#[test]
fn a_tick_without_a_number_has_no_generation_key_at_all() {
    // `None` не пишется ни `null`, ни нулём: тик для стороны 1.9 — ровно прежняя форма.
    let written = encode_message(&clock_tick(None)).unwrap();
    assert!(!written.contains("generation"), "{written}");
    let decoded = decode_message::<PlayerSessionMessage>(written.as_bytes()).unwrap();
    assert!(
        matches!(
            decoded,
            PlayerSessionMessage::Clock {
                generation: None,
                ..
            }
        ),
        "{decoded:?}"
    );
}

#[test]
fn a_bad_clock_generation_is_rejected_with_the_exact_reason() {
    const PREFIX: &str = r#"{"type":"clock","seq":1,"position_us":0,"state":"playing","rate":1000"#;
    let cases = [
        // Одна форма «номера нет» — отсутствие ключа; `null` — значение не того типа.
        (r#","generation":null}"#, MessageErrorKind::WrongValueType),
        (r#","generation":"2"}"#, MessageErrorKind::WrongValueType),
        (r#","generation":1.5}"#, MessageErrorKind::WrongValueType),
        (r#","generation":true}"#, MessageErrorKind::WrongValueType),
        (
            r#","generation":{"number":2}}"#,
            MessageErrorKind::WrongValueType,
        ),
        // Поколения считаются с 1.
        (r#","generation":0}"#, MessageErrorKind::NumberOutOfRange),
        (r#","generation":-1}"#, MessageErrorKind::NumberOutOfRange),
        (
            r#","generation":4294967296}"#,
            MessageErrorKind::NumberOutOfRange,
        ),
        // Строгость формы поле не ослабило.
        (
            r#","generation":2,"generation":3}"#,
            MessageErrorKind::DuplicateField,
        ),
        (
            r#","generation":2,"source_generation":2}"#,
            MessageErrorKind::UnknownField,
        ),
    ];
    for (tail, kind) in cases {
        let json = format!("{PREFIX}{tail}");
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
fn only_a_tick_with_a_number_requires_1_10() {
    let [(without_number, _), with_numbers @ ..] = documented_ticks();
    assert_eq!(without_number.introduced_in(), ProtocolVersion::V1_0);
    assert_eq!(without_number.required_version(), ProtocolVersion::V1_0);
    for (tick, _) in with_numbers {
        // Тип старый, а форма с полем — новая.
        assert_eq!(tick.introduced_in(), ProtocolVersion::V1_0);
        assert_eq!(tick.required_version(), CLOCK_GENERATION_INTRODUCED_IN);
    }
    // Остальные сообщения плеера требуют ровно версию своего типа.
    let show_page = PlayerSessionMessage::ShowPage {};
    assert_eq!(show_page.required_version(), show_page.introduced_in());
}

#[test]
fn against_a_1_9_extension_the_clock_carries_no_generation() {
    // Плеер сверяет общую версию с `CLOCK_GENERATION_INTRODUCED_IN` ДО отправки: со стороной
    // 1.9 тик уходит без номера — старый экстеншен отверг бы незнакомое поле.
    for (player, extension) in [
        (ProtocolVersion::V1_10, ProtocolVersion::V1_9),
        (ProtocolVersion::V1_9, ProtocolVersion::V1_10),
    ] {
        let agreed = negotiate_protocol_version(player, extension).unwrap();
        assert_eq!(agreed, ProtocolVersion::V1_9);
        assert!(!agreed.includes(CLOCK_GENERATION_INTRODUCED_IN));
        assert!(!agreed.includes(clock_tick(Some(2)).required_version()));
        assert!(agreed.includes(clock_tick(None).required_version()));
    }
    let agreed =
        negotiate_protocol_version(ProtocolVersion::V1_10, ProtocolVersion::V1_10).unwrap();
    assert!(agreed.includes(CLOCK_GENERATION_INTRODUCED_IN));
}
