//! `live_window`: версия протокола, предел частоты, сведение, первое окно поколения,
//! сброс учёта и «не трогает чужое».

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use browser_bridge_protocol::{
    LiveSeekable, MediaTime, PlayerSessionMessage, ProtocolVersion, StreamGeneration,
    encode_message,
};
use browser_bridge_session::SendToPageError;
use pushed_media_source::{BufferIntake, BufferStatus};

use super::{LIVE_WINDOW_INTERVAL, LIVE_WINDOW_SINCE, LiveWindowNote, LiveWindowTold};
use crate::browser_session::outbound::{PageOutbound, PageSender};
use crate::browser_session::playback::live_window::LiveWindowObservation;

/// Страница-подделка: запоминает отправленное, может отказать заранее заданным отказом.
#[derive(Clone, Default)]
struct FakePage {
    sent: Rc<RefCell<Vec<PlayerSessionMessage>>>,
    refusals: Rc<RefCell<VecDeque<SendToPageError>>>,
}

impl FakePage {
    fn take_sent(&self) -> Vec<PlayerSessionMessage> {
        std::mem::take(&mut *self.sent.borrow_mut())
    }

    fn live_windows(&self) -> Vec<PlayerSessionMessage> {
        self.take_sent()
            .into_iter()
            .filter(|message| matches!(message, PlayerSessionMessage::LiveWindow { .. }))
            .collect()
    }
}

impl PageSender for FakePage {
    fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError> {
        if let Some(reason) = self.refusals.borrow_mut().pop_front() {
            return Err(reason);
        }
        self.sent.borrow_mut().push(message.clone());
        Ok(())
    }
}

fn outbound_speaking(version: ProtocolVersion) -> (PageOutbound, FakePage) {
    let page = FakePage::default();
    let mut outbound = PageOutbound::new(Box::new(page.clone()));
    outbound.note_agreed_protocol(version);
    (outbound, page)
}

fn generation(number: u32) -> StreamGeneration {
    StreamGeneration::new(number).expect("generation")
}

fn us(micros: u64) -> MediaTime {
    MediaTime::from_micros(micros).expect("media time")
}

/// Эфир с DVR, моменты — в секундах.
fn dvr(edge: u64, start: u64, end: u64) -> LiveWindowObservation {
    LiveWindowObservation::LiveWithDvr {
        edge: Duration::from_secs(edge),
        start: Duration::from_secs(start),
        end: Duration::from_secs(end),
    }
}

/// Сообщение `live_window` с DVR, моменты — в секундах.
fn dvr_message(generation_number: u32, edge: u64, start: u64, end: u64) -> PlayerSessionMessage {
    PlayerSessionMessage::LiveWindow {
        generation: generation(generation_number),
        edge_us: us(edge * 1_000_000),
        seekable: LiveSeekable::Range {
            start_us: us(start * 1_000_000),
            end_us: us(end * 1_000_000),
        },
    }
}

/// Порог версии — тот же, что объявляет сообщение протокола.
#[test]
fn the_version_threshold_is_the_one_of_the_protocol_message() {
    assert_eq!(dvr_message(1, 3, 1, 2).introduced_in(), LIVE_WINDOW_SINCE);
}

/// Страница 1.2: первое окно уходит сразу, в форме протокола, и проходит его проверку.
#[test]
fn a_1_2_page_gets_the_first_window_at_once_in_protocol_form() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();

    let note = outbound.note_live_window(generation(3), dvr(61, 40, 60), now);
    outbound.flush(now);

    assert_eq!(note, LiveWindowNote::Queued);
    let sent = page.take_sent();
    assert_eq!(sent, vec![dvr_message(3, 61, 40, 60)]);
    encode_message(&sent[0]).expect("the window passes the protocol checks");
}

/// Эфир без DVR — `seekable.kind = none` с краем.
#[test]
fn a_live_stream_without_dvr_is_sent_as_none() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    let window = LiveWindowObservation::LiveWithoutDvr {
        edge: Duration::from_millis(61_500),
    };

    assert_eq!(
        outbound.note_live_window(generation(1), window, now),
        LiveWindowNote::Queued
    );
    outbound.flush(now);

    assert_eq!(
        page.take_sent(),
        vec![PlayerSessionMessage::LiveWindow {
            generation: generation(1),
            edge_us: us(61_500_000),
            seekable: LiveSeekable::None {},
        }]
    );
}

/// Страницам 1.1 и 1.0 окно не уходит никогда — ни сразу, ни после предела.
#[test]
fn pages_below_1_2_never_get_a_window() {
    for version in [ProtocolVersion::V1_1, ProtocolVersion::V1_0] {
        let (mut outbound, page) = outbound_speaking(version);
        let now = Instant::now();
        for step in 0..3_u64 {
            let at = now + LIVE_WINDOW_INTERVAL * u32::try_from(step).expect("step");
            assert_eq!(
                outbound.note_live_window(generation(1), dvr(10 + step, 1, 9 + step), at),
                LiveWindowNote::ProtocolTooOld,
                "{version:?}"
            );
            outbound.flush(at);
        }
        assert!(page.take_sent().is_empty(), "{version:?}");
        assert!(!outbound.has_pending(), "{version:?}");
    }
}

/// До `Attached` версия неизвестна: окно не ставится и не считается отправленным —
/// после подключения страницы 1.2 первое окно уходит сразу.
#[test]
fn before_attach_nothing_is_queued_and_the_first_window_goes_after_attach() {
    let page = FakePage::default();
    let mut outbound = PageOutbound::new(Box::new(page.clone()));
    let now = Instant::now();

    assert_eq!(
        outbound.note_live_window(generation(1), dvr(10, 1, 9), now),
        LiveWindowNote::NotAttachedYet
    );
    outbound.flush(now);
    assert!(page.take_sent().is_empty());

    outbound.note_agreed_protocol(ProtocolVersion::V1_2);
    let later = now + Duration::from_millis(10);
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(10, 1, 9), later),
        LiveWindowNote::Queued
    );
    outbound.flush(later);
    assert_eq!(page.live_windows(), vec![dvr_message(1, 10, 1, 9)]);
}

/// Не эфир и эфир без края — окна нет, ничего не уходит.
#[test]
fn no_window_is_sent_for_static_media_or_an_unknown_edge() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    for window in [
        LiveWindowObservation::NotLive,
        LiveWindowObservation::LiveEdgeUnknown,
    ] {
        assert_eq!(
            outbound.note_live_window(generation(1), window, now),
            LiveWindowNote::NoWindow
        );
    }
    outbound.flush(now);
    assert!(page.take_sent().is_empty());
    // «Окна нет» не занимает предел: настоящее окно уходит сразу.
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(5, 1, 4), now),
        LiveWindowNote::Queued
    );
}

/// Предел частоты и сведение: изменения внутри промежутка придерживаются, после срока
/// уходит ПОСЛЕДНЕЕ окно (промежуточное — нет), и не больше одного на промежуток.
#[test]
fn changes_inside_the_interval_are_coalesced_and_the_latest_goes_after_it() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);
    outbound.flush(now);
    assert_eq!(page.live_windows().len(), 1);

    let soon = now + Duration::from_millis(300);
    let later = now + Duration::from_millis(700);
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(11, 2, 10), soon),
        LiveWindowNote::Throttled
    );
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(12, 3, 11), later),
        LiveWindowNote::Throttled
    );
    outbound.flush(later);
    assert!(
        page.live_windows().is_empty(),
        "inside the interval nothing goes"
    );

    // Срок истёк ровно на границе: уходит то, что плеер видит сейчас.
    let due = now + LIVE_WINDOW_INTERVAL;
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(12, 3, 11), due),
        LiveWindowNote::Queued
    );
    outbound.flush(due);
    assert_eq!(page.live_windows(), vec![dvr_message(1, 12, 3, 11)]);

    // Отсчёт пошёл заново от последней постановки.
    let right_after = due + Duration::from_millis(100);
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(13, 4, 12), right_after),
        LiveWindowNote::Throttled
    );
}

/// Неизменное окно не повторяется — ни сразу, ни после предела.
#[test]
fn an_unchanged_window_is_not_repeated() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);
    outbound.flush(now);
    page.take_sent();

    for later in [Duration::from_millis(1), LIVE_WINDOW_INTERVAL * 5] {
        assert_eq!(
            outbound.note_live_window(generation(1), dvr(10, 1, 9), now + later),
            LiveWindowNote::Unchanged
        );
    }
    outbound.flush(now + LIVE_WINDOW_INTERVAL * 5);
    assert!(page.take_sent().is_empty());
}

/// Новое поколение: его первое окно уходит сразу, хотя окно прежнего ушло только что.
#[test]
fn the_first_window_of_a_new_generation_goes_at_once() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);
    let soon = now + Duration::from_millis(50);
    assert_eq!(
        outbound.note_live_window(generation(2), dvr(4, 0, 3), soon),
        LiveWindowNote::Queued
    );
    outbound.flush(soon);
    // Неотправленное окно прежнего поколения заменено окном нового (одно место).
    assert_eq!(page.live_windows(), vec![dvr_message(2, 4, 0, 3)]);
    // Дальше предел — уже по новому поколению.
    assert_eq!(
        outbound.note_live_window(generation(2), dvr(5, 1, 4), soon),
        LiveWindowNote::Throttled
    );
}

/// Новый сеанс сбрасывает учёт: странице нового сеанса окно уходит сразу (после её
/// `Attached`), даже то же самое и внутри промежутка.
#[test]
fn a_new_session_resets_the_cadence() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);
    outbound.flush(now);
    page.take_sent();

    outbound.reset_for_new_session();
    let soon = now + Duration::from_millis(50);
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(10, 1, 9), soon),
        LiveWindowNote::NotAttachedYet
    );
    outbound.note_agreed_protocol(ProtocolVersion::V1_2);
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(10, 1, 9), soon),
        LiveWindowNote::Queued
    );
    outbound.flush(soon);
    assert_eq!(page.live_windows(), vec![dvr_message(1, 10, 1, 9)]);
}

/// Страница переподключается: окно ждёт её, а неотправленное старое заменяется новым —
/// вернувшаяся страница получает одно, последнее окно.
#[test]
fn while_the_page_is_detached_only_the_latest_window_waits() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    page.refusals
        .borrow_mut()
        .push_back(SendToPageError::NoAttachedSession);
    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);
    outbound.flush(now);
    assert!(outbound.has_pending(), "the window waits for the page");

    let due = now + LIVE_WINDOW_INTERVAL;
    outbound.note_live_window(generation(1), dvr(11, 2, 10), due);
    outbound.flush(due);

    assert_eq!(page.take_sent(), vec![dvr_message(1, 11, 2, 10)]);
    assert!(!outbound.has_pending());
}

/// Окно не трогает чужое: ритм и номера часов, состояние буфера и его правило остаются
/// какими были.
#[test]
fn a_window_does_not_touch_the_clock_or_the_buffer_state() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    let buffer = BufferStatus {
        buffered_ahead: Duration::from_secs(3),
        intake: BufferIntake::Accepting,
    };
    outbound.note_buffer(generation(1), buffer, now);
    let clock_before = outbound.next_clock_at();

    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);

    assert_eq!(outbound.next_clock_at(), clock_before, "the clock cadence");
    outbound.flush(now);
    let sent = page.take_sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0], dvr_message(1, 10, 1, 9));
    assert!(
        matches!(
            sent[1],
            PlayerSessionMessage::BufferState { generation: g, .. } if g == generation(1)
        ),
        "the pending buffer state is still sent: {sent:?}"
    );
    // Номер первого тика часов не сдвинут окном.
    let observation = crate::browser_session::playback::PlaybackObservation {
        position: Duration::ZERO,
        duration: None,
        state: player_core::PlaybackState::Playing,
        rate: player_core::PlaybackRate::NORMAL,
        buffering: false,
        ended: false,
        fault: None,
        shown_load: None,
        seek_target: None,
        seek_answers: crate::browser_session::playback::PlayerSeekAnswers::default(),
        live_window: dvr(10, 1, 9),
    };
    outbound.tick_clock(&observation, now);
    assert!(
        matches!(
            page.take_sent().as_slice(),
            [PlayerSessionMessage::Clock { seq, .. }] if seq.0 == 0
        ),
        "the clock sequence starts at zero"
    );
}

/// «Страница знает время эфира» — только после ДОСТАВКИ окна этого поколения: не после
/// постановки, не для другого поколения и не после смены сеанса.
#[test]
fn a_generation_counts_as_told_only_once_its_window_is_delivered() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    assert_eq!(
        outbound.live_window_told(generation(1)),
        LiveWindowTold::NotYet
    );

    page.refusals
        .borrow_mut()
        .push_back(SendToPageError::NoAttachedSession);
    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);
    outbound.flush(now);
    assert_eq!(
        outbound.live_window_told(generation(1)),
        LiveWindowTold::NotYet,
        "queued but not delivered"
    );

    outbound.flush(now);
    assert_eq!(
        outbound.live_window_told(generation(1)),
        LiveWindowTold::Told
    );
    assert_eq!(
        outbound.live_window_told(generation(2)),
        LiveWindowTold::NotYet
    );

    outbound.reset_for_new_session();
    assert_eq!(
        outbound.live_window_told(generation(1)),
        LiveWindowTold::NotYet
    );
}

/// Повторное подключение страницы: учёт окна забыт — то же (неизменное) окно уходит
/// сразу, а «страница знает время эфира» — только после его новой доставки.
#[test]
fn a_reattached_page_gets_the_current_window_again_at_once() {
    let (mut outbound, page) = outbound_speaking(ProtocolVersion::V1_2);
    let now = Instant::now();
    outbound.note_live_window(generation(1), dvr(10, 1, 9), now);
    outbound.flush(now);
    page.take_sent();
    assert_eq!(
        outbound.live_window_told(generation(1)),
        LiveWindowTold::Told
    );

    outbound.forget_live_window_on_attach();
    assert_eq!(
        outbound.live_window_told(generation(1)),
        LiveWindowTold::NotYet
    );
    let soon = now + Duration::from_millis(50);
    assert_eq!(
        outbound.note_live_window(generation(1), dvr(10, 1, 9), soon),
        LiveWindowNote::Queued
    );
    outbound.flush(soon);
    assert_eq!(page.live_windows(), vec![dvr_message(1, 10, 1, 9)]);
    assert_eq!(
        outbound.live_window_told(generation(1)),
        LiveWindowTold::Told
    );
}
