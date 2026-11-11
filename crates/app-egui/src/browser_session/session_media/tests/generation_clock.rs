//! Часы странице при смене источника без цели (сессия 09a2, дыра Д-4).
//!
//! Новый `MediaSource` страницы без её перемотки (реклама, пересоздание плеера) —
//! новое поколение с правилом «с первого общего момента», цели у него нет. Пока оно не
//! показано, плеер ещё играет ПРЕЖНЕЕ поколение — но элемент страницы уже стоит в новом
//! источнике в нуле и ждёт данных. Раньше странице уходили часы прежнего поколения
//! (позиция фильма, `playing`, после отсечки — `ended`), и тень через ~2 с принимала их:
//! реклама «кончалась», не начавшись (нарушение решения 84). Правило: пока такое
//! поколение не показано, странице идут часы «0 / жду», как у ставящегося поколения.

use super::*;
use browser_bridge_protocol::ProtocolVersion;

use crate::browser_session::outbound::CLOCK_INTERVAL;

/// Подаёт в поток целый ролик фикстуры (видео + звук), длительность и конец потока —
/// «страница наконец положила данные нового источника».
fn deliver_fixture_clip(writer: &mut PushedStreamWriter) {
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
    writer.set_duration(PushedDuration::Finite(FIXTURE_CLIP));
    writer.end_of_stream().expect("end of stream");
}

/// Сеанс 1: поколение 1 показано и играет на 30 с; поколение 2 объявлено страницей без
/// перемотки и без байтов — ждёт данных. Писатели держатся тестом.
fn new_source_without_target_awaits_data() -> (Harness, [PushedStreamWriter; 2]) {
    let (mut harness, current, first_writer) = loaded_harness();
    let first_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(first_load, Duration::from_secs(30), PlaybackState::Playing);
    let (second_writer, second) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![second], None);
    (harness, [first_writer, second_writer])
}

/// Несколько проходов с шагом тика часов; плеер всё ещё показывает поколение 1 в
/// `position`, `state`. Возвращает часы, ушедшие странице.
fn tick_while_old_generation_shows(
    harness: &mut Harness,
    start: Instant,
    position: Duration,
    state: PlaybackState,
) -> Vec<(u64, ClockState)> {
    {
        let mut observation = harness.player.observation.borrow_mut();
        observation.position = position;
        observation.state = state;
        observation.ended = state == PlaybackState::Ended;
    }
    for step in 0..4_u32 {
        harness.media.drive(start + CLOCK_INTERVAL * step);
    }
    clocks(&harness.page.take_sent())
}

/// Главное: позиция и `playing` прежнего поколения странице не уходят, пока новое
/// поколение без цели не показано; после показа — часы нового поколения.
#[test]
fn a_new_source_without_a_target_gets_waiting_at_zero_until_it_is_shown() {
    let (mut harness, [_first_writer, mut second_writer]) = new_source_without_target_awaits_data();
    let start = Instant::now() + Duration::from_secs(1);

    let while_awaiting = tick_while_old_generation_shows(
        &mut harness,
        start,
        Duration::from_secs(30),
        PlaybackState::Playing,
    );
    assert!(!while_awaiting.is_empty(), "the page clock keeps ticking");
    assert!(
        while_awaiting
            .iter()
            .all(|clock| *clock == (0, ClockState::Waiting)),
        "the old generation's clock leaked into the new source: {while_awaiting:?}"
    );
    // Прежнее видео продолжает играть у зрителя: команд плееру нет.
    assert!(harness.player.take_calls().is_empty());

    // Данные нового источника пришли: поколение уходит плееру, плеер его показывает.
    deliver_fixture_clip(&mut second_writer);
    harness.drive_until_loaded();
    harness.page.take_sent();
    let second_load = harness.player.load_ticket(1);
    harness.player.show_load(
        second_load,
        Duration::from_millis(40),
        PlaybackState::Playing,
    );
    harness.media.drive(start + Duration::from_secs(10));
    assert_eq!(
        clocks(&harness.page.take_sent()),
        vec![(40_000, ClockState::Playing)],
        "once shown, the page sees the new generation's own clock"
    );
}

/// Прежнее поколение дошло до своей отсечки (заменённое кончается обрывом, решение 146)
/// и плеер говорит `Ended`: страница этого конца НЕ видит, пока новый источник не
/// показан, — иначе тень подняла бы `ended` на рекламе.
#[test]
fn the_end_of_the_replaced_generation_never_reaches_the_new_source() {
    let (mut harness, _writers) = new_source_without_target_awaits_data();
    let start = Instant::now() + Duration::from_secs(1);

    let after_cut = tick_while_old_generation_shows(
        &mut harness,
        start,
        Duration::from_millis(31_500),
        PlaybackState::Ended,
    );
    assert!(!after_cut.is_empty(), "the page clock keeps ticking");
    assert!(
        after_cut
            .iter()
            .all(|clock| *clock == (0, ClockState::Waiting)),
        "the replaced generation's end reached the new source: {after_cut:?}"
    );
}

/// Прощание согласовано с часами: пока новый источник без цели не показан, позиции для
/// `return_to_page` нет (позиция фильма в рекламе отмотала бы её не туда).
#[test]
fn a_new_source_without_a_target_gives_no_farewell_position() {
    let (mut harness, _writers) = new_source_without_target_awaits_data();
    let current = session(1);
    harness.media.drive(Instant::now() + Duration::from_secs(1));

    harness.media.bid_farewell_to_replaced(&current);

    assert!(
        harness.closer.farewells.borrow().is_empty(),
        "a farewell carried the old generation's position into the new source"
    );
}

// ---- номер поколения в часах (протокол 1.10, 16a2) ------------------------------------

/// Тики часов, ушедшие странице: (номер поколения в поле, позиция, состояние).
fn numbered_clocks(messages: &[PlayerSessionMessage]) -> Vec<(Option<u32>, u64, ClockState)> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                generation,
                position_us,
                state,
                ..
            } => Some((
                generation.map(StreamGeneration::get),
                position_us.as_micros(),
                *state,
            )),
            _ => None,
        })
        .collect()
}

/// Один тик (проход через интервал часов после `after`); возвращает ушедшие тики.
fn one_tick(harness: &mut Harness, after: Instant) -> Vec<(Option<u32>, u64, ClockState)> {
    harness.media.drive(after + CLOCK_INTERVAL);
    numbered_clocks(&harness.page.take_sent())
}

/// Правило 2 описывает ЖДУЩЕЕ поколение: пока плеер ещё играет поколение 1, «0 / жду» нового
/// источника несёт номер 2; до объявления и после показа — номер того, что показано.
#[test]
fn a_1_10_page_hears_which_generation_each_clock_describes() {
    let (mut harness, current, _first_writer) = loaded_harness();
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_10);
    let first_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(first_load, Duration::from_secs(30), PlaybackState::Playing);
    let start = Instant::now() + Duration::from_secs(1);
    assert_eq!(
        one_tick(&mut harness, start),
        vec![(Some(1), 30_000_000, ClockState::Playing)]
    );

    let (mut second_writer, second) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![second], None);
    assert_eq!(
        one_tick(&mut harness, start + CLOCK_INTERVAL),
        vec![(Some(2), 0, ClockState::Waiting)],
        "the new source's waiting clock must name the new generation"
    );

    deliver_fixture_clip(&mut second_writer);
    harness.drive_until_loaded();
    harness.page.take_sent();
    let second_load = harness.player.load_ticket(1);
    harness.player.show_load(
        second_load,
        Duration::from_millis(40),
        PlaybackState::Playing,
    );
    assert_eq!(
        one_tick(&mut harness, start + Duration::from_secs(10)),
        vec![(Some(2), 40_000, ClockState::Playing)]
    );
}

/// «Жду в цели» рождается двумя дорогами — номер у каждой свой: перемотка ЗАГРУЖЕННОГО
/// поколения без ответа плеера — номер загруженного (1), цель зрителя у ЖДУЩЕГО поколения —
/// номер ждущего (2).
#[test]
fn waiting_at_a_target_names_the_generation_that_waits() {
    let (mut harness, current, _first_writer) = loaded_harness();
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_10);
    let first_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(first_load, Duration::from_secs(30), PlaybackState::Playing);
    let start = Instant::now() + Duration::from_secs(1);

    // Загруженное поколение: перемотка страницы отдана, плеер ещё не ответил.
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);
    let target = MediaTime::from_micros(5_000_000).expect("time");
    harness.media.on_intake(
        vec![intent(&current, PageIntent::FollowPageSeek(target))],
        None,
    );
    assert_eq!(
        one_tick(&mut harness, start),
        vec![(Some(1), 5_000_000, ClockState::Waiting)]
    );
    harness.player.answer_seek();

    // Ждущее поколение: зритель перемотал, пока оно не открылось, — его цель ждут.
    let (_second_writer, second) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![second], None);
    harness.media.on_viewer_transport(
        crate::viewer_transport::ViewerTransportIntent::Seek(Duration::from_secs(8)),
        start,
    );
    assert_eq!(
        one_tick(&mut harness, start + CLOCK_INTERVAL),
        vec![(Some(2), 8_000_000, ClockState::Waiting)]
    );
}

/// Пока в плеере ничего нет, часы описывают открывающееся поколение (второе место отправки,
/// `tell_page_while_nothing_is_loaded`).
#[test]
fn the_opening_clock_names_the_opening_generation() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_10);
    let (_writer, first) = stream_generation(&current, 1, false);
    let start = MediaTime::from_micros(200_000).expect("time");
    harness.media.on_intake(
        vec![first, intent(&current, PageIntent::FollowPageSeek(start))],
        None,
    );
    assert_eq!(
        one_tick(&mut harness, Instant::now() + Duration::from_secs(1)),
        vec![(Some(1), 200_000, ClockState::Waiting)]
    );
}

/// Страница 1.9 номера не знает: те же часы без поля.
#[test]
fn a_1_9_page_hears_the_same_clock_without_the_generation() {
    let (mut harness, _writers) = new_source_without_target_awaits_data();
    harness
        .media
        .on_session_attached(&session(1), ProtocolVersion::V1_9);
    assert_eq!(
        one_tick(&mut harness, Instant::now() + Duration::from_secs(1)),
        vec![(None, 0, ClockState::Waiting)]
    );
}
