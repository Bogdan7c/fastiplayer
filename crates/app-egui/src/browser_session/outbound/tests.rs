use std::cell::RefCell;
use std::rc::Rc;

use super::*;

/// Страница-подделка: запоминает поставленные сообщения, на попытку может
/// ответить заранее заданным отказом.
#[derive(Clone, Default)]
struct FakePage {
    sent: Rc<RefCell<Vec<PlayerSessionMessage>>>,
    refusals: Rc<RefCell<VecDeque<SendToPageError>>>,
}

impl FakePage {
    fn refuse_next(&self, reason: SendToPageError) {
        self.refusals.borrow_mut().push_back(reason);
    }

    fn take_sent(&self) -> Vec<PlayerSessionMessage> {
        std::mem::take(&mut *self.sent.borrow_mut())
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

fn outbound() -> (PageOutbound, FakePage) {
    let page = FakePage::default();
    (PageOutbound::new(Box::new(page.clone())), page)
}

fn observation(position: Duration, state: PlaybackState, rate: f32) -> PlaybackObservation {
    PlaybackObservation {
        position,
        duration: None,
        state,
        rate: PlayerPlaybackRate::new(rate).expect("valid rate"),
        buffering: state == PlaybackState::Buffering,
        ended: state == PlaybackState::Ended,
        fault: None,
        shown_load: None,
    }
}

fn generation(number: u32) -> StreamGeneration {
    StreamGeneration::new(number).expect("generation")
}

fn status(ahead_ms: u64, intake: BufferIntake) -> BufferStatus {
    BufferStatus {
        buffered_ahead: Duration::from_millis(ahead_ms),
        intake,
    }
}

fn micros(value: u64) -> MediaTime {
    MediaTime::from_micros(value).expect("time")
}

fn clock(seq: u32, position_us: u64, state: ClockState, permille: u32) -> PlayerSessionMessage {
    PlayerSessionMessage::Clock {
        seq: ClockSequence(seq),
        position_us: micros(position_us),
        state,
        rate: PagePlaybackRate::from_permille(permille).expect("rate"),
    }
}

// ---- часы ------------------------------------------------------------------------

#[test]
fn every_player_state_maps_to_the_clock_state_the_page_element_expects() {
    let cases = [
        (PlaybackState::Playing, ClockState::Playing),
        (PlaybackState::Draining, ClockState::Playing),
        (PlaybackState::Paused, ClockState::Paused),
        (PlaybackState::Idle, ClockState::Paused),
        (PlaybackState::Stopped, ClockState::Paused),
        (PlaybackState::Failed, ClockState::Paused),
        (PlaybackState::Opening, ClockState::Waiting),
        (PlaybackState::Buffering, ClockState::Waiting),
        (PlaybackState::Scrubbing, ClockState::Waiting),
        (PlaybackState::Seeking, ClockState::Waiting),
        (PlaybackState::Ended, ClockState::Ended),
    ];
    for (state, expected) in cases {
        assert_eq!(clock_state_for(state), expected, "{state:?}");
    }
}

#[test]
fn clock_ticks_four_times_a_second_with_position_state_and_rate_in_protocol_units() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    let playing = observation(
        Duration::from_micros(1_234_567),
        PlaybackState::Playing,
        1.5,
    );

    outbound.tick_clock(&playing, start);
    outbound.tick_clock(&playing, start + Duration::from_millis(100));
    outbound.tick_clock(&playing, start + Duration::from_millis(249));
    assert_eq!(
        page.take_sent(),
        vec![clock(0, 1_234_567, ClockState::Playing, 1500)]
    );
    assert_eq!(outbound.next_clock_at(), Some(start + CLOCK_INTERVAL));

    let waiting = observation(Duration::from_secs(2), PlaybackState::Buffering, 1.0);
    outbound.tick_clock(&waiting, start + CLOCK_INTERVAL);
    assert_eq!(
        page.take_sent(),
        vec![clock(1, 2_000_000, ClockState::Waiting, 1000)]
    );
}

#[test]
fn clock_sequence_wraps_around_instead_of_stopping() {
    let (mut outbound, page) = outbound();
    outbound.next_clock_sequence = u32::MAX;
    let start = Instant::now();
    let ended = observation(Duration::from_millis(400), PlaybackState::Ended, 1.0);
    outbound.tick_clock(&ended, start);
    outbound.tick_clock(&ended, start + CLOCK_INTERVAL);
    assert_eq!(
        page.take_sent(),
        vec![
            clock(u32::MAX, 400_000, ClockState::Ended, 1000),
            clock(0, 400_000, ClockState::Ended, 1000),
        ]
    );
}

#[test]
fn clock_tick_refused_by_a_full_queue_is_dropped_and_replaced_by_the_next_one() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    let playing = observation(Duration::from_secs(1), PlaybackState::Playing, 1.0);
    page.refuse_next(SendToPageError::QueueFull);
    outbound.tick_clock(&playing, start);
    // Устаревший тик не повторяется ни отправкой отложенного, ни до срока.
    assert!(!outbound.has_pending());
    outbound.flush(start);
    outbound.tick_clock(&playing, start + Duration::from_millis(10));
    assert!(page.take_sent().is_empty());

    let later = observation(Duration::from_millis(1250), PlaybackState::Playing, 1.0);
    outbound.tick_clock(&later, start + CLOCK_INTERVAL);
    assert_eq!(
        page.take_sent(),
        vec![clock(1, 1_250_000, ClockState::Playing, 1000)]
    );
}

#[test]
fn clock_values_outside_the_protocol_range_are_clamped_not_rejected() {
    assert_eq!(clock_position(Duration::MAX), micros(MAX_JSON_SAFE_INTEGER));
    assert_eq!(
        clock_rate(PlayerPlaybackRate::MAX),
        PagePlaybackRate::from_permille(4000).expect("rate")
    );
    assert_eq!(
        clock_rate(PlayerPlaybackRate::MIN),
        PagePlaybackRate::from_permille(250).expect("rate")
    );
}

// ---- буфер -----------------------------------------------------------------------

fn buffer_state(number: u32, ahead_us: u64, intake: PageBufferIntake) -> PlayerSessionMessage {
    PlayerSessionMessage::BufferState {
        generation: generation(number),
        ahead_us: micros(ahead_us),
        intake,
    }
}

#[test]
fn buffer_state_goes_out_on_change_step_and_heartbeat_only() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let report = |outbound: &mut PageOutbound, number, ahead, intake, millis| {
        outbound.note_buffer(generation(number), status(ahead, intake), at(millis));
        outbound.flush(at(millis));
    };

    // Первое состояние поколения — сразу.
    report(&mut outbound, 1, 5_000, BufferIntake::Accepting, 0);
    // Запас сдвинулся меньше чем на секунду — молчим.
    report(&mut outbound, 1, 5_900, BufferIntake::Accepting, 300);
    // Сдвиг на секунду от отправленного — сообщаем.
    report(&mut outbound, 1, 6_000, BufferIntake::Accepting, 600);
    // Источник стал «полон» — сразу, даже без сдвига запаса.
    report(&mut outbound, 1, 6_100, BufferIntake::Full, 700);
    // Ничего не менялось, но прошло 2 с — напоминание.
    report(&mut outbound, 1, 6_100, BufferIntake::Full, 2_000);
    report(&mut outbound, 1, 6_100, BufferIntake::Full, 2_699);
    report(&mut outbound, 1, 6_100, BufferIntake::Full, 2_700);
    // Новое поколение — сразу.
    report(&mut outbound, 2, 0, BufferIntake::Accepting, 2_710);

    assert_eq!(
        page.take_sent(),
        vec![
            buffer_state(1, 5_000_000, PageBufferIntake::Accepting),
            buffer_state(1, 6_000_000, PageBufferIntake::Accepting),
            buffer_state(1, 6_100_000, PageBufferIntake::Full),
            buffer_state(1, 6_100_000, PageBufferIntake::Full),
            buffer_state(2, 0, PageBufferIntake::Accepting),
        ]
    );
}

#[test]
fn buffer_state_refused_by_a_full_queue_is_retried_with_the_freshest_value() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    page.refuse_next(SendToPageError::QueueFull);
    outbound.note_buffer(generation(1), status(3_000, BufferIntake::Accepting), start);
    outbound.flush(start);
    assert!(outbound.has_pending());
    assert!(page.take_sent().is_empty());

    // Свежее значение (даже с маленьким сдвигом) заменяет отложенное.
    let later = start + Duration::from_millis(50);
    outbound.note_buffer(generation(1), status(3_200, BufferIntake::Accepting), later);
    outbound.flush(later);
    assert!(!outbound.has_pending());
    assert_eq!(
        page.take_sent(),
        vec![buffer_state(1, 3_200_000, PageBufferIntake::Accepting)]
    );
}

// ---- перемотка и отказы ------------------------------------------------------

#[test]
fn page_seek_requests_get_fresh_increasing_ids_and_the_latest_survives_a_full_queue() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    let first = outbound.request_page_seek(PageSeekRequest {
        target: Duration::from_secs(90),
    });
    page.refuse_next(SendToPageError::QueueFull);
    outbound.flush(start);
    assert!(page.take_sent().is_empty());
    assert!(outbound.has_pending());

    // Новая цель, пока прежняя просьба не ушла: странице нужна только новая.
    let second = outbound.request_page_seek(PageSeekRequest {
        target: Duration::from_secs(30),
    });
    assert_eq!((first, second), (SeekId(1), SeekId(2)));

    outbound.flush(start + PENDING_RETRY_INTERVAL);
    assert_eq!(
        page.take_sent(),
        vec![PlayerSessionMessage::Seek {
            seek_id: SeekId(2),
            target_us: micros(30_000_000),
        }]
    );
    assert!(!outbound.has_pending());
}

#[test]
fn a_generation_is_rejected_once_and_older_generations_are_not_rejected_later() {
    let (mut outbound, page) = outbound();
    assert!(outbound.reject_generation(generation(2), StreamRejection::ProtectedContent));
    assert!(!outbound.reject_generation(generation(2), StreamRejection::UnsupportedCodec));
    assert!(!outbound.reject_generation(generation(1), StreamRejection::UnparsableMedia));
    outbound.flush(Instant::now());
    assert_eq!(
        page.take_sent(),
        vec![PlayerSessionMessage::StreamRejected {
            generation: generation(2),
            reason: StreamRejection::ProtectedContent,
        }]
    );
}

/// Страница переподключается (обрыв в пределах `reconnect_grace`): отказ и просьба
/// перемотаться ждут её и уходят, когда она вернётся; тик часов не ждёт.
#[test]
fn events_wait_for_a_reconnecting_page_and_clock_ticks_do_not() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    page.refuse_next(SendToPageError::NoAttachedSession);
    outbound.reject_generation(generation(1), StreamRejection::UnsupportedCodec);
    outbound.request_page_seek(PageSeekRequest {
        target: Duration::from_secs(5),
    });
    outbound.flush(start);
    assert!(page.take_sent().is_empty());
    assert!(outbound.has_pending(), "events wait for the page");

    page.refuse_next(SendToPageError::NoAttachedSession);
    outbound.tick_clock(
        &observation(Duration::from_secs(1), PlaybackState::Playing, 1.0),
        start,
    );
    assert!(page.take_sent().is_empty());

    // Страница вернулась: события уходят по порядку, а не устаревший тик — нет.
    outbound.flush(start + PENDING_RETRY_INTERVAL);
    assert_eq!(
        page.take_sent(),
        vec![
            PlayerSessionMessage::StreamRejected {
                generation: generation(1),
                reason: StreamRejection::UnsupportedCodec,
            },
            PlayerSessionMessage::Seek {
                seek_id: SeekId(1),
                target_us: micros(5_000_000),
            },
        ]
    );
    assert!(!outbound.has_pending());
}

/// Сколько бы ни длилось переподключение, ждёт не больше одного отказа (самого
/// свежего) и одной просьбы перемотаться (последней).
#[test]
fn pending_events_stay_bounded_while_the_page_is_away() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    for number in 1..=1_000_u32 {
        outbound.reject_generation(generation(number), StreamRejection::UnparsableMedia);
        outbound.request_page_seek(PageSeekRequest {
            target: Duration::from_secs(u64::from(number)),
        });
        page.refuse_next(SendToPageError::NoAttachedSession);
        outbound.flush(start);
    }
    assert_eq!(outbound.pending_events.len(), 2);

    outbound.flush(start + PENDING_RETRY_INTERVAL);
    assert_eq!(
        page.take_sent(),
        vec![
            PlayerSessionMessage::StreamRejected {
                generation: generation(1_000),
                reason: StreamRejection::UnparsableMedia,
            },
            PlayerSessionMessage::Seek {
                seek_id: SeekId(1_000),
                target_us: micros(1_000_000_000),
            },
        ]
    );
}

/// Сеанс закончился, пока страница была отключена: ждать её больше незачем.
#[test]
fn an_ended_session_forgets_what_waited_for_its_page() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    page.refuse_next(SendToPageError::NoAttachedSession);
    outbound.reject_generation(generation(1), StreamRejection::UnsupportedCodec);
    outbound.note_buffer(generation(1), status(1_000, BufferIntake::Accepting), start);
    outbound.flush(start);
    assert!(outbound.has_pending());

    outbound.forget_pending_of_ended_session();
    assert!(!outbound.has_pending());
    outbound.flush(start + PENDING_RETRY_INTERVAL);
    assert!(page.take_sent().is_empty());
}

#[test]
fn shutting_down_server_drops_messages_instead_of_retrying() {
    let (mut outbound, page) = outbound();
    page.refuse_next(SendToPageError::ShuttingDown);
    outbound.reject_generation(generation(1), StreamRejection::UnsupportedCodec);
    outbound.request_page_seek(PageSeekRequest {
        target: Duration::from_secs(5),
    });
    outbound.flush(Instant::now());
    // Отказ выброшен, следующая в очереди просьба ушла — порядок не застрял.
    assert_eq!(
        page.take_sent(),
        vec![PlayerSessionMessage::Seek {
            seek_id: SeekId(1),
            target_us: micros(5_000_000),
        }]
    );
    assert!(!outbound.has_pending());
}

#[test]
fn a_new_session_forgets_messages_meant_for_the_old_page_but_never_reuses_ids() {
    let (mut outbound, page) = outbound();
    let start = Instant::now();
    page.refuse_next(SendToPageError::QueueFull);
    outbound.reject_generation(generation(3), StreamRejection::UnsupportedCodec);
    outbound.request_page_seek(PageSeekRequest {
        target: Duration::from_secs(1),
    });
    outbound.flush(start);
    outbound.reset_for_new_session();
    assert!(!outbound.has_pending());

    // В новом сеансе поколения снова с 1 — отказ от него не считается повтором.
    assert!(outbound.reject_generation(generation(1), StreamRejection::UnsupportedCodec));
    let seek_id = outbound.request_page_seek(PageSeekRequest {
        target: Duration::from_secs(2),
    });
    assert_eq!(seek_id, SeekId(2));
    outbound.flush(start);
    assert_eq!(page.take_sent().len(), 2);
}
