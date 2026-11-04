//! Часы страницы вокруг перемотки (живая проверка 09b2: список часов `[0, 597240083, …]`).
//!
//! Тень без своего удержания верит любому тику, поэтому часы обязаны говорить цель всё время,
//! пока страница уже на ней, а наблюдение плеера — ещё нет. Два таких окна:
//! - посадка у конца потока: ядро в `Draining`, позиция старая, цель есть;
//! - команда в очереди плеера: цели в наблюдении ещё нет вовсе.
//!
//! Плеер — подделка, она воспроизводит оба окна без расчёта на нагрузку.

use super::generation_placement::{
    LONG_CLIP, LONG_WAIT, append_media_at, generation_with_head, page_time, player_seeks, with_wait,
};
use super::*;
use crate::browser_session::outbound::CLOCK_INTERVAL;
use crate::browser_session::session_media::seek_handover::SEEK_ANSWER_PATIENCE;

/// T0 передачи: страница стоит за 3 с до конца ролика, как в записи живой проверки.
const PAGE_POSITION: Duration = Duration::from_secs(597);
const PAGE_POSITION_US: u64 = 597_000_000;
/// Другое место того же ролика.
const ELSEWHERE: Duration = Duration::from_secs(30);
/// Где плеер стоял до перемотки на показанном поколении.
const OLD_PLACE: Duration = Duration::from_secs(5);
/// Куда эфир на новой шкале садится на самом деле (`live_page_restart`).
const NEW_TIMELINE_LANDING: Duration = Duration::from_millis(89);

/// Проходы цикла с шагом тика часов: каждый проход обязан дать ровно один тик.
struct Ticks {
    next: Instant,
}

impl Ticks {
    /// Позже всех сроков, которые тест успел завести настоящим временем; часы подготовки
    /// (крутились настоящим временем) забыты.
    fn after_setup(harness: &Harness) -> Self {
        harness.page.take_sent();
        Self {
            next: Instant::now() + Duration::from_secs(1),
        }
    }

    /// Один проход; возвращает часы, ушедшие странице.
    fn pass(&mut self, harness: &mut Harness) -> Vec<(u64, ClockState)> {
        harness.media.drive(self.next);
        self.next += CLOCK_INTERVAL;
        clocks(&harness.page.take_sent())
    }
}

/// Страница назвала позицию (`seek_started{page_seek}`), как это делает переходник.
fn page_names_position(
    harness: &mut Harness,
    current: &SessionRef,
    writer: &mut PushedStreamWriter,
    position: Duration,
) {
    writer.note_page_seek(position);
    harness.media.on_intake(
        vec![intent(
            current,
            PageIntent::FollowPageSeek(page_time(position)),
        )],
        None,
    );
}

/// Передача с T0: поколение отдано плееру (ролик — уже на T0, эфир — там, где лежат байты).
fn handed_off_generation(duration: PushedDuration) -> (Harness, SessionRef, PushedStreamWriter) {
    let mut harness = with_wait(Harness::new(), LONG_WAIT);
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (mut writer, notice) = generation_with_head(&current, 1, duration);
    harness.media.on_intake(vec![notice], None);
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    append_media_at(&mut writer, PAGE_POSITION);
    harness.drive_until_loaded();
    (harness, current, writer)
}

/// Показанное поколение играет на `OLD_PLACE`; часы до перемотки уже ушли и забыты.
fn playing_at_the_old_place() -> (Harness, SessionRef, PushedStreamWriter, Ticks) {
    let (mut harness, current, writer) = loaded_harness();
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, OLD_PLACE, PlaybackState::Playing);
    let mut ticks = Ticks::after_setup(&harness);
    ticks.pass(&mut harness);
    (harness, current, writer, ticks)
}

/// Снимок записи: установка прошла, позиция — начало медиа, идёт посадка, а демуксер уже дошёл
/// до конца потока. Именно он дал тик «играю, 0».
fn landing_in_the_end_of_stream_drain(harness: &Harness) {
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::ZERO, PlaybackState::Draining);
    harness.player.observation.borrow_mut().seek_target = Some(PAGE_POSITION);
}

/// Посадка кончилась там, где плеер теперь и стоит.
fn landed_at(harness: &Harness, position: Duration) {
    let mut observation = harness.player.observation.borrow_mut();
    observation.seek_target = None;
    observation.position = position;
    observation.state = PlaybackState::Playing;
}

/// Главное: окно записи живой проверки. Каждый тик посадки — цель, и «жду», а не «играю».
#[test]
fn a_landing_that_reached_the_end_of_the_stream_keeps_the_clock_at_the_target() {
    let (mut harness, _current, _writer) = handed_off_generation(PushedDuration::Finite(LONG_CLIP));
    landing_in_the_end_of_stream_drain(&harness);
    let mut ticks = Ticks::after_setup(&harness);

    for _ in 0..3 {
        assert_eq!(
            ticks.pass(&mut harness),
            vec![(PAGE_POSITION_US, ClockState::Waiting)],
            "the shadow believes any tick: it must never hear the start of the media"
        );
    }

    landed_at(&harness, PAGE_POSITION);
    assert_eq!(
        ticks.pass(&mut harness),
        vec![(PAGE_POSITION_US, ClockState::Playing)],
        "after the commit the clock is the player again"
    );
}

/// Перемотка после показа (эфир, файл, T0 после загрузки): команда лежит в очереди плеера, а
/// следующий тик приходит раньше её исполнения. Цель держится не один проход, а до ответа.
#[test]
fn a_seek_after_the_show_keeps_the_clock_at_the_target_until_the_player_answers() {
    let (mut harness, current, mut writer) = handed_off_generation(PushedDuration::Live);
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);
    harness.player.take_calls();
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::ZERO, PlaybackState::Playing);
    let mut ticks = Ticks::after_setup(&harness);

    for _ in 0..3 {
        assert_eq!(
            ticks.pass(&mut harness),
            vec![(PAGE_POSITION_US, ClockState::Waiting)],
            "the observation is about the start of the media, the page already stands at T0"
        );
    }
    assert_eq!(
        player_seeks(&harness.player.take_calls()),
        vec![PAGE_POSITION]
    );
}

/// Ответ — не позиция: эфир на новой шкале законно садится далеко от цели, и этот настоящий
/// прыжок страница обязана услышать (никаких фильтров по значению часов).
#[test]
fn a_genuine_landing_far_from_the_target_is_spoken_after_the_answer() {
    let (mut harness, current, mut writer) = handed_off_generation(PushedDuration::Live);
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::ZERO, PlaybackState::Playing);
    let mut ticks = Ticks::after_setup(&harness);
    ticks.pass(&mut harness);

    harness.player.answer_seek();
    landed_at(&harness, NEW_TIMELINE_LANDING);
    assert_eq!(
        ticks.pass(&mut harness),
        vec![(89_000, ClockState::Playing)]
    );
}

/// Своя перемотка страницы на показанном поколении: цель слышна с первого тика, а новая
/// перемотка ждёт своего ответа, не чужого, уже виденного.
#[test]
fn every_seek_on_the_shown_generation_waits_for_its_own_answer() {
    let (mut harness, current, mut writer, mut ticks) = playing_at_the_old_place();
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);

    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    assert_eq!(
        ticks.pass(&mut harness),
        vec![(PAGE_POSITION_US, ClockState::Waiting)]
    );

    // Плеер ответил и отказал: стоит где стоял, и часы снова говорят его позицию.
    harness.player.answer_seek();
    assert_eq!(
        ticks.pass(&mut harness),
        vec![(5_000_000, ClockState::Playing)]
    );

    page_names_position(&mut harness, &current, &mut writer, ELSEWHERE);
    assert_eq!(
        ticks.pass(&mut harness),
        vec![(30_000_000, ClockState::Waiting)],
        "the answer to the previous seek says nothing about this one"
    );
}

/// Команда не дошла до плеера: ждать ответа не от кого, часы не застывают на цели.
#[test]
fn a_seek_the_player_refused_to_queue_is_not_awaited() {
    let (mut harness, current, mut writer, mut ticks) = playing_at_the_old_place();
    harness
        .player
        .seek_answers
        .set(FakeSeekAnswers::CommandRefused);

    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);

    assert_eq!(
        ticks.pass(&mut harness),
        vec![(5_000_000, ClockState::Playing)]
    );
    assert_eq!(
        player_seeks(&harness.player.take_calls()),
        Vec::<Duration>::new()
    );
}

/// Потерянный ответ (очередь событий плеера конечна) не вешает часы на цели навсегда.
#[test]
fn a_lost_answer_does_not_pin_the_clock_to_the_target_forever() {
    let (mut harness, current, mut writer, mut ticks) = playing_at_the_old_place();
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    let waiting = vec![(PAGE_POSITION_US, ClockState::Waiting)];
    assert_eq!(ticks.pass(&mut harness), waiting);

    ticks.next += SEEK_ANSWER_PATIENCE - CLOCK_INTERVAL * 2;
    assert_eq!(
        ticks.pass(&mut harness),
        waiting,
        "still within the patience"
    );

    ticks.next += CLOCK_INTERVAL;
    assert_eq!(
        ticks.pass(&mut harness),
        vec![(5_000_000, ClockState::Playing)]
    );
}

/// Долг принадлежит поколению: новое поколение начинает без чужой цели.
#[test]
fn a_new_generation_does_not_inherit_the_unanswered_seek_of_the_previous_one() {
    let (mut harness, current, mut writer, mut ticks) = playing_at_the_old_place();
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);
    page_names_position(&mut harness, &current, &mut writer, PAGE_POSITION);
    ticks.pass(&mut harness);

    let (_second_writer, notice) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    let second_load = harness.player.load_ticket(1);
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Playing);
    harness.page.take_sent();

    assert_eq!(ticks.pass(&mut harness), vec![(0, ClockState::Playing)]);
}
