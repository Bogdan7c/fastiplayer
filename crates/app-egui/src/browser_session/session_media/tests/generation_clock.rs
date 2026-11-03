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
