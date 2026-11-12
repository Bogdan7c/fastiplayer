//! `play` страницы после конца ролика (16a2): у доигравшего медиа плееру не уходит, воля
//! «играть» достаётся следующему поколению; повтор перемоткой страницы и `play` зрителя
//! работают как раньше; устаревшее наблюдение «у конца» не глотает `play` после перемотки
//! или паузы той же пачки.
//!
//! Перемотка страницы после конца (16a3, находка 3): у доигравшего поколения она
//! откладывается до `play` страницы (повтор), а новая серия или «жду следующую серию» её
//! выбрасывают — прошлая серия не играет с начала, пока страница несёт следующую.

use browser_bridge_protocol::session::NextEpisodePatience;

use super::*;
use crate::browser_session::intake_notice::PageEpisodeWord;
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

/// Поколение 1 доиграло (`Ended`), пока страница ИГРАЛА — как в живом случае: воля
/// «играть» после конца у плеера по-прежнему «играть». Команды плееру сброшены.
fn first_generation_played_to_the_end() -> (Harness, SessionRef, PushedStreamWriter) {
    let (mut harness, current, writer) = loaded_harness();
    let first_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(first_load, FIXTURE_CLIP, PlaybackState::Ended);
    harness.player.observation.borrow_mut().ended = true;
    harness.media.drive(Instant::now());
    harness.player.take_calls();
    harness.page.take_sent();
    assert!(harness.media.page_wants_play, "the page was playing");
    (harness, current, writer)
}

/// Перемотка страницы на ноль (klonua делает её в такте `ended`, до смены источника).
fn page_seek_to_zero(harness: &mut Harness, current: &SessionRef) {
    harness.media.on_intake(
        vec![intent(current, PageIntent::FollowPageSeek(MediaTime::ZERO))],
        None,
    );
}

/// Страница сняла источник и ждёт следующую серию.
fn page_awaits_next_episode(harness: &mut Harness, current: &SessionRef) {
    harness.media.on_intake(
        vec![IntakeNotice::EpisodeChange {
            session: current.clone(),
            word: PageEpisodeWord::AwaitingNextEpisode(NextEpisodePatience::from_millis(60_000)),
        }],
        None,
    );
}

/// Команда плееру, которая сдвинула бы или запустила доигравшую серию.
fn moves_or_plays(call: &PlayerCall) -> bool {
    matches!(call, PlayerCall::Seek(_) | PlayerCall::Play)
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
    // Перемотка у конца отложена до `play`; `play` того же поколения — повтор: перемотка
    // и «играть» уходят плееру парой.
    assert_eq!(
        harness.player.take_calls(),
        vec![PlayerCall::Seek(Duration::ZERO), PlayerCall::Play]
    );
}

/// Перемотка отдана плееру проходом раньше (зрителем), а ответа ещё нет: наблюдение
/// по-прежнему «у конца», но плеер уже идёт к цели — `play` страницы ему нужен.
#[test]
fn a_page_play_after_an_unanswered_seek_reaches_the_player() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Ended);
    harness.player.seek_answers.set(FakeSeekAnswers::Withheld);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(Duration::ZERO), Instant::now());
    assert!(
        harness
            .player
            .take_calls()
            .contains(&PlayerCall::Seek(Duration::ZERO))
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

// ---- перемотка страницы после конца (16a3, находка 3) ----------------------------------

/// Главное (форма живого журнала klonua): серия доиграла, страница играла и сбросила элемент
/// на ноль, сняла источник и принесла следующую серию. Доигравшая серия плееру не
/// перематывается и не играет; следующая грузится играющей.
#[test]
fn a_page_seek_after_the_end_does_not_replay_the_finished_episode() {
    let (mut harness, current, _first_writer) = first_generation_played_to_the_end();

    page_seek_to_zero(&mut harness, &current);
    harness.media.drive(Instant::now());
    let after_seek = harness.player.take_calls();
    assert!(
        !after_seek.iter().any(moves_or_plays),
        "the finished episode was moved or played: {after_seek:?}"
    );

    page_awaits_next_episode(&mut harness, &current);
    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    let calls = harness.drive_until_loaded();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, PlayerCall::Load(_, LoadStart::Playing))),
        "the next episode did not start playing: {calls:?}"
    );
    let before_load: Vec<&PlayerCall> = calls
        .iter()
        .take_while(|call| !matches!(call, PlayerCall::Load(..)))
        .collect();
    assert!(
        !before_load.iter().any(|call| moves_or_plays(call)),
        "the finished episode was commanded before the next one loaded: {calls:?}"
    );
}

/// Страница сбросила элемент на ноль, сняла источник и сказала `play` раньше объявления
/// новой серии (порядок Tortuga): отложенная перемотка выброшена — это не повтор.
#[test]
fn a_held_page_seek_is_dropped_when_the_page_awaits_the_next_episode() {
    let (mut harness, current, _writer) = first_generation_played_to_the_end();
    page_seek_to_zero(&mut harness, &current);
    page_awaits_next_episode(&mut harness, &current);
    page_play(&mut harness, &current);
    harness.media.drive(Instant::now());
    let calls = harness.player.take_calls();
    assert!(
        !calls.iter().any(moves_or_plays),
        "the finished episode was replayed: {calls:?}"
    );
    assert!(
        harness.media.page_wants_play,
        "the will to play waits for the next episode"
    );
}

/// Новая серия объявлена (ещё не загружена) после перемотки: `play` страницы — для неё, не
/// повтор доигравшей.
#[test]
fn a_held_page_seek_is_dropped_by_a_new_generation() {
    let (mut harness, current, _first_writer) = first_generation_played_to_the_end();
    page_seek_to_zero(&mut harness, &current);
    let (_second_writer, second) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![second], None);
    page_play(&mut harness, &current);
    let calls = harness.player.take_calls();
    assert!(
        !calls.iter().any(moves_or_plays),
        "the finished episode was replayed: {calls:?}"
    );
}

/// Перемотка, пауза и `play` страницы (сайт сам повторяет ролик с паузой посередине): повтор
/// с отложенной цели.
#[test]
fn a_page_seek_pause_and_play_after_the_end_replay_from_the_held_target() {
    let (mut harness, current, _writer) = first_generation_played_to_the_end();
    let target = Duration::from_secs(1);
    harness.media.on_intake(
        vec![
            intent(
                &current,
                PageIntent::FollowPageSeek(MediaTime::from_micros(1_000_000).expect("time")),
            ),
            intent(&current, PageIntent::Pause),
            intent(&current, PageIntent::Play),
        ],
        None,
    );
    let calls = harness.player.take_calls();
    assert_eq!(calls.last(), Some(&PlayerCall::Play), "{calls:?}");
    assert!(calls.contains(&PlayerCall::Seek(target)), "{calls:?}");
    assert!(
        !calls.contains(&PlayerCall::Seek(Duration::ZERO)),
        "the replay ignored the page's target: {calls:?}"
    );
}

/// Пока хвост доигрывается (`Draining`), страница ещё видит играющий ролик: её перемотка —
/// настоящая и уходит плееру сразу.
#[test]
fn a_page_seek_while_the_tail_drains_reaches_the_player() {
    let (mut harness, current, _writer) = first_generation_at(PlaybackState::Draining);
    page_seek_to_zero(&mut harness, &current);
    assert!(
        harness
            .player
            .take_calls()
            .contains(&PlayerCall::Seek(Duration::ZERO))
    );
}

/// Зритель нажал «играть» после конца — повтор ядром, как в обычном плеере; отложенная
/// перемотка страницы устарела и позже не исполняется.
#[test]
fn a_viewer_play_drops_the_held_page_seek() {
    let (mut harness, current, _writer) = first_generation_played_to_the_end();
    page_seek_to_zero(&mut harness, &current);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Play, Instant::now());
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
    page_play(&mut harness, &current);
    assert!(
        !harness.player.take_calls().iter().any(moves_or_plays),
        "the stale page seek was carried out"
    );
}
