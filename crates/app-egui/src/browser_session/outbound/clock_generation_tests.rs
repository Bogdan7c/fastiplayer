//! Номер поколения в тике часов: кому он уходит и как выглядит на проводе (16a2).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use browser_bridge_protocol::{PlayerSessionMessage, ProtocolVersion, encode_message};
use browser_bridge_session::SendToPageError;
use player_core::{PlaybackRate, PlaybackState};

use super::*;
use crate::browser_session::outbound::PageSender;
use crate::browser_session::playback::live_window::LiveWindowObservation;
use crate::browser_session::playback::{PlaybackObservation, PlayerSeekAnswers};

/// Отправитель, который записывает всё, что ему дали.
#[derive(Clone, Default)]
struct RecordingSender(Arc<Mutex<Vec<PlayerSessionMessage>>>);

impl PageSender for RecordingSender {
    fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError> {
        if let Ok(mut sent) = self.0.lock() {
            sent.push(message.clone());
        }
        Ok(())
    }
}

impl RecordingSender {
    fn sent(&self) -> Vec<PlayerSessionMessage> {
        self.0.lock().map(|sent| sent.clone()).unwrap_or_default()
    }
}

fn generation(number: u32) -> StreamGeneration {
    StreamGeneration::new(number).expect("generation")
}

fn playing_at_one_second() -> PlaybackObservation {
    PlaybackObservation {
        position: Duration::from_secs(1),
        duration: None,
        state: PlaybackState::Playing,
        rate: PlaybackRate::NORMAL,
        buffering: false,
        ended: false,
        fault: None,
        shown_load: None,
        seek_target: None,
        seek_answers: PlayerSeekAnswers::default(),
        live_window: LiveWindowObservation::NotLive,
    }
}

/// Один тик часов про поколение `described` странице с этой версией (`None` — страница
/// ещё не подключалась); возвращает ушедший тик.
fn one_tick(agreed: Option<ProtocolVersion>, described: StreamGeneration) -> PlayerSessionMessage {
    let sender = RecordingSender::default();
    let mut outbound = PageOutbound::new(Box::new(sender.clone()));
    if let Some(version) = agreed {
        outbound.note_agreed_protocol(version);
    }
    outbound.tick_clock(&playing_at_one_second(), described, Instant::now());
    let sent = sender.sent();
    assert_eq!(sent.len(), 1, "exactly one clock tick: {sent:?}");
    sent.into_iter().next().expect("one tick")
}

fn generation_of(tick: &PlayerSessionMessage) -> Option<StreamGeneration> {
    match tick {
        PlayerSessionMessage::Clock { generation, .. } => *generation,
        other => panic!("not a clock tick: {other:?}"),
    }
}

/// Страница 1.10 получает ровно тот номер, который назвала медиа-часть, и тик требует 1.10.
#[test]
fn a_1_10_page_gets_the_described_generation_in_the_clock() {
    let tick = one_tick(Some(ProtocolVersion::V1_10), generation(7));
    assert_eq!(generation_of(&tick), Some(generation(7)));
    assert_eq!(tick.required_version(), ProtocolVersion::V1_10);
    let wire = encode_message(&tick).expect("encodes");
    assert!(wire.ends_with(r#","generation":7}"#), "{wire}");
}

/// Страница 1.9 номера не знает: тик без поля, ровно форма 1.9 (ключа нет на проводе).
#[test]
fn a_1_9_page_gets_the_clock_without_the_generation_field() {
    let tick = one_tick(Some(ProtocolVersion::V1_9), generation(7));
    assert_eq!(generation_of(&tick), None);
    assert!(ProtocolVersion::V1_9.includes(tick.required_version()));
    let wire = encode_message(&tick).expect("encodes");
    assert!(!wire.contains("generation"), "{wire}");
}

/// Версия страницы ещё не известна (не было `Attached`): номер не уходит — тик без номера
/// законен при любой версии, а тик с номером странице 1.9 был бы нарушением.
#[test]
fn before_the_attach_the_clock_carries_no_generation() {
    let tick = one_tick(None, generation(3));
    assert_eq!(generation_of(&tick), None);
}

/// Порог — ровно версия протокола, где поле появилось (общая константа протокола).
#[test]
fn the_threshold_is_the_protocol_constant() {
    assert_eq!(CLOCK_GENERATION_INTRODUCED_IN, ProtocolVersion::V1_10);
    let sender = RecordingSender::default();
    let mut outbound = PageOutbound::new(Box::new(sender));
    outbound.note_agreed_protocol(CLOCK_GENERATION_INTRODUCED_IN);
    assert_eq!(
        outbound.clock_generation_field(generation(2)),
        Some(generation(2))
    );
}
