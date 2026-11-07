//! Свойство сторожа поколений на случайном входе (протокол 1.1): содержимое поколения
//! (MSE, файл, HLS) решается один раз, сообщения новее общей версии не принимаются,
//! отказ не меняет сторожа. Генератор — свой xorshift64*, как в `property.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::BTreeMap;

use browser_bridge_protocol::{
    GateDecision, MediaDuration, MimeType, PageSessionMessage, ProtocolVersion, ResourceHandle,
    SourceBufferIndex, StreamGate, StreamGeneration,
};

/// Детерминированный генератор xorshift64* (зерно не ноль).
struct Generator(u64);

impl Generator {
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

    fn pick<'item, T>(&mut self, items: &'item [T]) -> &'item T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// Вид содержимого поколения в модели теста.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ModelContent {
    Undecided,
    MediaSource,
    File,
    Hls,
}

#[test]
fn stream_gate_decides_generation_content_once_and_respects_the_agreed_version() {
    let mut generator = Generator(0x6a7e_0011);
    // Сколько раз сторож принял объявление каждого вида: тест не должен быть пустым.
    let mut accepted_kinds: BTreeMap<ModelContent, usize> = BTreeMap::new();
    for round in 0..200 {
        let agreed = *generator.pick(&[ProtocolVersion::V1_0, ProtocolVersion::V1_1]);
        let mut gate = StreamGate::for_agreed_version(agreed);
        let mut content = ModelContent::Undecided;
        for _ in 0..200 {
            let next = gate
                .current_generation()
                .map_or(1, |current| current.get() + 1);
            let current = gate.current_generation().map_or(1, StreamGeneration::get);
            let generation = StreamGeneration::new(current).unwrap();
            let (message, kind) = match generator.below(5) {
                0 => (
                    PageSessionMessage::GenerationStarted {
                        generation: StreamGeneration::new(next).unwrap(),
                    },
                    ModelContent::Undecided,
                ),
                1 => (
                    PageSessionMessage::SourceBufferAdded {
                        generation,
                        source_buffer: SourceBufferIndex::new(generator.below(8) as u16).unwrap(),
                        mime_type: MimeType::new("video/mp4").unwrap(),
                    },
                    ModelContent::MediaSource,
                ),
                2 => (
                    PageSessionMessage::FileSourceAdded { generation },
                    ModelContent::File,
                ),
                3 => (
                    PageSessionMessage::HlsSourceAdded {
                        generation,
                        root: ResourceHandle(generator.next_u64() as u32),
                    },
                    ModelContent::Hls,
                ),
                _ => (
                    PageSessionMessage::DurationChanged {
                        generation,
                        duration: MediaDuration::Unknown {},
                    },
                    ModelContent::MediaSource,
                ),
            };
            let before = gate.clone();
            let result = gate.apply_message(&message);
            match (&message, result) {
                (PageSessionMessage::GenerationStarted { .. }, Ok(GateDecision::Accept)) => {
                    content = ModelContent::Undecided;
                }
                (_, Ok(GateDecision::Accept)) => {
                    assert!(
                        content == ModelContent::Undecided || content == kind,
                        "round {round}: принято {message:?} в поколении {content:?}"
                    );
                    assert!(
                        agreed.includes(message.introduced_in()),
                        "round {round}: принято сообщение новее договора {agreed:?}"
                    );
                    // Длительность допустима до решения и ничего не решает.
                    if !matches!(message, PageSessionMessage::DurationChanged { .. }) {
                        content = kind;
                        *accepted_kinds.entry(kind).or_default() += 1;
                    }
                }
                (_, Ok(GateDecision::DropStale)) => panic!("в модели нет устаревших поколений"),
                (_, Err(_)) => assert_eq!(gate, before, "round {round}: отказ изменил сторожа"),
            }
        }
    }
    for kind in [
        ModelContent::MediaSource,
        ModelContent::File,
        ModelContent::Hls,
    ] {
        assert!(
            accepted_kinds.get(&kind).copied().unwrap_or(0) > 10,
            "{kind:?}: {accepted_kinds:?}"
        );
    }
}
