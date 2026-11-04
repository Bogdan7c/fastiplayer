//! Часы странице, пока поколение открывается и в плеере ещё ничего нет (сессия 09b2).
//!
//! Пульс тени бьётся только на тиках часов (решение 180), а фон кончает передачу после
//! 10 с тишины. Поэтому часы обязаны идти с той минуты, когда плееру есть что сказать, но
//! не обязаны двигать страницу: тень при передаче стоит на настоящей позиции элемента, и
//! тик «0» отмотал бы её назад. Здесь проверяются правила `page_clock`, плеер — подделка.

use super::generation_placement::{HOLD_CHECK, LONG_CLIP, generation_with_head};
use super::*;
use crate::browser_session::outbound::CLOCK_INTERVAL;

/// Где стоит элемент страницы при передаче (`page_seek{T0}`).
const PAGE_START: Duration = Duration::from_millis(200);
/// Цель, у которой данных нет (поток держит только начало ролика).
const GATE_TARGET: Duration = Duration::from_secs(300);

/// Сообщение страницы «я стою здесь».
fn page_names_start(session: &SessionRef, position: Duration) -> IntakeNotice {
    let micros = u64::try_from(position.as_micros()).expect("µs");
    let target = MediaTime::from_micros(micros).expect("time");
    intent(session, PageIntent::FollowPageSeek(target))
}

/// Сеанс 1: поколение 1 объявлено без байтов, страница назвала свою позицию. Писатель
/// держится тестом (иначе поток кончился бы, не начавшись).
fn opening_with_named_start() -> (Harness, SessionRef, PushedStreamWriter) {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (writer, generation) = stream_generation(&current, 1, false);
    let named_start = page_names_start(&current, PAGE_START);
    harness.media.on_intake(vec![generation, named_start], None);
    (harness, current, writer)
}

/// Четыре прохода с шагом тика; возвращает ушедшие странице часы.
fn tick_four_times(harness: &mut Harness, start: Instant) -> Vec<(u64, ClockState)> {
    for step in 0..4_u32 {
        harness.media.drive(start + CLOCK_INTERVAL * step);
    }
    clocks(&harness.page.take_sent())
}

/// Номера тиков из сообщений.
fn clock_sequences(messages: &[PlayerSessionMessage]) -> Vec<u32> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock { seq, .. } => Some(seq.0),
            _ => None,
        })
        .collect()
}

/// Главное: пока поколение открывается, страница слышит «жду» в своей позиции, а цикл
/// просыпается ради следующего тика, даже если открытие спрашивать ещё рано.
#[test]
fn an_opening_generation_keeps_the_clock_ticking_at_the_named_start() {
    let (mut harness, _current, _writer) = opening_with_named_start();
    let start = Instant::now() + Duration::from_secs(1);

    assert_eq!(
        tick_four_times(&mut harness, start),
        vec![(200_000, ClockState::Waiting); 4],
        "the page must hear `waiting` at its own position, not at zero"
    );
    let after_last_tick = start + CLOCK_INTERVAL * 3;
    let wake_at = harness.media.drive(after_last_tick).expect("a clock wake");
    assert!(
        wake_at <= after_last_tick + CLOCK_INTERVAL,
        "the loop must wake for the next tick"
    );
    assert!(
        harness.player.take_calls().is_empty(),
        "the clock must not command the player"
    );
}

/// `page_seek{T0}` идёт следом за `generation_started`: тик между ними сказал бы «0» и
/// отмотал бы тень от T0. Поэтому до слова страницы часов нет вовсе, а слово запускает
/// их сразу.
#[test]
fn no_clock_reaches_the_page_before_it_names_where_it_stands() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, generation) = stream_generation(&current, 1, false);
    harness.media.on_intake(vec![generation], None);
    let start = Instant::now() + Duration::from_secs(1);

    assert!(
        tick_four_times(&mut harness, start).is_empty(),
        "a tick before page_seek{{T0}} would drag the shadow to zero"
    );

    let named_start = page_names_start(&current, PAGE_START);
    harness.media.on_intake(vec![named_start], None);
    harness.media.drive(start + Duration::from_secs(1));
    assert_eq!(
        clocks(&harness.page.take_sent()),
        vec![(200_000, ClockState::Waiting)]
    );
}

/// Новый источник сменил ещё не открытое поколение и позицию не назвал (реклама): элемент
/// страницы уже в нуле нового источника, часы — тоже.
#[test]
fn a_new_source_replacing_the_opening_generation_waits_at_zero() {
    let (mut harness, current, _first_writer) = opening_with_named_start();
    let (_second_writer, second) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![second], None);
    let start = Instant::now() + Duration::from_secs(1);

    assert_eq!(
        tick_four_times(&mut harness, start),
        vec![(0, ClockState::Waiting); 4]
    );
}

/// Нет сеанса — некому и незачем: уведомления чужого экземпляра не становятся ждущим
/// поколением, цикл не просыпается.
#[test]
fn without_a_session_an_announced_generation_gets_no_clock() {
    let mut harness = Harness::new();
    let stranger = session(9);
    let (_writer, generation) = stream_generation(&stranger, 1, false);
    let named_start = page_names_start(&stranger, PAGE_START);
    harness.media.on_intake(vec![generation, named_start], None);

    assert_eq!(harness.media.drive(Instant::now()), None);
    assert!(harness.page.take_sent().is_empty());
}

/// Вкладка ушла, поколение так и открывается: страницы нет — часов нет.
#[test]
fn draining_silences_the_clock_of_an_opening_generation() {
    let (mut harness, current, _writer) = opening_with_named_start();
    let start = Instant::now() + Duration::from_secs(1);
    harness.media.drive(start);
    assert_eq!(clocks(&harness.page.take_sent()).len(), 1);

    harness.media.on_session_ended(&current, TAB_GONE_END);

    assert!(
        tick_four_times(&mut harness, start + Duration::from_secs(1)).is_empty(),
        "there is no page left to tell anything to"
    );
}

/// Новый сеанс: плеер всё ещё показывает видео прежней вкладки (30 с, конец). Ни его
/// позиция, ни `ended` новой странице не уходят.
#[test]
fn a_stale_generation_of_the_replaced_session_never_reaches_the_clock() {
    let (mut harness, _old_session, _old_writer) = loaded_harness();
    let old_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(old_load, Duration::from_secs(30), PlaybackState::Ended);
    harness.player.observation.borrow_mut().ended = true;

    let replacing = session(2);
    harness.media.on_session_opened(&replacing);
    harness.player.take_calls();
    let (_writer, generation) = stream_generation(&replacing, 1, false);
    let named_start = page_names_start(&replacing, PAGE_START);
    harness.media.on_intake(vec![generation, named_start], None);
    let start = Instant::now() + Duration::from_secs(1);

    assert_eq!(
        tick_four_times(&mut harness, start),
        vec![(200_000, ClockState::Waiting); 4],
        "the replaced session's position or end leaked to the new page"
    );
}

/// Отказ снял единственное открытие: сказать странице больше нечего, часы встают.
#[test]
fn a_rejected_opening_generation_stops_the_clock() {
    let (mut harness, current, _writer) = opening_with_named_start();
    let start = Instant::now() + Duration::from_secs(1);
    harness.media.drive(start);
    harness.page.take_sent();

    harness.media.on_intake(
        vec![IntakeNotice::Reject {
            session: current,
            generation: generation(1),
            reason: StreamRejection::UnsupportedCodec,
        }],
        None,
    );

    assert!(tick_four_times(&mut harness, start + Duration::from_secs(1)).is_empty());
}

/// Ритм и номера общие на сеанс: два прохода одного тика дают одну отправку, а передача
/// поколения плееру не добавляет внеочередного тика и не сбивает нумерацию.
#[test]
fn opening_and_loading_share_one_clock_cadence_and_numbering() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, generation) = stream_generation(&current, 1, true);
    let named_start = page_names_start(&current, PAGE_START);
    harness.media.on_intake(vec![generation, named_start], None);

    let start = Instant::now();
    harness.media.drive(start);
    harness.media.drive(start);
    let opening = harness.page.take_sent();
    assert_eq!(
        clocks(&opening).len(),
        1,
        "one tick per clock interval: {opening:?}"
    );

    harness.drive_until_loaded();
    let during_load = harness.page.take_sent();
    let next_tick = Instant::now() + CLOCK_INTERVAL;
    harness.media.drive(next_tick);
    let after_load = harness.page.take_sent();

    let sequences: Vec<u32> = [opening, during_load, after_load]
        .iter()
        .flat_map(|messages| clock_sequences(messages))
        .collect();
    let contiguous: Vec<u32> = (sequences[0]..).take(sequences.len()).collect();
    assert_eq!(
        sequences, contiguous,
        "clock numbering must stay contiguous"
    );
}

/// Поколение отдано плееру с названного места, плеер ещё ставит его (наблюдение — про
/// прежнее): страница по-прежнему ждёт на своём месте, а не проваливается в ноль.
#[test]
fn an_installing_generation_with_a_named_start_stays_at_the_named_start() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, generation) = stream_generation(&current, 1, true);
    let named_start = page_names_start(&current, PAGE_START);
    harness.media.on_intake(vec![generation, named_start], None);
    harness.drive_until_loaded();
    harness.page.take_sent();
    let start = Instant::now() + Duration::from_secs(1);

    assert_eq!(
        tick_four_times(&mut harness, start),
        vec![(200_000, ClockState::Waiting); 4]
    );
}

/// Перемотка страницы после загрузки, но до показа, новее стартовой позиции: часы
/// установки говорят её.
#[test]
fn a_page_seek_during_install_moves_the_waiting_clock() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, generation) = stream_generation(&current, 1, true);
    let named_start = page_names_start(&current, PAGE_START);
    harness.media.on_intake(vec![generation, named_start], None);
    harness.drive_until_loaded();
    harness.page.take_sent();

    let later = page_names_start(&current, Duration::from_millis(300));
    harness.media.on_intake(vec![later], None);
    let start = Instant::now() + Duration::from_secs(1);

    assert_eq!(
        tick_four_times(&mut harness, start),
        vec![(300_000, ClockState::Waiting); 4]
    );
}

/// Плеер показал поколение и в том же проходе получил перемотку к цели: наблюдение ещё про
/// начало медиа, а страница стоит на цели. Тик этого прохода не отматывает её в ноль.
#[test]
fn the_pass_that_commands_the_seek_after_show_keeps_the_page_at_the_target() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, generation) = stream_generation(&current, 1, true);
    let named_start = page_names_start(&current, PAGE_START);
    harness.media.on_intake(vec![generation, named_start], None);
    harness.drive_until_loaded();
    harness.page.take_sent();
    let later = page_names_start(&current, Duration::from_millis(300));
    harness.media.on_intake(vec![later], None);

    let load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(load, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(Instant::now() + Duration::from_secs(1));

    assert_eq!(
        harness.player.take_calls().first(),
        Some(&PlayerCall::Seek(Duration::from_millis(300))),
        "the deferred page seek goes to the player when it shows the generation"
    );
    assert_eq!(
        clocks(&harness.page.take_sent()),
        vec![(300_000, ClockState::Waiting)]
    );
}

/// Первое поколение открыто, но ворота держат его до данных у цели: пока плеер ждёт, часы
/// идут в цели, а цикл просыпается к следующему тику.
#[test]
fn a_first_generation_held_by_the_gate_keeps_the_clock_at_its_target() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, generation) =
        generation_with_head(&current, 1, PushedDuration::Finite(LONG_CLIP));
    let named_start = page_names_start(&current, GATE_TARGET);
    harness.media.on_intake(vec![generation, named_start], None);

    // Открытие потока — доли секунды; держим цикл, пока поток не окажется у ворот.
    let deadline = Instant::now() + HOLD_CHECK;
    while Instant::now() < deadline {
        harness.media.drive(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        harness.player.take_calls().is_empty(),
        "the gate must hold the generation"
    );

    let tick_at = Instant::now() + Duration::from_secs(1);
    harness.page.take_sent();
    let wake_at = harness.media.drive(tick_at).expect("a clock wake");
    assert_eq!(wake_at, tick_at + CLOCK_INTERVAL);
    assert_eq!(
        clocks(&harness.page.take_sent()),
        vec![(300_000_000, ClockState::Waiting)],
        "the page must wait at its own position"
    );
}

/// Страница на миг отключена: тик пропадает, но часы не встают и не копят долг.
#[test]
fn a_detached_page_loses_a_tick_and_the_next_one_arrives() {
    let (mut harness, _current, _writer) = opening_with_named_start();
    harness
        .page
        .refusals
        .borrow_mut()
        .push_back(SendToPageError::NoAttachedSession);
    let start = Instant::now() + Duration::from_secs(1);

    assert_eq!(
        tick_four_times(&mut harness, start),
        vec![(200_000, ClockState::Waiting); 3]
    );
}
