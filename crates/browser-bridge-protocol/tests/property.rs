//! Property-тесты протокола на своём детерминированном генераторе.
//!
//! `proptest` в дереве нет; по образцу `ac3-decoder` (xorshift) генератор свой:
//! без новой зависимости, а упавший прогон повторяется точно по зерну.
//!
//! Свойства:
//! 1. случайное валидное сообщение → запись → разбор = исходное;
//! 2. случайные байты, обрезки и порча валидных фикстур → только
//!    типизированная ошибка или валидное сообщение, без паники;
//! 3. сторож поколений на случайных последовательностях принимает байты
//!    только текущего поколения и только подряд идущими номерами.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;

use browser_bridge_protocol::control::{
    ControlRefusal, ControlRequestId, GrantOutcome, Hello, HelloAccepted, LoopbackPort,
    OpenSessionRequest, PageIdentity, PlaybackHints, PositionHint, RefusalReason, ReplyTarget,
    SessionGrant, SessionIntent,
};
use browser_bridge_protocol::limits::{
    MAX_DATA_FRAME_PAYLOAD_BYTES, MAX_JSON_SAFE_INTEGER, MAX_PLAYBACK_RATE_PERMILLE,
    MAX_RANGE_FETCH_BYTES, MIN_PLAYBACK_RATE_PERMILLE,
};
use browser_bridge_protocol::session::{
    BufferIntake, ClockSequence, ClockState, PagePlaybackRequest, RangeFetchFailure,
    RangeFetchOutcome, RemovalEnd, ResourceSize, ResumeState, SeekCause, StreamRejection,
    TabGoneReason, VariantKind, VariantMenu, VariantOption, VariantSelection,
};
use browser_bridge_protocol::{
    AppendSequence, BridgeSoftware, ByteOffset, CapabilityToken, DataFrameHeader,
    ExtensionControlMessage, GateDecision, MediaDuration, MediaTime, MediaTimeOffset,
    MessageErrorKind, MimeType, PageSessionMessage, PageTitle, PlaybackRate, PlayerControlMessage,
    PlayerSessionMessage, ProtocolMessage, ProtocolVersion, RangeBytesHeader, RangeFetchLength,
    RangeRequestId, SeekId, SessionKey, SessionToken, SiteDomain, SoftwareVersion,
    SourceBufferBytesHeader, SourceBufferIndex, StreamGate, StreamGeneration, VariantId,
    VariantLabel, decode_data_frame, decode_message, encode_data_frame, encode_message,
};

/// Детерминированный генератор xorshift64*.
struct Generator(u64);

impl Generator {
    fn new(seed: u64) -> Self {
        // Нулевое состояние у xorshift вырождается — подменяем константой.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Число в `0..bound` (bound > 0).
    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    /// Число в `low..=high`.
    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }

    fn pick<'item, T>(&mut self, items: &'item [T]) -> &'item T {
        &items[self.below(items.len() as u64) as usize]
    }

    fn coin(&mut self) -> bool {
        self.below(2) == 0
    }
}

/// Строка из символов пула, не длиннее `max_bytes` байт UTF-8.
fn text_from_pool(
    generator: &mut Generator,
    pool: &[char],
    min_chars: u64,
    max_bytes: usize,
) -> String {
    let wanted_chars = generator.between(min_chars, 40);
    let mut text = String::new();
    for _ in 0..wanted_chars {
        let character = *generator.pick(pool);
        if text.len() + character.len_utf8() > max_bytes {
            break;
        }
        text.push(character);
    }
    text
}

/// Текст для человека: кириллица, эмодзи, арабский, иврит, CJK, метки направления.
const DISPLAY_POOL: &[char] = &[
    'a', 'Z', '7', ' ', '—', 'Ш', 'ї', 'є', 'ґ', '🎬', '👨', '\u{200D}', '👩', 'م', 'ر', 'ש', 'ל',
    '日', '本', '\u{200E}', '\u{200F}', '"', '\\', '/', '<', '{', '\u{FEFF}', 'é',
];
const OPAQUE_POOL: &[char] = &['a', 'z', 'A', 'Z', '0', '9', '-', '_'];
const DOMAIN_POOL: &[char] = &['a', 'q', 'z', '0', '5', '-', '.'];
const MIME_POOL: &[char] = &[
    'v', 'i', 'd', 'e', 'o', '/', ';', ' ', '=', '"', ',', '.', '1', '~',
];

fn page_title(generator: &mut Generator) -> PageTitle {
    PageTitle::new(text_from_pool(generator, DISPLAY_POOL, 0, 1024)).unwrap()
}

fn variant_label(generator: &mut Generator) -> VariantLabel {
    VariantLabel::new(text_from_pool(generator, DISPLAY_POOL, 1, 256))
        .unwrap_or_else(|_| VariantLabel::new("x").unwrap())
}

fn session_key(generator: &mut Generator) -> SessionKey {
    SessionKey::new(text_from_pool(generator, OPAQUE_POOL, 1, 64)).unwrap()
}

fn variant_id(generator: &mut Generator, ordinal: usize) -> VariantId {
    // Порядковый номер в конце делает ID уникальными внутри меню.
    let prefix = text_from_pool(generator, OPAQUE_POOL, 1, 32);
    VariantId::new(format!("{prefix}-{ordinal}")).unwrap()
}

fn session_token(generator: &mut Generator) -> SessionToken {
    let length = generator.between(32, 128) as usize;
    let token: String = (0..length).map(|_| *generator.pick(OPAQUE_POOL)).collect();
    SessionToken::new(token).unwrap()
}

fn site_domain(generator: &mut Generator) -> SiteDomain {
    SiteDomain::new(text_from_pool(generator, DOMAIN_POOL, 1, 253)).unwrap()
}

fn mime_type(generator: &mut Generator) -> MimeType {
    MimeType::new(text_from_pool(generator, MIME_POOL, 1, 256)).unwrap()
}

fn media_time(generator: &mut Generator) -> MediaTime {
    let micros = match generator.below(4) {
        0 => 0,
        1 => MAX_JSON_SAFE_INTEGER,
        _ => generator.below(MAX_JSON_SAFE_INTEGER + 1),
    };
    MediaTime::from_micros(micros).unwrap()
}

fn media_offset(generator: &mut Generator) -> MediaTimeOffset {
    let magnitude = generator.below(MAX_JSON_SAFE_INTEGER + 1) as i64;
    MediaTimeOffset::from_micros(if generator.coin() {
        magnitude
    } else {
        -magnitude
    })
    .unwrap()
}

fn media_duration(generator: &mut Generator) -> MediaDuration {
    match generator.below(3) {
        0 => MediaDuration::Unknown {},
        1 => MediaDuration::Live {},
        _ => MediaDuration::Finite {
            duration_us: media_time(generator),
        },
    }
}

fn playback_rate(generator: &mut Generator) -> PlaybackRate {
    let permille = generator.between(
        u64::from(MIN_PLAYBACK_RATE_PERMILLE),
        u64::from(MAX_PLAYBACK_RATE_PERMILLE),
    );
    PlaybackRate::from_permille(permille as u32).unwrap()
}

fn generation(generator: &mut Generator) -> StreamGeneration {
    StreamGeneration::new(generator.between(1, u64::from(u32::MAX)) as u32).unwrap()
}

fn source_buffer(generator: &mut Generator) -> SourceBufferIndex {
    SourceBufferIndex::new(generator.below(8) as u16).unwrap()
}

fn variant_kind(generator: &mut Generator) -> VariantKind {
    *generator.pick(&[
        VariantKind::Audio,
        VariantKind::Quality,
        VariantKind::Subtitles,
    ])
}

fn variant_menu(generator: &mut Generator) -> VariantMenu {
    let count = generator.below(12) as usize;
    let selected = generator.below(count as u64 + 1) as usize; // == count — ничего не выбрано
    let options = (0..count)
        .map(|ordinal| VariantOption {
            id: variant_id(generator, ordinal),
            label: variant_label(generator),
            selection: if ordinal == selected {
                VariantSelection::Selected
            } else {
                VariantSelection::Available
            },
        })
        .collect();
    VariantMenu {
        kind: variant_kind(generator),
        options,
    }
}

fn random_page_message(generator: &mut Generator) -> PageSessionMessage {
    let generation = generation(generator);
    let source_buffer = source_buffer(generator);
    match generator.below(14) {
        0 => PageSessionMessage::Attach {
            session_key: session_key(generator),
            token: session_token(generator),
        },
        1 => PageSessionMessage::GenerationStarted { generation },
        2 => PageSessionMessage::SourceBufferAdded {
            generation,
            source_buffer,
            mime_type: mime_type(generator),
        },
        3 => PageSessionMessage::SourceBufferTypeChanged {
            generation,
            source_buffer,
            mime_type: mime_type(generator),
        },
        4 => PageSessionMessage::TimestampOffsetChanged {
            generation,
            source_buffer,
            offset_us: media_offset(generator),
        },
        5 => {
            let start = generator.below(MAX_JSON_SAFE_INTEGER);
            let end = if generator.coin() {
                RemovalEnd::EndOfTimeline {}
            } else {
                RemovalEnd::At {
                    time_us: MediaTime::from_micros(
                        generator.between(start + 1, MAX_JSON_SAFE_INTEGER),
                    )
                    .unwrap(),
                }
            };
            PageSessionMessage::SourceBufferRemoved {
                generation,
                source_buffer,
                start_us: MediaTime::from_micros(start).unwrap(),
                end,
            }
        }
        6 => PageSessionMessage::SourceBufferAborted {
            generation,
            source_buffer,
        },
        7 => PageSessionMessage::SeekStarted {
            target_us: media_time(generator),
            cause: if generator.coin() {
                SeekCause::PageSeek {}
            } else {
                SeekCause::PlayerSeek {
                    seek_id: SeekId(generator.next_u64() as u32),
                }
            },
        },
        8 => PageSessionMessage::DurationChanged {
            generation,
            duration: media_duration(generator),
        },
        9 => PageSessionMessage::EndOfStream { generation },
        10 => PageSessionMessage::PagePlayback {
            request: match generator.below(3) {
                0 => PagePlaybackRequest::Play {},
                1 => PagePlaybackRequest::Pause {},
                _ => PagePlaybackRequest::SetRate {
                    rate: playback_rate(generator),
                },
            },
        },
        11 => PageSessionMessage::VariantMenuChanged {
            menu: variant_menu(generator),
        },
        12 => PageSessionMessage::RangeFetchFinished {
            request_id: RangeRequestId(generator.next_u64() as u32),
            outcome: if generator.coin() {
                RangeFetchOutcome::Failed {
                    failure: *generator.pick(&[
                        RangeFetchFailure::NetworkError,
                        RangeFetchFailure::HttpError,
                        RangeFetchFailure::ResourceChanged,
                        RangeFetchFailure::OutOfRange,
                    ]),
                }
            } else {
                RangeFetchOutcome::Completed {
                    resource_size: if generator.coin() {
                        ResourceSize::Unknown {}
                    } else {
                        ResourceSize::Known {
                            size_bytes: ByteOffset::new(generator.below(MAX_JSON_SAFE_INTEGER + 1))
                                .unwrap(),
                        }
                    },
                }
            },
        },
        _ => PageSessionMessage::TabGone {
            reason: *generator.pick(&[
                TabGoneReason::Closed,
                TabGoneReason::NavigatedAway,
                TabGoneReason::BrowserClosing,
            ]),
        },
    }
}

fn random_player_message(generator: &mut Generator) -> PlayerSessionMessage {
    match generator.below(12) {
        0 => PlayerSessionMessage::Attached {},
        1 => PlayerSessionMessage::Clock {
            seq: ClockSequence(generator.next_u64() as u32),
            position_us: media_time(generator),
            state: *generator.pick(&[
                ClockState::Playing,
                ClockState::Paused,
                ClockState::Waiting,
                ClockState::Ended,
            ]),
            rate: playback_rate(generator),
        },
        2 => PlayerSessionMessage::BufferState {
            generation: generation(generator),
            ahead_us: media_time(generator),
            intake: *generator.pick(&[BufferIntake::Accepting, BufferIntake::Full]),
        },
        3 => PlayerSessionMessage::Play {},
        4 => PlayerSessionMessage::Pause {},
        5 => PlayerSessionMessage::Seek {
            seek_id: SeekId(generator.next_u64() as u32),
            target_us: media_time(generator),
        },
        6 => PlayerSessionMessage::SetRate {
            rate: playback_rate(generator),
        },
        7 => PlayerSessionMessage::ReturnToPage {
            position_us: media_time(generator),
            resume: *generator.pick(&[ResumeState::Playing, ResumeState::Paused]),
        },
        8 => PlayerSessionMessage::SelectVariant {
            kind: variant_kind(generator),
            variant_id: variant_id(generator, 0),
        },
        9 => PlayerSessionMessage::NextEpisode {},
        10 => PlayerSessionMessage::FetchRange {
            request_id: RangeRequestId(generator.next_u64() as u32),
            offset: ByteOffset::new(generator.below(MAX_JSON_SAFE_INTEGER + 1)).unwrap(),
            length: RangeFetchLength::new(generator.between(1, MAX_RANGE_FETCH_BYTES)).unwrap(),
        },
        _ => PlayerSessionMessage::StreamRejected {
            generation: generation(generator),
            reason: *generator.pick(&[
                StreamRejection::ProtectedContent,
                StreamRejection::UnsupportedCodec,
                StreamRejection::UnparsableMedia,
                StreamRejection::FileUnavailable,
                StreamRejection::PlaybackFailed,
            ]),
        },
    }
}

fn protocol_version(generator: &mut Generator) -> ProtocolVersion {
    ProtocolVersion {
        major: generator.next_u64() as u16,
        minor: generator.next_u64() as u16,
    }
}

fn hello(generator: &mut Generator) -> Hello {
    let count = generator.below(6);
    Hello {
        protocol: protocol_version(generator),
        software_version: SoftwareVersion::new(format!(
            "{}.{}.{}",
            generator.below(9),
            generator.below(99),
            generator.below(999)
        ))
        .unwrap(),
        capabilities: (0..count)
            .map(|ordinal| CapabilityToken::new(format!("feature.f{ordinal}")).unwrap())
            .collect(),
    }
}

fn random_extension_control(generator: &mut Generator) -> ExtensionControlMessage {
    if generator.coin() {
        return ExtensionControlMessage::Hello(hello(generator));
    }
    ExtensionControlMessage::OpenSession(OpenSessionRequest {
        request_id: ControlRequestId(generator.next_u64() as u32),
        session_key: session_key(generator),
        page: PageIdentity {
            title: page_title(generator),
            domain: site_domain(generator),
        },
        intent: *generator.pick(&[
            SessionIntent::Window,
            SessionIntent::PictureInPicture,
            SessionIntent::InPage,
        ]),
        hints: PlaybackHints {
            duration: media_duration(generator),
            position: if generator.coin() {
                PositionHint::Unknown {}
            } else {
                PositionHint::At {
                    position_us: media_time(generator),
                }
            },
        },
    })
}

fn random_player_control(generator: &mut Generator) -> PlayerControlMessage {
    match generator.below(3) {
        0 => PlayerControlMessage::HelloAccepted(HelloAccepted {
            agreed_protocol: protocol_version(generator),
            player: hello(generator),
        }),
        1 => PlayerControlMessage::SessionGranted(SessionGrant {
            request_id: ControlRequestId(generator.next_u64() as u32),
            session_key: session_key(generator),
            port: LoopbackPort::new(generator.between(1, u64::from(u16::MAX)) as u16).unwrap(),
            token: session_token(generator),
            outcome: *generator.pick(&[
                GrantOutcome::Started,
                GrantOutcome::ReusedExisting,
                GrantOutcome::ReplacedPrevious,
            ]),
        }),
        _ => PlayerControlMessage::Refused(ControlRefusal {
            in_reply_to: match generator.below(3) {
                0 => ReplyTarget::Hello {},
                1 => ReplyTarget::UnparsedMessage {},
                _ => ReplyTarget::OpenSession {
                    request_id: ControlRequestId(generator.next_u64() as u32),
                },
            },
            reason: *generator.pick(&[
                RefusalReason::PlayerUnavailable {},
                RefusalReason::ProtectedContent {},
                RefusalReason::HelloRequired {},
                RefusalReason::IncompatibleVersion {
                    update: BridgeSoftware::Extension,
                },
                RefusalReason::MalformedMessage {
                    error: MessageErrorKind::UnknownField,
                },
            ]),
        }),
    }
}

/// Свойство 1 для одного вида сообщений.
fn assert_round_trips<M>(seed: u64, iterations: usize, make: fn(&mut Generator) -> M)
where
    M: ProtocolMessage + PartialEq + Debug,
{
    let mut generator = Generator::new(seed);
    for iteration in 0..iterations {
        let original = make(&mut generator);
        let written = encode_message(&original)
            .unwrap_or_else(|error| panic!("#{iteration} {original:?}: {error}"));
        let parsed: M = decode_message(written.as_bytes())
            .unwrap_or_else(|error| panic!("#{iteration} {written}: {error}"));
        assert_eq!(parsed, original, "#{iteration} seed {seed}");
    }
}

#[test]
fn random_valid_messages_round_trip_unchanged() {
    assert_round_trips(0x0b1d_9e01, 3000, random_page_message);
    assert_round_trips(0x0b1d_9e02, 3000, random_player_message);
    assert_round_trips(0x0b1d_9e03, 1500, random_extension_control);
    assert_round_trips(0x0b1d_9e04, 1500, random_player_control);
}

#[test]
fn random_data_frame_headers_round_trip_unchanged() {
    let mut generator = Generator::new(0xda7a_f4a3);
    for iteration in 0..5000 {
        let header = if generator.coin() {
            DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
                generation: generation(&mut generator),
                source_buffer: source_buffer(&mut generator),
                sequence: AppendSequence(generator.next_u64()),
            })
        } else {
            DataFrameHeader::RangeBytes(RangeBytesHeader {
                request_id: RangeRequestId(generator.next_u64() as u32),
                offset: ByteOffset::new(generator.below(MAX_JSON_SAFE_INTEGER + 1)).unwrap(),
            })
        };
        let payload: Vec<u8> = (0..generator.between(1, 64))
            .map(|_| generator.next_u64() as u8)
            .collect();
        let frame = encode_data_frame(&header, &payload).unwrap();
        let parsed = decode_data_frame(&frame).unwrap();
        assert_eq!(
            (parsed.header, parsed.payload),
            (header, payload.as_slice()),
            "#{iteration}"
        );
    }
}

#[test]
fn data_frame_payload_limit_is_exact() {
    let header = DataFrameHeader::RangeBytes(RangeBytesHeader {
        request_id: RangeRequestId(1),
        offset: ByteOffset::new(0).unwrap(),
    });
    let at_limit = vec![0xAB; MAX_DATA_FRAME_PAYLOAD_BYTES];
    let frame = encode_data_frame(&header, &at_limit).unwrap();
    assert_eq!(
        decode_data_frame(&frame).unwrap().payload.len(),
        MAX_DATA_FRAME_PAYLOAD_BYTES
    );
    let mut over_limit = frame;
    over_limit.push(0xAB);
    assert!(decode_data_frame(&over_limit).is_err());
    assert!(encode_data_frame(&header, &[0xAB; MAX_DATA_FRAME_PAYLOAD_BYTES + 1]).is_err());
}

/// Разбор любым видом: только `Ok`/`Err`, а при `Ok` — устойчивая запись.
fn decode_with_every_schema(bytes: &[u8]) {
    fn stable<M: ProtocolMessage + PartialEq + Debug>(bytes: &[u8]) {
        if let Ok(message) = decode_message::<M>(bytes) {
            let written = encode_message(&message).unwrap();
            assert_eq!(decode_message::<M>(written.as_bytes()).unwrap(), message);
        }
    }
    stable::<ExtensionControlMessage>(bytes);
    stable::<PlayerControlMessage>(bytes);
    stable::<PageSessionMessage>(bytes);
    stable::<PlayerSessionMessage>(bytes);
    if let Ok(frame) = decode_data_frame(bytes) {
        assert_eq!(
            encode_data_frame(&frame.header, frame.payload).unwrap(),
            bytes
        );
    }
}

fn valid_fixture_files() -> Vec<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut files = Vec::new();
    for directory in ["control/valid", "session/valid", "data/valid"] {
        for entry in std::fs::read_dir(root.join(directory)).unwrap() {
            files.push(std::fs::read(entry.unwrap().path()).unwrap());
        }
    }
    assert!(files.len() > 40, "валидных фикстур мало: {}", files.len());
    files
}

#[test]
fn every_truncation_of_every_valid_fixture_is_a_typed_error() {
    for fixture in valid_fixture_files() {
        for length in 0..fixture.len() {
            let prefix = &fixture[..length];
            decode_with_every_schema(prefix);
            // Обрезанный JSON-объект никогда не валиден; у рамки короче
            // заголовка + 1 байт груза — тоже ошибка.
            if fixture.first() == Some(&b'{') {
                assert!(decode_message::<PageSessionMessage>(prefix).is_err());
                assert!(decode_message::<ExtensionControlMessage>(prefix).is_err());
            } else if length <= 24 {
                assert!(decode_data_frame(prefix).is_err());
            }
        }
    }
}

#[test]
fn corrupted_fixtures_and_random_bytes_never_panic() {
    let mut generator = Generator::new(0xbad_c0de);
    let fixtures = valid_fixture_files();
    for _ in 0..20_000 {
        let mut bytes = generator.pick(&fixtures).clone();
        // От 1 до 4 повреждений: замена байта, вставка, удаление, повтор куска.
        for _ in 0..generator.between(1, 4) {
            if bytes.is_empty() {
                break;
            }
            let position = generator.below(bytes.len() as u64) as usize;
            match generator.below(4) {
                0 => bytes[position] = generator.next_u64() as u8,
                1 => bytes.insert(position, *generator.pick(b"{}[]\",:0\\u")),
                2 => {
                    bytes.remove(position);
                }
                _ => {
                    let end = (position + generator.between(1, 16) as usize).min(bytes.len());
                    let piece = bytes[position..end].to_vec();
                    bytes.splice(position..position, piece);
                }
            }
        }
        decode_with_every_schema(&bytes);
    }
    for _ in 0..5_000 {
        let length = generator.below(300) as usize;
        let bytes: Vec<u8> = (0..length).map(|_| generator.next_u64() as u8).collect();
        decode_with_every_schema(&bytes);
    }
}

#[test]
fn deeply_nested_json_is_rejected_without_stack_overflow() {
    // Вложенность в корне: синтаксис проверяется без рекурсии, корень — не объект.
    let nested_root = format!("{}{}", "[".repeat(30_000), "]".repeat(30_000));
    assert_eq!(
        decode_message::<PageSessionMessage>(nested_root.as_bytes()).map_err(|error| error.kind()),
        Err(MessageErrorKind::NotAnObject)
    );
    // Вложенность внутри поля: настоящий разбор упирается в предел глубины
    // serde_json (128) и честно отказывает, а не роняет стек.
    let nested_field = format!(
        "{{\"type\":\"tab_gone\",\"reason\":{}{}}}",
        "[".repeat(30_000),
        "]".repeat(30_000)
    );
    assert_eq!(
        decode_message::<PageSessionMessage>(nested_field.as_bytes()).map_err(|error| error.kind()),
        Err(MessageErrorKind::Syntax)
    );
    // Сверх лимита размера — отказ ещё до разбора.
    let oversized = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert_eq!(
        decode_message::<PageSessionMessage>(oversized.as_bytes()).map_err(|error| error.kind()),
        Err(MessageErrorKind::TooLarge)
    );
}

#[test]
fn stream_gate_accepts_only_current_generation_in_consecutive_order_on_random_input() {
    let mut generator = Generator::new(0x6a7e_0001);
    for _ in 0..200 {
        let mut gate = StreamGate::new();
        // Модель: следующий ожидаемый номер по (поколение, SourceBuffer).
        let mut accepted_next: BTreeMap<(u32, u16), u64> = BTreeMap::new();
        for _ in 0..400 {
            let current = gate.current_generation().map_or(0, StreamGeneration::get);
            let generation_number =
                (current as i64 + generator.between(0, 3) as i64 - 1).max(1) as u32;
            let generation = StreamGeneration::new(generation_number).unwrap();
            let index = SourceBufferIndex::new(generator.below(3) as u16).unwrap();
            match generator.below(3) {
                0 => {
                    let _ =
                        gate.apply_message(&PageSessionMessage::GenerationStarted { generation });
                }
                1 => {
                    let _ = gate.apply_message(&PageSessionMessage::SourceBufferAdded {
                        generation,
                        source_buffer: index,
                        mime_type: MimeType::new("video/mp4").unwrap(),
                    });
                }
                _ => {
                    let key = (generation_number, index.get());
                    let expected = accepted_next.get(&key).copied().unwrap_or(0);
                    // Чаще правильный номер, иногда соседний — чтобы ловить пропуски.
                    let sequence =
                        (expected as i64 + generator.between(0, 4) as i64 - 1).max(0) as u64;
                    let header = SourceBufferBytesHeader {
                        generation,
                        source_buffer: index,
                        sequence: AppendSequence(sequence),
                    };
                    if gate.admit_source_buffer_bytes(&header) == Ok(GateDecision::Accept) {
                        assert_eq!(
                            Some(generation),
                            gate.current_generation(),
                            "принято не текущее поколение"
                        );
                        assert_eq!(sequence, expected, "принят номер не по порядку");
                        accepted_next.insert(key, expected + 1);
                    }
                }
            }
        }
    }
}
