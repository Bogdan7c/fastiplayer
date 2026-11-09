//! Договоры протокола 1.5 (сессия 13b1, решения 270–274 — субтитры страницы), которых не
//! покрывает общий проход по `manifest.json`:
//! - одно новое сообщение `subtitle_track_offered` (страница → плеер) — версия в
//!   `message-versions.json`, и новый вид двоичной рамки 3 — версия у `DataFrameHeader`;
//! - сообщение пишется ровно задокументированным JSON, рамка — ровно задокументированными
//!   байтами, и оба читаются обратно;
//! - правила сторожа — общие случаи `fixtures/page-subtitle-gate.json` (каждое правило
//!   договора — свой случай, принятые и отвергнутые последовательности);
//! - живой сценарий: документ больше рамки уходит кусками ≤ 1 МиБ и собирается обратно
//!   байт в байт, при общей версии 1.4 субтитров нет ни у сторожа, ни у договора версий.
//!
//! Те же форматы проверяет экстеншен (`extensions/browser/tests/protocol/protocol-1-5.test.ts`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use browser_bridge_protocol::limits::{
    DATA_FRAME_HEADER_BYTES, MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_PAGE_SUBTITLE_DOCUMENT_BYTES,
};
use browser_bridge_protocol::session::{
    PAGE_SUBTITLES_INTRODUCED_IN, PageSubtitleByteLength, PageSubtitleDisplay, PageSubtitleLabel,
    PageSubtitleLanguage, PageSubtitleOrigin, PageSubtitleTrackId, StreamScope, SubtitleFormatHint,
};
use browser_bridge_protocol::{
    ByteOffset, CURRENT_PROTOCOL_VERSION, DataFrameHeader, GateDecision, PageSessionMessage,
    PageSubtitleBytesHeader, ProtocolVersion, RangeBytesHeader, RangeRequestId, StreamGate,
    StreamViolation, SubtitleLabel, SubtitleLanguageTag, decode_data_frame, decode_message,
    encode_data_frame, encode_message, negotiate_protocol_version,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageVersions {
    page_session: BTreeMap<String, String>,
    player_session: BTreeMap<String, String>,
}

/// Общие случаи сторожа (`generate_fixtures.py`, `write_page_subtitle_gate_cases`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateCases {
    cases: Vec<GateCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateCase {
    name: String,
    agreed: String,
    steps: Vec<GateStep>,
    expect: GateExpectation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum GateStep {
    /// Текстовая рамка: ровно байты сообщения на проводе.
    Text(String),
    /// Двоичная рамка шестнадцатеричной строкой.
    BinaryHex(String),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum GateExpectation {
    /// Все шаги приняты; готовые документы — ровно эти.
    Ok { documents: Vec<ExpectedDocument> },
    /// Шаг `at_step` — нарушение `violation`, все до него приняты.
    Violation { at_step: usize, violation: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedDocument {
    track: u32,
    utf8: String,
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Версия из записи фикстуры `"1.5"`.
fn version_of(text: &str) -> ProtocolVersion {
    let (major, minor) = text.split_once('.').unwrap();
    ProtocolVersion {
        major: major.parse().unwrap(),
        minor: minor.parse().unwrap(),
    }
}

/// Имя нарушения в словаре фикстур (snake_case варианта `StreamViolation`).
///
/// Сопоставление исчерпывающее: новое нарушение без имени здесь не соберётся.
const fn violation_name(violation: StreamViolation) -> &'static str {
    match violation {
        StreamViolation::GenerationOutOfOrder => "generation_out_of_order",
        StreamViolation::GenerationNotAnnounced => "generation_not_announced",
        StreamViolation::SourceBufferNotAnnounced => "source_buffer_not_announced",
        StreamViolation::SourceBufferAlreadyAnnounced => "source_buffer_already_announced",
        StreamViolation::FileSourceAlreadyAnnounced => "file_source_already_announced",
        StreamViolation::FileSourceAfterSourceBuffer => "file_source_after_source_buffer",
        StreamViolation::SourceBufferInFileGeneration => "source_buffer_in_file_generation",
        StreamViolation::MediaSourceMessageInFileGeneration => {
            "media_source_message_in_file_generation"
        }
        StreamViolation::HlsSourceAlreadyAnnounced => "hls_source_already_announced",
        StreamViolation::HlsSourceAfterSourceBuffer => "hls_source_after_source_buffer",
        StreamViolation::HlsSourceInFileGeneration => "hls_source_in_file_generation",
        StreamViolation::FileSourceInHlsGeneration => "file_source_in_hls_generation",
        StreamViolation::SourceBufferInHlsGeneration => "source_buffer_in_hls_generation",
        StreamViolation::MediaSourceMessageInHlsGeneration => {
            "media_source_message_in_hls_generation"
        }
        StreamViolation::PageIsPresent => "page_is_present",
        StreamViolation::PageIsAway => "page_is_away",
        StreamViolation::MessageNotInAgreedVersion { .. } => "message_not_in_agreed_version",
        StreamViolation::DataFrameNotInAgreedVersion { .. } => "data_frame_not_in_agreed_version",
        StreamViolation::TooManyPageSubtitleTracks => "too_many_page_subtitle_tracks",
        StreamViolation::PageSubtitleTrackNotOffered => "page_subtitle_track_not_offered",
        StreamViolation::PageSubtitleDocumentComplete => "page_subtitle_document_complete",
        StreamViolation::PageSubtitleOffsetGap { .. } => "page_subtitle_offset_gap",
        StreamViolation::PageSubtitleBeyondByteLength => "page_subtitle_beyond_byte_length",
        StreamViolation::SequenceGap { .. } => "sequence_gap",
    }
}

/// Документ дорожки, как его собирает получатель: объявленная длина и пришедшие байты.
#[derive(Debug, Default)]
struct AssembledDocument {
    declared_bytes: u64,
    received: Vec<u8>,
}

/// Проигрывание шагов через сторожа и сборка документов по принятым рамкам.
#[derive(Debug, Default)]
struct GateReplay {
    documents: BTreeMap<u32, AssembledDocument>,
}

impl GateReplay {
    /// Один шаг; `Err` — нарушение сторожа.
    fn step(&mut self, gate: &mut StreamGate, step: &GateStep) -> Result<(), StreamViolation> {
        match step {
            GateStep::Text(text) => {
                let message: PageSessionMessage = decode_message(text.as_bytes()).unwrap();
                // Шаги канонические: обратно байт в байт.
                assert_eq!(encode_message(&message).unwrap(), *text);
                gate.apply_message(&message)?;
                if let PageSessionMessage::SubtitleTrackOffered {
                    track, byte_length, ..
                } = message
                {
                    // Новое предложение — замена: прежние байты дорожки забываются.
                    self.documents.insert(
                        track.get(),
                        AssembledDocument {
                            declared_bytes: byte_length.as_bytes(),
                            received: Vec::new(),
                        },
                    );
                }
                Ok(())
            }
            GateStep::BinaryHex(hex) => {
                let bytes = decode_hex(hex);
                let frame = decode_data_frame(&bytes).unwrap();
                match frame.header {
                    DataFrameHeader::PageSubtitleBytes(header) => {
                        let decision = gate.admit_page_subtitle_bytes(&header, frame.payload)?;
                        assert_eq!(decision, GateDecision::Accept);
                        self.documents
                            .get_mut(&header.track.get())
                            .expect("сторож принял байты непредложенной дорожки")
                            .received
                            .extend_from_slice(frame.payload);
                    }
                    DataFrameHeader::SourceBufferBytes(header) => {
                        gate.admit_source_buffer_bytes(&header)?;
                    }
                    DataFrameHeader::RangeBytes(_) => {
                        gate.admit_range_bytes()?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Готовые документы: пришло ровно объявленное число байт.
    fn complete_documents(&self) -> BTreeMap<u32, String> {
        self.documents
            .iter()
            .filter(|(_, document)| {
                u64::try_from(document.received.len()).unwrap() == document.declared_bytes
            })
            .map(|(track, document)| {
                (
                    *track,
                    String::from_utf8(document.received.clone()).unwrap(),
                )
            })
            .collect()
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "нечётная длина hex");
    (0..hex.len())
        .step_by(2)
        .map(|start| u8::from_str_radix(&hex[start..start + 2], 16).unwrap())
        .collect()
}

fn track(number: u32) -> PageSubtitleTrackId {
    PageSubtitleTrackId::new(number).unwrap()
}

fn offer(number: u32, byte_length: u64) -> PageSessionMessage {
    PageSessionMessage::SubtitleTrackOffered {
        track: track(number),
        language: PageSubtitleLanguage::Known {
            tag: SubtitleLanguageTag::new("uk").unwrap(),
        },
        label: PageSubtitleLabel::Text {
            text: SubtitleLabel::new("Українська").unwrap(),
        },
        format_hint: SubtitleFormatHint::Json3,
        origin: PageSubtitleOrigin::PageDownload,
        page_display: PageSubtitleDisplay::Showing,
        byte_length: PageSubtitleByteLength::new(byte_length).unwrap(),
    }
}

#[test]
fn protocol_1_5_adds_exactly_one_documented_message_type() {
    let table: MessageVersions = read_json("message-versions.json");
    let newer_page: BTreeSet<&str> = table
        .page_session
        .iter()
        .filter(|(_, version)| version.as_str() == "1.5")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(newer_page, BTreeSet::from(["subtitle_track_offered"]));
    assert!(
        table
            .player_session
            .values()
            .all(|version| version.as_str() != "1.5"),
        "плееру в 1.5 новых сообщений нет"
    );
    assert_eq!(
        version_of(&table.page_session["subtitle_track_offered"]),
        PAGE_SUBTITLES_INTRODUCED_IN
    );
    assert_eq!(PAGE_SUBTITLES_INTRODUCED_IN, CURRENT_PROTOCOL_VERSION);
}

#[test]
fn the_offer_needs_1_5_and_is_not_bound_to_a_stream_generation() {
    let message = offer(1, 10);
    assert_eq!(message.introduced_in(), ProtocolVersion::V1_5);
    assert_eq!(message.required_version(), ProtocolVersion::V1_5);
    assert_eq!(message.stream_scope(), StreamScope::Unbound);
}

#[test]
fn only_the_subtitle_frame_kind_is_new_in_1_5() {
    let offset = ByteOffset::new(0).unwrap();
    let subtitle = DataFrameHeader::PageSubtitleBytes(PageSubtitleBytesHeader {
        track: track(1),
        offset,
    });
    let range = DataFrameHeader::RangeBytes(RangeBytesHeader {
        request_id: RangeRequestId(1),
        offset,
    });
    assert_eq!(subtitle.introduced_in(), ProtocolVersion::V1_5);
    assert_eq!(range.introduced_in(), ProtocolVersion::V1_0);
}

#[test]
fn negotiating_1_5_against_1_4_gives_1_4_in_both_roles() {
    assert_eq!(
        negotiate_protocol_version(ProtocolVersion::V1_5, ProtocolVersion::V1_4),
        Ok(ProtocolVersion::V1_4)
    );
    assert_eq!(
        negotiate_protocol_version(ProtocolVersion::V1_4, ProtocolVersion::V1_5),
        Ok(ProtocolVersion::V1_4)
    );
}

#[test]
fn the_offer_encodes_to_the_exact_documented_json_and_parses_back() {
    let documented = concat!(
        r#"{"type":"subtitle_track_offered","track":7,"#,
        r#""language":{"kind":"known","tag":"uk"},"#,
        r#""label":{"kind":"text","text":"Українська"},"#,
        r#""format_hint":"json3","origin":"page_download","page_display":"showing","#,
        r#""byte_length":4096}"#
    );
    let message = offer(7, 4096);
    assert_eq!(encode_message(&message).unwrap(), documented);
    assert_eq!(
        decode_message::<PageSessionMessage>(documented.as_bytes()).unwrap(),
        message
    );
    // Неизвестный язык и нет подписи — метки, а не пропавшие поля.
    let anonymous = PageSessionMessage::SubtitleTrackOffered {
        track: track(2),
        language: PageSubtitleLanguage::Unknown {},
        label: PageSubtitleLabel::None {},
        format_hint: SubtitleFormatHint::WebVtt,
        origin: PageSubtitleOrigin::TextTrackCues,
        page_display: PageSubtitleDisplay::NotShowing,
        byte_length: PageSubtitleByteLength::new(1).unwrap(),
    };
    assert_eq!(
        encode_message(&anonymous).unwrap(),
        concat!(
            r#"{"type":"subtitle_track_offered","track":2,"language":{"kind":"unknown"},"#,
            r#""label":{"kind":"none"},"format_hint":"web_vtt","origin":"text_track_cues","#,
            r#""page_display":"not_showing","byte_length":1}"#
        )
    );
}

#[test]
fn the_subtitle_frame_has_the_documented_byte_layout() {
    let header = DataFrameHeader::PageSubtitleBytes(PageSubtitleBytesHeader {
        track: track(0x0102_0304),
        offset: ByteOffset::new(0x0506_0708_090a).unwrap(),
    });
    let frame = encode_data_frame(&header, b"WEBVTT").unwrap();
    let mut expected = vec![3_u8, 0, 0, 0, 0x04, 0x03, 0x02, 0x01];
    expected.extend_from_slice(&0x0506_0708_090a_u64.to_le_bytes());
    expected.extend_from_slice(&[0; 8]);
    expected.extend_from_slice(b"WEBVTT");
    assert_eq!(frame, expected);
    assert_eq!(frame.len(), DATA_FRAME_HEADER_BYTES + 6);
    let parsed = decode_data_frame(&frame).unwrap();
    assert_eq!(
        (parsed.header, parsed.payload),
        (header, b"WEBVTT".as_slice())
    );
}

#[test]
fn every_shared_gate_case_is_accepted_or_rejected_exactly_as_the_fixture_says() {
    let cases: GateCases = read_json("page-subtitle-gate.json");
    assert!(
        cases.cases.len() >= 15,
        "случаев сторожа меньше, чем правил"
    );
    for case in &cases.cases {
        let mut gate = StreamGate::for_agreed_version(version_of(&case.agreed));
        let mut replay = GateReplay::default();
        let outcome = case
            .steps
            .iter()
            .enumerate()
            .find_map(|(index, step)| replay.step(&mut gate, step).err().map(|v| (index, v)));
        match (&case.expect, outcome) {
            (GateExpectation::Ok { documents }, None) => {
                let expected: BTreeMap<u32, String> = documents
                    .iter()
                    .map(|document| (document.track, document.utf8.clone()))
                    .collect();
                assert_eq!(replay.complete_documents(), expected, "{}", case.name);
            }
            (
                GateExpectation::Violation { at_step, violation },
                Some((index, received_violation)),
            ) => {
                assert_eq!(
                    (index, violation_name(received_violation)),
                    (*at_step, violation.as_str()),
                    "{}",
                    case.name
                );
            }
            (expect, outcome) => {
                panic!("{}: ожидалось {expect:?}, получено {outcome:?}", case.name)
            }
        }
    }
}

#[test]
fn a_document_larger_than_a_frame_travels_in_pieces_and_is_reassembled_byte_for_byte() {
    // 2,5 МиБ: три рамки (1 МиБ, 1 МиБ, 0,5 МиБ) — так шлёт экстеншен (`splitPageSubtitleDocument`).
    let document: Vec<u8> = (0..MAX_DATA_FRAME_PAYLOAD_BYTES * 5 / 2)
        .map(|index| b"WEBVTT\n"[index % 7])
        .collect();
    let byte_length = u64::try_from(document.len()).unwrap();
    assert!(byte_length <= MAX_PAGE_SUBTITLE_DOCUMENT_BYTES);
    let mut gate = StreamGate::new();
    assert_eq!(
        gate.apply_message(&offer(3, byte_length)),
        Ok(GateDecision::Accept)
    );
    let mut reassembled = Vec::new();
    let mut frames = 0;
    for (index, piece) in document.chunks(MAX_DATA_FRAME_PAYLOAD_BYTES).enumerate() {
        let header = DataFrameHeader::PageSubtitleBytes(PageSubtitleBytesHeader {
            track: track(3),
            offset: ByteOffset::new(u64::try_from(index * MAX_DATA_FRAME_PAYLOAD_BYTES).unwrap())
                .unwrap(),
        });
        let wire = encode_data_frame(&header, piece).unwrap();
        let frame = decode_data_frame(&wire).unwrap();
        let DataFrameHeader::PageSubtitleBytes(parsed) = frame.header else {
            panic!("рамка вида 3 должна разбираться видом 3");
        };
        assert_eq!(
            gate.admit_page_subtitle_bytes(&parsed, frame.payload),
            Ok(GateDecision::Accept)
        );
        reassembled.extend_from_slice(frame.payload);
        frames += 1;
    }
    assert_eq!(frames, 3);
    assert_eq!(reassembled, document);
    // Документ готов: ещё байт — нарушение.
    let extra = PageSubtitleBytesHeader {
        track: track(3),
        offset: ByteOffset::new(byte_length).unwrap(),
    };
    assert_eq!(
        gate.admit_page_subtitle_bytes(&extra, b"x"),
        Err(StreamViolation::PageSubtitleDocumentComplete)
    );
}
