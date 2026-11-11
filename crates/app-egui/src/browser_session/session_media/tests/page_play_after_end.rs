//! `play` страницы после конца ролика (16a2): у доигравшего медиа плееру не уходит, воля
//! «играть» достаётся следующему поколению; повтор перемоткой страницы и `play` зрителя
//! работают как раньше; устаревшее наблюдение «у конца» не глотает `play` после перемотки
//! или паузы той же пачки.

use super::*;
use crate::viewer_transport::ViewerTransportIntent;

/// Поколение 1 загружено, страница на паузе, плеер показывает его в `state` у конца
/// ролика; один проход — медиа-часть видела это наблюдение. Команды плееру сброшены.
fn first_generation_at(state: PlaybackState) -> (Harness, SessionRef, PushedStreamWriter) {
    let (mut harness, current, writer) = loaded_harness();
    // Страница на паузе: тогда видно, что воля «играть» — именно от её `play`.
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    let first_load = harness.player.load_ticket(0);
    harness.player.show_load(first_load, FIXTURE_CLIP, state);
    harness.player.observation.borrow_mut().ended = state == PlaybackState::Ended;
    harness.media.drive(Instant::now());
    harness.player.take_calls();
    harness.page.take_sent();
    (harness, current, writer)
}

fn page_play(harness: &mut Harness, current: &SessionRef) {
    harness
        .media
        .on_intake(vec![intent(current, PageIntent::Play)], None);
}

/// Главное: `play` страницы у `Ended` плееру не уходит (иначе ядро повторило бы серию 1 с
/// начала), а следующее поколение грузится уже играющим — воля страницы записана.
#[test]
fn a_page_play_after_the_end_starts_the_next_generation_not_a_replay() {
    let (mut harness, current, _first_writer) = first_generation_at(PlaybackState::Ended);

    page_play(&mut harness, &current);
    harness.media.drive(Instant::now());
    assert!(
        !harness.player.take_calls().contains(&PlayerCall::Play),
        "the finished episode was commanded to play again"
    );

    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    let calls = harness.drive_until_loaded();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, PlayerCall::Load(_, LoadStart::Playing))),
        "the next generation did not start playing: {calls:?}"
    );
    assert!(!calls.contains(&PlayerCall::Play), "{calls:?}");
}

/// То же во время доигрывания хвоста после EOF (`Draining`): ядро считает и это поводом
/// повтора с начала.
#[test]
fn a_page_play_while_the_tail_drains_is_not_sent_to_the_player() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Draining);
    page_play(&mut harness, &current);
    assert!(harness.player.take_calls().is_empty());
    assert!(harness.media.page_wants_play, "the will to play is kept");
}

/// Новое поколение уже отдано плееру, но ещё ставится: наблюдение про серию 1 у конца, и
/// команда досталась бы ей. `play` ждёт показа, а при показе плеер приводится к воле
/// страницы (`install_reconcile`).
#[test]
fn a_page_play_while_the_next_generation_installs_waits_for_its_show() {
    let (mut harness, current, _first_writer) = first_generation_at(PlaybackState::Ended);
    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    let calls = harness.drive_until_loaded();
    // Страница была на паузе — поколение 2 грузится на паузе.
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, PlayerCall::Load(_, LoadStart::Paused))),
        "{calls:?}"
    );

    page_play(&mut harness, &current);
    assert!(
        harness.player.take_calls().is_empty(),
        "the play reached the finished episode while the next one installs"
    );

    let second_load = harness.player.load_ticket(1);
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Paused);
    harness.player.observation.borrow_mut().ended = false;
    harness.media.drive(Instant::now());
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
}

/// Настоящий повтор приходит перемоткой страницы перед `play` (тень при `ended`), одной
/// пачкой, до следующего прохода: `play` уходит плееру следом за перемоткой.
#[test]
fn a_page_seek_to_zero_then_play_still_replays_the_same_generation() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Ended);
    harness.media.on_intake(
        vec![
            intent(&current, PageIntent::FollowPageSeek(MediaTime::ZERO)),
            intent(&current, PageIntent::Play),
        ],
        None,
    );
    assert_eq!(
        harness.player.take_calls(),
        vec![
            PlayerCall::Seek(Duration::ZERO),
            // Страница была на паузе: перемотка восстанавливает паузу...
            PlayerCall::Pause,
            // ...а `play` страницы следом — уже не у конца: повтор играет.
            PlayerCall::Play,
        ]
    );
}

/// Перемотка отдана плееру проходом раньше, а ответа ещё нет: наблюдение по-прежнему «у
/// конца», но плеер уже идёт к цели — `play` ему нужен.
#[test]
fn a_page_play_after_an_unanswered_seek_reaches_the_player() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Ended);
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);
    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::FollowPageSeek(MediaTime::ZERO),
        )],
        None,
    );
    harness.media.drive(Instant::now());
    harness.player.take_calls();

    page_play(&mut harness, &current);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
}

/// «Пауза + play» одной пачкой во время доигрывания хвоста: пауза вывела плеер из
/// доигрывания, устаревшее наблюдение `Draining` не глотает `play`.
#[test]
fn a_page_pause_then_play_while_draining_reaches_the_player() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Draining);
    harness.media.on_intake(
        vec![
            intent(&current, PageIntent::Pause),
            intent(&current, PageIntent::Play),
        ],
        None,
    );
    assert_eq!(
        harness.player.take_calls(),
        vec![PlayerCall::Pause, PlayerCall::Play]
    );
}

/// «Пауза + play» одной пачкой у `Ended` (сайт ставит паузу, меняет источник и играет):
/// пауза конец не снимает — `play` плееру не уходит, воля достаётся следующему поколению.
#[test]
fn a_page_pause_then_play_after_the_end_still_waits_for_the_next_generation() {
    let (mut harness, current, _first_writer) = first_generation_at(PlaybackState::Ended);
    harness.media.on_intake(
        vec![
            intent(&current, PageIntent::Pause),
            intent(&current, PageIntent::Play),
        ],
        None,
    );
    assert!(
        !harness.player.take_calls().contains(&PlayerCall::Play),
        "the finished episode was commanded to play again"
    );
    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    let calls = harness.drive_until_loaded();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, PlayerCall::Load(_, LoadStart::Playing))),
        "{calls:?}"
    );
}

/// Пометка команды живёт один проход: следующее наблюдение снова решает само. (Подделка
/// после паузы остаётся в `Draining` — так видно, что решает именно свежее наблюдение.)
#[test]
fn a_fresh_observation_at_the_end_gates_the_page_play_again() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Draining);
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    harness.media.drive(Instant::now());
    harness.player.take_calls();

    page_play(&mut harness, &current);
    assert!(harness.player.take_calls().is_empty());
}

/// Не у конца — `play` страницы как раньше: плеер получает команду.
#[test]
fn a_page_play_in_the_middle_reaches_the_player() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Paused);
    page_play(&mut harness, &current);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
}

/// `play` зрителя в нашем окне после конца — повтор, как в обычном плеере; эхо страницы
/// второй команды не даёт.
#[test]
fn a_viewer_play_after_the_end_still_replays() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Ended);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Play, Instant::now());
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
    page_play(&mut harness, &current);
    assert!(harness.player.take_calls().is_empty());
}
