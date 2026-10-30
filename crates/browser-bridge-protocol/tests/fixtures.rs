//! Общие фикстуры протокола (`fixtures/manifest.json`): Rust-сторона договора.
//!
//! Те же файлы и те же ожидания проверяют тесты экстеншена на TypeScript
//! (сессия 07). Каждая фикстура:
//! - валидная — разбирается и записывается обратно **байт в байт**;
//! - невалидная — отвергается с **точным** видом причины.
//!
//! Записанные потоки (`fixtures/streams/`) проигрываются через сторожа
//! поколений, и принятые байты каждого SourceBuffer сверяются с исходным
//! медиафайлом.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use browser_bridge_protocol::limits::{MAX_JSON_MESSAGE_BYTES, MAX_JSON_SAFE_INTEGER};
use browser_bridge_protocol::{
    BridgeSoftware, CURRENT_PROTOCOL_VERSION, DataFrame, DataFrameHeader, ExtensionControlMessage,
    GateDecision, IncompatibleVersion, MessageErrorKind, PageSessionMessage, PeerCapabilities,
    PlayerControlMessage, PlayerSessionMessage, ProtocolMessage, StreamGate, decode_data_frame,
    decode_message, encode_data_frame, encode_message, negotiate_protocol_version,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    cases: Vec<FixtureCase>,
    streams: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureCase {
    file: String,
    schema: Schema,
    expect: Expectation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Schema {
    ExtensionControl,
    PlayerControl,
    PageSession,
    PlayerSession,
    DataFrame,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum Expectation {
    Ok {
        /// Только у двоичных рамок: ожидаемые поля заголовка.
        #[serde(default)]
        frame: Option<Value>,
    },
    Error {
        kind: String,
    },
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn load_manifest() -> Manifest {
    let text = std::fs::read_to_string(fixtures_root().join("manifest.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Имя вида ошибки так, как его пишут фикстуры (snake_case serde).
fn kind_name(kind: impl serde::Serialize) -> String {
    match serde_json::to_value(kind).unwrap() {
        Value::String(name) => name,
        other => panic!("вид ошибки должен быть строкой, а не {other}"),
    }
}

/// Проверка одной JSON-фикстуры вида `M`.
fn check_json_case<M>(bytes: &[u8], expect: &Expectation) -> Result<(), String>
where
    M: ProtocolMessage + PartialEq + Debug,
{
    match (decode_message::<M>(bytes), expect) {
        (Ok(message), Expectation::Ok { frame: None }) => {
            let written = encode_message(&message).map_err(|error| format!("запись: {error}"))?;
            if written.as_bytes() == bytes {
                Ok(())
            } else {
                Err(format!(
                    "обратно не байт в байт:\n  было  {}\n  стало {written}",
                    String::from_utf8_lossy(bytes)
                ))
            }
        }
        (Err(error), Expectation::Error { kind }) if kind_name(error.kind()) == *kind => Ok(()),
        (result, expect) => Err(format!("получено {result:?}, ожидалось {expect:?}")),
    }
}

/// Поля заголовка рамки в форме фикстур; 64-битные числа больше 2⁵³ − 1 — строкой.
fn frame_summary(frame: &DataFrame<'_>) -> Value {
    match frame.header {
        DataFrameHeader::SourceBufferBytes(header) => {
            let sequence = header.sequence.0;
            let sequence_value = if sequence <= MAX_JSON_SAFE_INTEGER {
                json!(sequence)
            } else {
                json!(sequence.to_string())
            };
            json!({
                "kind": "source_buffer_bytes",
                "generation": header.generation.get(),
                "source_buffer": header.source_buffer.get(),
                "sequence": sequence_value,
                "payload_len": frame.payload.len(),
            })
        }
        DataFrameHeader::RangeBytes(header) => json!({
            "kind": "range_bytes",
            "request_id": header.request_id.0,
            "offset": header.offset.as_bytes(),
            "payload_len": frame.payload.len(),
        }),
    }
}

fn check_data_frame_case(bytes: &[u8], expect: &Expectation) -> Result<(), String> {
    match (decode_data_frame(bytes), expect) {
        (
            Ok(frame),
            Expectation::Ok {
                frame: Some(expected),
            },
        ) => {
            let summary = frame_summary(&frame);
            if summary != *expected {
                return Err(format!("заголовок {summary}, ожидался {expected}"));
            }
            let written = encode_data_frame(&frame.header, frame.payload)
                .map_err(|error| format!("запись: {error}"))?;
            if written == bytes {
                Ok(())
            } else {
                Err("рамка обратно не байт в байт".to_owned())
            }
        }
        (Err(error), Expectation::Error { kind }) if kind_name(error) == *kind => Ok(()),
        (result, expect) => Err(format!("получено {result:?}, ожидалось {expect:?}")),
    }
}

fn check_case(case: &FixtureCase) -> Result<(), String> {
    let bytes =
        std::fs::read(fixtures_root().join(&case.file)).map_err(|error| error.to_string())?;
    match case.schema {
        Schema::ExtensionControl => {
            check_json_case::<ExtensionControlMessage>(&bytes, &case.expect)
        }
        Schema::PlayerControl => check_json_case::<PlayerControlMessage>(&bytes, &case.expect),
        Schema::PageSession => check_json_case::<PageSessionMessage>(&bytes, &case.expect),
        Schema::PlayerSession => check_json_case::<PlayerSessionMessage>(&bytes, &case.expect),
        Schema::DataFrame => check_data_frame_case(&bytes, &case.expect),
    }
}

#[test]
fn every_fixture_round_trips_or_is_rejected_with_the_exact_reason() {
    let manifest = load_manifest();
    assert!(
        manifest.cases.len() > 80,
        "фикстур подозрительно мало: {}",
        manifest.cases.len()
    );
    let failures: Vec<String> = manifest
        .cases
        .iter()
        .filter_map(|case| {
            check_case(case)
                .err()
                .map(|reason| format!("{}: {reason}", case.file))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "фикстуры не сошлись:\n{}",
        failures.join("\n")
    );
}

/// Значения `type` валидных фикстур одной схемы.
fn covered_types(manifest: &Manifest, schema: Schema) -> BTreeSet<String> {
    manifest
        .cases
        .iter()
        .filter(|case| case.schema == schema && matches!(case.expect, Expectation::Ok { .. }))
        .map(|case| {
            let text = std::fs::read_to_string(fixtures_root().join(&case.file)).unwrap();
            let message: Value = serde_json::from_str(&text).unwrap();
            message["type"].as_str().unwrap().to_owned()
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
fn every_known_message_type_has_a_valid_fixture_and_no_fixture_type_is_unlisted() {
    let manifest = load_manifest();
    // Совпадение множеств заодно доказывает, что KNOWN_TYPES не разошёлся с
    // вариантами перечислений: валидная фикстура с типом вне списка не разобралась бы.
    for (schema, known) in [
        (
            Schema::ExtensionControl,
            known_types::<ExtensionControlMessage>(),
        ),
        (Schema::PlayerControl, known_types::<PlayerControlMessage>()),
        (Schema::PageSession, known_types::<PageSessionMessage>()),
        (Schema::PlayerSession, known_types::<PlayerSessionMessage>()),
    ] {
        assert_eq!(covered_types(&manifest, schema), known, "{schema:?}");
    }
}

#[test]
fn every_error_kind_used_by_fixtures_is_a_real_kind_name() {
    let manifest = load_manifest();
    let json_kinds: BTreeSet<String> = manifest
        .cases
        .iter()
        .filter(|case| case.schema != Schema::DataFrame)
        .filter_map(|case| match &case.expect {
            Expectation::Error { kind } => Some(kind.clone()),
            Expectation::Ok { .. } => None,
        })
        .collect();
    for kind in &json_kinds {
        let parsed: Result<MessageErrorKind, _> = serde_json::from_value(json!(kind));
        assert!(
            parsed.is_ok(),
            "в фикстурах вид {kind:?}, которого нет в MessageErrorKind"
        );
    }
    // Ключевые виды отказа покрыты фикстурами (TypeScript опирается на эти же файлы).
    for required in [
        "unknown_type",
        "unknown_field",
        "missing_field",
        "duplicate_field",
        "syntax",
        "not_utf8",
    ] {
        assert!(json_kinds.contains(required), "нет фикстуры на {required}");
    }
}

#[test]
fn message_over_the_size_limit_is_rejected_and_one_at_the_limit_is_parsed() {
    let hello = br#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}"#;
    // Добиваем пробелами внутри объекта: JSON остаётся валидным при любой длине.
    let padded = |total: usize| {
        let mut message = hello[..hello.len() - 1].to_vec();
        message.resize(total - 1, b' ');
        message.push(b'}');
        message
    };
    let at_limit = padded(MAX_JSON_MESSAGE_BYTES);
    assert_eq!(at_limit.len(), MAX_JSON_MESSAGE_BYTES);
    assert!(decode_message::<ExtensionControlMessage>(&at_limit).is_ok());
    let over_limit = padded(MAX_JSON_MESSAGE_BYTES + 1);
    assert_eq!(
        decode_message::<ExtensionControlMessage>(&over_limit).map_err(|error| error.kind()),
        Err(MessageErrorKind::TooLarge)
    );
}

#[test]
fn newer_major_from_the_extension_parses_but_negotiates_to_update_the_player() {
    let bytes =
        std::fs::read(fixtures_root().join("control/valid/hello-extension-major-2.json")).unwrap();
    let ExtensionControlMessage::Hello(hello) = decode_message(&bytes).unwrap() else {
        panic!("фикстура должна быть hello");
    };
    assert_eq!(
        negotiate_protocol_version(CURRENT_PROTOCOL_VERSION, hello.protocol),
        Err(IncompatibleVersion {
            update: BridgeSoftware::Player
        })
    );
}

#[test]
fn player_hello_with_a_capability_from_the_future_keeps_known_ones() {
    let bytes = std::fs::read(fixtures_root().join("control/valid/hello-accepted.json")).unwrap();
    let PlayerControlMessage::HelloAccepted(accepted) = decode_message(&bytes).unwrap() else {
        panic!("фикстура должна быть hello_accepted");
    };
    let parsed = PeerCapabilities::from_tokens(&accepted.player.capabilities);
    assert_eq!(parsed.ignored_unknown_tokens, 1);
    assert_eq!(parsed.capabilities.cheap_video_codecs.len(), 1);
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedStream {
    steps: Vec<StreamStep>,
    expect: StreamExpectation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum StreamStep {
    Text(String),
    Binary(String),
    /// Темп записи: страница ждала часов плеера (сторож его не видит).
    AwaitClockUs(u64),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamExpectation {
    admitted: Vec<AdmittedBytes>,
    dropped_frames: usize,
    dropped_messages: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmittedBytes {
    generation: u32,
    source_buffer: u16,
    media: String,
    ranges: Vec<[usize; 2]>,
}

/// Итог проигрывания потока через сторожа.
#[derive(Debug, Default)]
struct ReplayOutcome {
    admitted: BTreeMap<(u32, u16), Vec<u8>>,
    dropped_frames: usize,
    dropped_messages: usize,
    /// Позиция часов, которую ждала последняя пауза записи.
    last_awaited_clock_us: Option<u64>,
}

fn replay(directory: &Path, steps: &[StreamStep]) -> ReplayOutcome {
    let mut gate = StreamGate::new();
    let mut outcome = ReplayOutcome::default();
    for step in steps {
        match step {
            StreamStep::Text(file) => {
                let bytes = std::fs::read(directory.join(file)).unwrap();
                let message: PageSessionMessage = decode_message(&bytes).unwrap();
                // Шаги потока тоже канонические: обратно байт в байт.
                assert_eq!(
                    encode_message(&message).unwrap().as_bytes(),
                    bytes,
                    "{file}"
                );
                if gate.apply_message(&message).unwrap() == GateDecision::DropStale {
                    outcome.dropped_messages += 1;
                }
            }
            StreamStep::Binary(file) => {
                let bytes = std::fs::read(directory.join(file)).unwrap();
                let frame = decode_data_frame(&bytes).unwrap();
                let DataFrameHeader::SourceBufferBytes(header) = frame.header else {
                    panic!("{file}: в записанных потоках только байты SourceBuffer");
                };
                match gate.admit_source_buffer_bytes(&header).unwrap() {
                    GateDecision::Accept => outcome
                        .admitted
                        .entry((header.generation.get(), header.source_buffer.get()))
                        .or_default()
                        .extend_from_slice(frame.payload),
                    GateDecision::DropStale => outcome.dropped_frames += 1,
                }
            }
            // Не провод: только пауза проигрывателя записи. Часы плеера не идут назад,
            // поэтому каждая пауза записи ждёт позицию дальше прежней.
            StreamStep::AwaitClockUs(position_us) => {
                assert!(
                    outcome.last_awaited_clock_us < Some(*position_us),
                    "recording pauses must wait for a later clock: {position_us}"
                );
                outcome.last_awaited_clock_us = Some(*position_us);
            }
        }
    }
    outcome
}

#[test]
fn recorded_streams_reassemble_source_buffer_bytes_exactly_and_drop_only_stale_ones() {
    let manifest = load_manifest();
    assert_eq!(manifest.streams.len(), 5);
    for name in &manifest.streams {
        let directory = fixtures_root().join("streams").join(name);
        let recorded: RecordedStream =
            serde_json::from_str(&std::fs::read_to_string(directory.join("stream.json")).unwrap())
                .unwrap();
        let outcome = replay(&directory, &recorded.steps);

        let mut expected = BTreeMap::new();
        for admitted in &recorded.expect.admitted {
            let media = std::fs::read(fixtures_root().join("media").join(&admitted.media)).unwrap();
            let bytes: Vec<u8> = admitted
                .ranges
                .iter()
                .flat_map(|[start, end]| media[*start..*end].iter().copied())
                .collect();
            expected.insert((admitted.generation, admitted.source_buffer), bytes);
        }
        assert_eq!(
            outcome.admitted.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "{name}: набор SourceBuffer"
        );
        for (key, bytes) in &expected {
            assert!(
                outcome.admitted[key] == *bytes,
                "{name} {key:?}: байты не совпали с медиафайлом"
            );
        }
        assert_eq!(
            outcome.dropped_frames, recorded.expect.dropped_frames,
            "{name}: рамки"
        );
        assert_eq!(
            outcome.dropped_messages, recorded.expect.dropped_messages,
            "{name}: сообщения"
        );
    }
}
