//! Ворота нового поколения, продолжение 04d: клик зрителя в эфире во время смены
//! поколения (решение владельца — клик исполняется после показа), цель зрителя при второй
//! смене качества до передачи (требование 147) и уход страницы, когда данных нет нигде.
//!
//! Плеер и страница подделаны, потоки — настоящий `pushed-media-source`; помощники — из
//! соседнего `generation_placement`.

use super::generation_placement::{
    HOLD_CHECK, LONG_CLIP, LONG_WAIT, VIDEO_INIT_END, append_media_at, generation_with_head,
    init_end, page_seek_targets, page_time, player_seeks, second_generation_opening, start_of_load,
    with_wait,
};
use super::*;
use crate::browser_session::session_media::generation_placement::GenerationWait;
use crate::viewer_transport::ViewerTransportIntent;

/// Поколение с объявленными полосами и только init-сегментами: медиа нет нигде.
fn generation_with_init_only(
    session: &SessionRef,
    number: u32,
    duration: PushedDuration,
) -> (PushedStreamWriter, IntakeNotice) {
    let (mut writer, notice) = stream_generation(session, number, false);
    writer.set_duration(duration);
    let video = fixture_media("video.mp4");
    let audio = fixture_media("audio.mp4");
    for (lane, init) in [
        (0, &video[..VIDEO_INIT_END]),
        (1, &audio[..init_end(&audio)]),
    ] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, init).expect("init");
    }
    (writer, notice)
}

// ---- эфир -----------------------------------------------------------------------

/// Решение владельца (04d, открытый вопрос 1 → вариант b): клик зрителя в эфире, пока
/// новое поколение открывается, не теряется. Эфир не позиционируется (03e), поэтому
/// поколение уходит плееру сразу, с живого входа, а клик исполняется обычной перемоткой
/// плеера, как только поколение показано. Странице — ровно один `seek` на клик: при
/// установке и при перемотке после показа второй просьбы нет.
#[test]
fn a_viewer_click_on_a_live_switch_is_performed_once_the_generation_is_shown() {
    let (mut harness, _current, _first, _second) = second_generation_opening(PushedDuration::Live);
    let target = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    let calls = harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), None, "live starts at its join");
    assert!(
        player_seeks(&calls).is_empty(),
        "not to the old media before the install: {calls:?}"
    );

    harness.show_last_load(Duration::ZERO);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(Duration::from_millis(100)));
    assert_eq!(
        player_seeks(&after_show),
        vec![target],
        "the click is performed exactly once after the show"
    );
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![target],
        "one page seek for the click, none for the seek after the show"
    );
}

/// Клик в эфире туда, где у нового поколения данные уже есть: страницу не просят вовсе,
/// а клик всё равно исполняется после показа.
#[test]
fn a_live_click_already_on_the_shelf_asks_the_page_nothing_and_is_still_performed() {
    let (mut harness, _current, _first, mut second) =
        second_generation_opening(PushedDuration::Live);
    let target = Duration::from_secs(300);
    append_media_at(&mut second, target);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    harness.drive_until_loaded();
    harness.show_last_load(Duration::ZERO);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(Duration::from_millis(100)));

    assert_eq!(player_seeks(&after_show), vec![target]);
    assert!(page_seek_targets(&harness.page.take_sent()).is_empty());
}

/// После клика в эфире страница перемоталась сама в другое место: эфир начинается там,
/// где страница положила байты, а после показа клик зрителя повторяется один раз —
/// странице и плееру (решение 149 действует и в эфире: клик внутри окна DVR
/// исполняется, решение 148).
#[test]
fn a_page_seek_after_a_live_click_is_answered_by_one_repeat_of_the_click() {
    let (harness, current, _first) = loaded_harness();
    let mut harness = with_wait(harness, LONG_WAIT);
    let (mut second, notice) = generation_with_init_only(&current, 2, PushedDuration::Live);
    harness.media.on_intake(vec![notice], None);
    let click = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(click), Instant::now());
    let page_target = Duration::from_secs(120);
    second.note_page_seek(page_target);
    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::FollowPageSeek(page_time(page_target)),
        )],
        None,
    );
    append_media_at(&mut second, page_target);
    harness.drive_until_loaded();
    harness.show_last_load(page_target);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(Duration::from_millis(100)));

    assert_eq!(player_seeks(&after_show), vec![click]);
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![click, click]
    );
}

/// Поток без длительности (не эфир) неперематываем (ограничение 03b): плеер отверг бы
/// перемотку, поэтому клик, как до 04d, не исполняется — страницу попросили один раз.
#[test]
fn a_click_on_a_switch_of_unknown_duration_is_not_performed_by_the_player() {
    let (mut harness, _current, _first, _second) =
        second_generation_opening(PushedDuration::Unknown);
    let target = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    harness.drive_until_loaded();
    harness.show_last_load(Duration::ZERO);
    let mut after_show = harness.player.take_calls();
    after_show.extend(harness.drive_for(Duration::from_millis(100)));

    assert!(player_seeks(&after_show).is_empty());
    assert_eq!(page_seek_targets(&harness.page.take_sent()), vec![target]);
}

// ---- вторая смена качества до передачи --------------------------------------------

/// Требование 147, находка ревью: клик ждёт данных на поколении 2, а страница успевает
/// открыть поколение 3 (ещё одна смена качества). Цель зрителя переходит к поколению 3
/// без новой просьбы странице; страница кладёт байты только у клика — поколение 3 встаёт
/// там. Ожидание видно всё это время.
#[test]
fn a_viewer_click_survives_a_second_quality_change_before_the_hand_over() {
    let (mut harness, current, _first, _second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let target = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(target), Instant::now());
    harness.drive_for(Duration::from_millis(100));

    let (mut third, third_notice) =
        generation_with_head(&current, 3, PushedDuration::Finite(LONG_CLIP));
    harness.media.on_intake(vec![third_notice], None);
    let calls = harness.drive_for(HOLD_CHECK);
    assert_eq!(
        harness.load_count(),
        1,
        "handed over without data: {calls:?}"
    );
    assert_eq!(
        harness.media.generation_wait(),
        GenerationWait::AwaitingDataAtTarget,
        "the carried click is still awaited"
    );

    append_media_at(&mut third, target);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), Some(target));
    harness.show_last_load(target);
    harness.drive_for(Duration::from_millis(100));
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![target],
        "one page seek in total: the carry-over and the install never ask again"
    );
}

/// Перенесённая цель зрителя и потом своя перемотка страницы на поколении 3 в другое
/// место: последняя побеждает — поколение 3 встаёт у страницы.
#[test]
fn a_page_seek_on_the_replacing_generation_overrides_the_carried_click() {
    let (mut harness, current, _first, _second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    let click = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(click), Instant::now());
    harness.drive_for(Duration::from_millis(100));

    let (mut third, third_notice) =
        generation_with_head(&current, 3, PushedDuration::Finite(LONG_CLIP));
    let page_target = Duration::from_secs(120);
    third.note_page_seek(page_target);
    harness.media.on_intake(
        vec![
            third_notice,
            intent(&current, PageIntent::FollowPageSeek(page_time(page_target))),
        ],
        None,
    );
    append_media_at(&mut third, page_target);
    harness.drive_until_loaded();

    assert_eq!(start_of_load(&harness, 1), Some(page_target));
    assert_eq!(page_seek_targets(&harness.page.take_sent()), vec![click]);
}

// ---- страница ушла, данных нет нигде -------------------------------------------------

/// Страница ушла, пока поколение ждало клика, а данных нет нигде, откуда можно начать
/// (звуковая полоса пуста): ворота не держат сеанс вечно — доигрывание кончается по
/// сроку застоя, и окно получает конец «вкладка закрыта — застой».
#[test]
fn a_page_gone_with_no_data_anywhere_ends_through_the_drain_timeout() {
    let (harness, current, _first) = loaded_harness();
    let mut harness = with_wait(harness, LONG_WAIT);
    let (mut second, notice) =
        generation_with_init_only(&current, 2, PushedDuration::Finite(LONG_CLIP));
    // Видео есть (начало ролика), звука нет нигде: общего старта нет.
    second
        .append(
            LaneId::new(0),
            &fixture_media("video.mp4")[VIDEO_INIT_END..],
        )
        .expect("video");
    harness.media.on_intake(vec![notice], None);
    harness.media.on_viewer_transport(
        ViewerTransportIntent::Seek(Duration::from_secs(300)),
        Instant::now(),
    );
    harness.drive_for(HOLD_CHECK);
    assert_eq!(
        harness.load_count(),
        1,
        "no data anywhere: nothing handed over"
    );

    harness.media.on_session_ended(&current, TAB_GONE_END);
    harness.media.on_intake(
        vec![IntakeNotice::InstanceSealed {
            session: current.clone(),
        }],
        None,
    );
    let start = Instant::now();
    harness.media.drive(start);
    assert!(matches!(
        harness.media.playback_drain(start),
        DrainProgress::Draining { .. }
    ));
    let much_later = start + Duration::from_secs(120);
    harness.media.drive(much_later);
    assert_eq!(
        harness.media.playback_drain(much_later),
        DrainProgress::Drained(DrainEnd::Stalled)
    );
    assert!(
        matches!(
            harness.media.session_end_view(),
            crate::browser_session::session_end::SessionEndView::Finished(_)
        ),
        "the window is told the session finished"
    );
    assert_eq!(
        harness.load_count(),
        1,
        "still nothing handed over without data"
    );
}

// ---- что видят часы и окно, пока поколение не отдано --------------------------------

/// Часы странице и спиннер окна показывают ожидание только того, что действительно ждут.
/// Своя перемотка страницы до открытия потока — обычная смена качества: часы, как до 04d,
/// идут от прежнего видео, спиннера нет. Клик зрителя до открытия — ждут наверняка
/// (ворота или перемотка после показа): часы «жду» в клике, окно — спиннер.
#[test]
fn only_what_is_really_awaited_makes_the_page_clock_and_the_window_wait() {
    let (harness, current, _first) = loaded_harness();
    let mut harness = with_wait(harness, LONG_WAIT);
    // Поколение 2 объявлено, байтов нет вовсе: поток не открывается.
    let (mut second, notice) = stream_generation(&current, 2, false);
    second.set_duration(PushedDuration::Finite(LONG_CLIP));
    for lane in [0, 1] {
        second
            .add_lane(LaneId::new(lane), PushedContainer::FragmentedMp4)
            .expect("lane");
    }
    let first_load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(first_load, Duration::from_secs(2), PlaybackState::Playing);
    let page_target = Duration::from_secs(120);
    second.note_page_seek(page_target);
    harness.media.on_intake(
        vec![
            notice,
            intent(&current, PageIntent::FollowPageSeek(page_time(page_target))),
        ],
        None,
    );
    harness.drive_for(Duration::from_millis(600));
    assert_eq!(harness.media.generation_wait(), GenerationWait::Idle);
    let clocks_before_click = clocks(&harness.page.take_sent());
    assert!(
        !clocks_before_click.is_empty()
            && clocks_before_click
                .iter()
                .all(|clock| *clock == (2_000_000, ClockState::Playing)),
        "a page seek before the stream opens keeps the old video's clock: {clocks_before_click:?}"
    );

    let click = Duration::from_secs(300);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(click), Instant::now());
    harness.drive_for(Duration::from_millis(600));
    assert_eq!(
        harness.media.generation_wait(),
        GenerationWait::AwaitingDataAtTarget
    );
    let clocks_after_click = clocks(&harness.page.take_sent());
    assert!(
        !clocks_after_click.is_empty()
            && clocks_after_click
                .iter()
                .all(|clock| *clock == (300_000_000, ClockState::Waiting)),
        "the viewer click is awaited at its target: {clocks_after_click:?}"
    );
}
