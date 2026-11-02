//! Fuzz рамок и потока посредника на своём детерминированном генераторе.
//!
//! `proptest` в дереве нет; генератор xorshift64* — как в property-тестах
//! протокола: без новой зависимости, а упавший прогон повторяется по зерну.
//!
//! Каждый прогон собирает случайный вход браузера: валидные `hello` и
//! `open_session`, мусор вместо JSON, случайные байты, пустые рамки, иногда —
//! огромная объявленная длина или обрыв посреди рамки. Вход режется на случайные
//! куски, иногда с прерыванием сигналом. Дверь отвечает случайно: валидным
//! ответом плеера, мусором или отказом.
//!
//! Свойства:
//! 1. посредник всегда завершается типизированным выходом (без паники и зависания),
//!    и именно тем, который предсказывает модель входа;
//! 2. на каждую целую рамку до точки выхода — ровно один ответ, и каждый ответ —
//!    валидный `PlayerControlMessage` не длиннее предела браузера;
//! 3. в дверь звонят только сообщения, которые строго разбираются;
//! 4. вход не читается дальше нужного: при огромной длине — только префикс.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::ffi::OsString;
use std::io::{self, Read};

use browser_bridge_host::door::{DoorFailure, PlayerDoor};
use browser_bridge_host::framing::{
    FrameReadError, MAX_MESSAGE_FROM_BROWSER_BYTES, MAX_MESSAGE_TO_BROWSER_BYTES,
};
use browser_bridge_host::host::{HostExit, run_host};
use browser_bridge_host::log::{HostEvent, HostLog};
use browser_bridge_protocol::extension_identity::CHROMIUM_DEVELOPMENT_EXTENSION_ID;
use browser_bridge_protocol::{ExtensionControlMessage, PlayerControlMessage, decode_message};

/// Сколько случайных прогонов.
const RUNS: u64 = 3000;

const HELLO: &str = r#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}"#;
const OPEN_SESSION: &str = r#"{"type":"open_session","request_id":3,"session_key":"tab-1-video-1","page":{"title":"Episode","domain":"example.org"},"intent":"window","hints":{"duration":{"kind":"unknown"},"position":{"kind":"unknown"}}}"#;
const HELLO_ACCEPTED: &str =
    include_str!("../../browser-bridge-protocol/fixtures/control/valid/hello-accepted.json");
const SESSION_GRANTED: &str =
    include_str!("../../browser-bridge-protocol/fixtures/control/valid/session-granted.json");

/// Детерминированный генератор xorshift64*.
struct Generator(u64);

impl Generator {
    fn new(seed: u64) -> Self {
        // Нулевое состояние у xorshift вырождается — подменяем константой.
        Self(seed.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15))
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
    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next_u64() % u64::try_from(bound).unwrap()).unwrap()
    }

    fn bytes(&mut self, count: usize) -> Vec<u8> {
        (0..count)
            .map(|_| self.next_u64().to_le_bytes()[0])
            .collect()
    }
}

/// Чем кончается вход по модели теста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedEnd {
    /// Все рамки целы, вход закрыт между рамками.
    Closed,
    /// Последняя рамка объявила длину больше предела.
    TooLarge,
    /// Вход оборван посреди префикса.
    TruncatedLength,
    /// Вход оборван посреди тела.
    TruncatedMessage,
}

/// Случайный вход браузера и его модель.
struct GeneratedInput {
    bytes: Vec<u8>,
    /// Тела целых рамок до точки выхода.
    whole_frames: Vec<Vec<u8>>,
    end: ExpectedEnd,
    /// Сколько байт посредник обязан прочитать до выхода.
    bytes_to_consume: usize,
}

fn length_prefix(length: usize) -> [u8; 4] {
    u32::try_from(length).unwrap().to_ne_bytes()
}

fn random_body(generator: &mut Generator) -> Vec<u8> {
    match generator.below(7) {
        0 => HELLO.as_bytes().to_vec(),
        1 => OPEN_SESSION.as_bytes().to_vec(),
        2 => Vec::new(),
        3 => br#"{"type":"tab_gone","reason":"navigated_away"}"#.to_vec(),
        4 => {
            // Валидное сообщение с порченым байтом.
            let mut body = HELLO.as_bytes().to_vec();
            let position = generator.below(body.len());
            body[position] = generator.bytes(1)[0];
            body
        }
        5 => {
            // Валидное сообщение, обрезанное посередине.
            let cut = generator.below(OPEN_SESSION.len());
            OPEN_SESSION.as_bytes()[..cut].to_vec()
        }
        _ => {
            let length = generator.below(300);
            generator.bytes(length)
        }
    }
}

fn generate_input(generator: &mut Generator) -> GeneratedInput {
    let mut bytes = Vec::new();
    let mut whole_frames = Vec::new();
    for _ in 0..generator.below(8) {
        let body = random_body(generator);
        bytes.extend(length_prefix(body.len()));
        bytes.extend(&body);
        whole_frames.push(body);
    }
    let end = match generator.below(4) {
        0 => ExpectedEnd::Closed,
        1 => {
            let over = MAX_MESSAGE_FROM_BROWSER_BYTES + 1;
            let declared = over + generator.below(usize::try_from(u32::MAX).unwrap() - over);
            bytes.extend(length_prefix(declared));
            let consumed = bytes.len();
            // Хвост «тела», который читать нельзя.
            let tail_length = generator.below(64);
            bytes.extend(generator.bytes(tail_length));
            return GeneratedInput {
                bytes,
                whole_frames,
                end: ExpectedEnd::TooLarge,
                bytes_to_consume: consumed,
            };
        }
        2 => {
            let prefix = length_prefix(5);
            bytes.extend(&prefix[..1 + generator.below(3)]);
            ExpectedEnd::TruncatedLength
        }
        _ => {
            let body = random_body(generator);
            if body.is_empty() {
                // Пустое тело оборвать нельзя — тогда это обрыв префикса.
                bytes.extend(&length_prefix(9)[..2]);
                ExpectedEnd::TruncatedLength
            } else {
                bytes.extend(length_prefix(body.len()));
                bytes.extend(&body[..generator.below(body.len())]);
                ExpectedEnd::TruncatedMessage
            }
        }
    };
    let consumed = bytes.len();
    GeneratedInput {
        bytes,
        whole_frames,
        end,
        bytes_to_consume: consumed,
    }
}

/// Вход, режущий байты на случайные куски, иногда прерываемый сигналом.
struct ShreddingInput<'generator> {
    bytes: Vec<u8>,
    position: usize,
    generator: &'generator mut Generator,
}

impl Read for ShreddingInput<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.generator.below(10) == 0 {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let available = self.bytes.len() - self.position;
        let count = buffer
            .len()
            .min(available)
            .min(1 + self.generator.below(40));
        buffer[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

/// Дверь со случайными ответами (что в неё звонят, проверяет второй тест).
struct RandomDoor {
    generator: Generator,
}

impl PlayerDoor for RandomDoor {
    fn ring(&mut self, _message_json: &str) -> Result<String, DoorFailure> {
        match self.generator.below(6) {
            0 => Ok(HELLO_ACCEPTED.trim_end().to_owned()),
            1 => Ok(SESSION_GRANTED.trim_end().to_owned()),
            2 => Ok(String::from_utf8_lossy(&self.generator.bytes(30)).into_owned()),
            3 => Err(DoorFailure::PlayerNotResponding),
            4 => Err(DoorFailure::PlayerShuttingDown),
            _ => Err(DoorFailure::PlayerUnavailable),
        }
    }
}

/// Журнал, который ничего не делает.
struct SilentLog;

impl HostLog for SilentLog {
    fn record(&mut self, _event: &HostEvent) {}
}

/// Разрезает вывод посредника на тела ответов.
fn reply_bodies(output: &[u8]) -> Vec<&[u8]> {
    let mut rest = output;
    let mut bodies = Vec::new();
    while !rest.is_empty() {
        let (prefix, after) = rest.split_first_chunk::<4>().expect("whole prefix");
        let length = usize::try_from(u32::from_ne_bytes(*prefix)).unwrap();
        assert!(length <= MAX_MESSAGE_TO_BROWSER_BYTES);
        let (body, after) = after.split_at(length);
        bodies.push(body);
        rest = after;
    }
    bodies
}

fn exit_matches(exit: HostExit, end: ExpectedEnd) -> bool {
    match end {
        ExpectedEnd::Closed => exit == HostExit::InputClosed,
        ExpectedEnd::TooLarge => {
            matches!(exit, HostExit::BrokenFrame(FrameReadError::TooLarge { .. }))
        }
        ExpectedEnd::TruncatedLength => matches!(
            exit,
            HostExit::BrokenFrame(FrameReadError::TruncatedLength { .. })
        ),
        ExpectedEnd::TruncatedMessage => matches!(
            exit,
            HostExit::BrokenFrame(FrameReadError::TruncatedMessage { .. })
        ),
    }
}

#[test]
fn random_browser_input_always_ends_typed_with_one_valid_reply_per_frame() {
    let arguments = vec![OsString::from(format!(
        "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/"
    ))];
    for seed in 0..RUNS {
        let mut generator = Generator::new(seed);
        let input = generate_input(&mut generator);
        let mut door = Some(RandomDoor {
            generator: Generator::new(seed ^ 0xD00D),
        });
        let mut shredding = ShreddingInput {
            bytes: input.bytes.clone(),
            position: 0,
            generator: &mut generator,
        };
        let mut output = Vec::new();
        let exit = run_host(
            &arguments,
            &mut shredding,
            &mut output,
            || door.take().expect("the door is opened once"),
            &mut SilentLog,
        );
        let consumed = shredding.position;

        assert!(
            exit_matches(exit, input.end),
            "seed {seed}: exit {exit:?}, expected {:?}",
            input.end
        );
        assert_eq!(
            consumed, input.bytes_to_consume,
            "seed {seed}: the host read past the point it had to stop at"
        );
        let bodies = reply_bodies(&output);
        assert_eq!(bodies.len(), input.whole_frames.len(), "seed {seed}");
        for body in bodies {
            decode_message::<PlayerControlMessage>(body)
                .unwrap_or_else(|error| panic!("seed {seed}: invalid reply {error}"));
        }
    }
}

#[test]
fn only_strictly_valid_messages_reach_the_door() {
    let arguments = vec![OsString::from(format!(
        "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/"
    ))];
    for seed in 0..RUNS {
        let mut generator = Generator::new(seed.wrapping_mul(31));
        let input = generate_input(&mut generator);
        let expected_rings: Vec<String> = input
            .whole_frames
            .iter()
            .filter(|body| decode_message::<ExtensionControlMessage>(body).is_ok())
            .map(|body| String::from_utf8(body.clone()).unwrap())
            .collect();
        let rung_with = std::cell::RefCell::new(Vec::new());
        struct RecordingDoor<'record>(&'record std::cell::RefCell<Vec<String>>);
        impl PlayerDoor for RecordingDoor<'_> {
            fn ring(&mut self, message_json: &str) -> Result<String, DoorFailure> {
                self.0.borrow_mut().push(message_json.to_owned());
                Err(DoorFailure::PlayerUnavailable)
            }
        }
        let shredding = ShreddingInput {
            bytes: input.bytes.clone(),
            position: 0,
            generator: &mut generator,
        };
        run_host(
            &arguments,
            shredding,
            Vec::new(),
            || RecordingDoor(&rung_with),
            &mut SilentLog,
        );
        assert_eq!(rung_with.into_inner(), expected_rings, "seed {seed}");
    }
}
