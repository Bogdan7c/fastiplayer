//! Клик зрителя главнее самовольного возврата страницы (решение 149) сквозь медиа-часть:
//! плеер и страница подделаны, потоки — настоящий `pushed-media-source`.
//!
//! Сценарий стенда: поколение 1 играет, страница открывает поколение 2 (смена качества),
//! зритель кликает в X; страница исполняет клик, поколение 2 показывается у X — и тут
//! YouTube сам возвращает себя на место до смены (своя перемотка в Y).

use super::generation_placement::{
    LONG_CLIP, append_media_at, page_seek_targets, page_time, player_seeks,
    second_generation_opening, start_of_load,
};
use super::*;
use crate::viewer_transport::ViewerTransportIntent;

const CLICK: Duration = Duration::from_secs(300);
const PAGE_RETURN: Duration = Duration::from_secs(120);

/// Своя перемотка страницы к `target`, как её доставляет переходник: сначала источнику
/// (в порядке провода), потом медиа-части.
fn page_seeks_itself(
    harness: &mut Harness,
    session: &SessionRef,
    writer: &mut PushedStreamWriter,
    target: Duration,
) {
    writer.note_page_seek(target);
    harness.media.on_intake(
        vec![intent(
            session,
            PageIntent::FollowPageSeek(page_time(target)),
        )],
        None,
    );
}

/// Клик в X во время смены поколения, страница подчинилась (байты у X), поколение 2
/// показано у X. Возвращает команды плееру после показа.
fn click_obeyed_and_shown() -> (Harness, SessionRef, PushedStreamWriter, PushedStreamWriter) {
    let (mut harness, current, first, mut second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(CLICK), Instant::now());
    append_media_at(&mut second, CLICK);
    harness.drive_until_loaded();
    assert_eq!(start_of_load(&harness, 1), Some(CLICK));
    harness.show_last_load(CLICK);
    harness.player.take_calls();
    (harness, current, first, second)
}

/// Возврат страницы сразу после показа (в окне подтверждения): плеер не уходит за ней,
/// а повторяет клик — странице `seek` X и плеер к X. Страница вернулась снова —
/// остаёмся у неё, третьего `seek` нет.
#[test]
fn a_page_return_right_after_the_show_is_answered_by_one_repeat_and_then_followed() {
    let (mut harness, current, _first, mut second) = click_obeyed_and_shown();

    page_seeks_itself(&mut harness, &current, &mut second, PAGE_RETURN);
    let after_return = harness.drive_for(Duration::from_millis(100));
    assert_eq!(
        player_seeks(&after_return),
        vec![CLICK],
        "the player repeats the click instead of following the page back"
    );
    assert_eq!(
        page_seek_targets(&harness.page.take_sent()),
        vec![CLICK, CLICK],
        "the click and exactly one repeat"
    );

    page_seeks_itself(&mut harness, &current, &mut second, PAGE_RETURN);
    let after_second_return = harness.drive_for(Duration::from_millis(100));
    assert_eq!(
        player_seeks(&after_second_return),
        vec![PAGE_RETURN],
        "the page disobeyed twice: the player stays where the page is"
    );
    assert!(
        page_seek_targets(&harness.page.take_sent()).is_empty(),
        "no third seek to the page"
    );
}

/// Эхо клика после показа (страница перемоталась рядом с X) — не возврат: ничего не
/// повторяется, плеер следует как обычно.
#[test]
fn an_echo_of_the_click_after_the_show_repeats_nothing() {
    let (mut harness, current, _first, mut second) = click_obeyed_and_shown();
    let echo = CLICK + Duration::from_millis(150);

    page_seeks_itself(&mut harness, &current, &mut second, echo);
    let calls = harness.drive_for(Duration::from_millis(100));
    assert_eq!(player_seeks(&calls), vec![echo]);
    assert_eq!(page_seek_targets(&harness.page.take_sent()), vec![CLICK]);
}

/// Находка ревью: эхо клика после показа, потом окно подтверждения проходит — поздняя
/// своя перемотка страницы (глава, реклама, автопереход) просто исполняется, старый
/// клик не повторяется ни странице, ни плееру.
#[test]
fn an_echo_after_the_show_then_the_window_passes_and_a_late_page_seek_is_followed() {
    let (mut harness, current, _first, mut second) = click_obeyed_and_shown();
    let echo = CLICK + Duration::from_millis(150);
    page_seeks_itself(&mut harness, &current, &mut second, echo);
    harness.drive_for(
        crate::browser_session::session_media::viewer_transport::VIEWER_WILL_CONFIRMATION_WINDOW
            + Duration::from_millis(100),
    );
    harness.player.take_calls();
    harness.page.take_sent();

    page_seeks_itself(&mut harness, &current, &mut second, PAGE_RETURN);
    let calls = harness.drive_for(Duration::from_millis(100));
    assert_eq!(player_seeks(&calls), vec![PAGE_RETURN]);
    assert!(
        page_seek_targets(&harness.page.take_sent()).is_empty(),
        "the old click is not repeated"
    );
}

/// Окно подтверждения после показа прошло: своя перемотка страницы — снова её воля
/// (реклама, автопереход), клик не повторяется.
#[test]
fn after_the_confirmation_window_the_page_leads_again() {
    let (mut harness, current, _first, mut second) = click_obeyed_and_shown();
    harness.drive_for(
        crate::browser_session::session_media::viewer_transport::VIEWER_WILL_CONFIRMATION_WINDOW
            + Duration::from_millis(100),
    );
    harness.player.take_calls();

    page_seeks_itself(&mut harness, &current, &mut second, PAGE_RETURN);
    let calls = harness.drive_for(Duration::from_millis(100));
    assert_eq!(player_seeks(&calls), vec![PAGE_RETURN]);
    assert_eq!(page_seek_targets(&harness.page.take_sent()), vec![CLICK]);
}

/// Клик без смены поколения — обычная перемотка: возврат страницы потом просто
/// исполняется (никакой претензии).
#[test]
fn a_click_without_a_generation_switch_claims_nothing() {
    let (mut harness, current, mut first) = loaded_harness();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(CLICK), Instant::now());
    harness.drive_for(Duration::from_millis(50));
    harness.player.take_calls();
    harness.page.take_sent();

    page_seeks_itself(&mut harness, &current, &mut first, PAGE_RETURN);
    let calls = harness.drive_for(Duration::from_millis(100));
    assert_eq!(player_seeks(&calls), vec![PAGE_RETURN]);
    assert!(page_seek_targets(&harness.page.take_sent()).is_empty());
}

/// Поколение, во время смены на которое кликнул зритель, отвергнуто: претензии клика
/// больше нет — повторить его после показа какого-то следующего поколения было бы чужой
/// волей.
#[test]
fn a_rejected_switch_drops_the_click_claim() {
    let (mut harness, _current, _first, _second) =
        second_generation_opening(PushedDuration::Finite(LONG_CLIP));
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(CLICK), Instant::now());
    let second = StreamGeneration::new(2).expect("generation");
    harness.media.reject(
        second,
        StreamRejection::PlaybackFailed,
        crate::browser_session::session_media::generation_rejection::ViewerAwareness::Unaware,
    );

    let verdict = harness
        .media
        .viewer_will
        .click_claim()
        .judge_page_seek(PAGE_RETURN, |_, _| false);
    assert_eq!(
        verdict,
        crate::browser_session::session_media::viewer_click_claim::PageSeekAgainstClick::Unclaimed
    );
}
