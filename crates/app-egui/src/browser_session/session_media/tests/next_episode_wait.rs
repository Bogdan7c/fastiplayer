//! «Ждём следующую серию» и смена заголовка вкладки в медиа-части (протокол 1.9, 16a):
//! надпись снимает первый показ нового поколения или конец сеанса; слова страницы ничего
//! не меняют у плеера и страницы; слова чужого экземпляра не исполняются.

use browser_bridge_protocol::PageTitle;
use browser_bridge_protocol::session::{NextEpisodePatience, TabGoneReason};

use super::*;
use crate::browser_session::intake_notice::PageEpisodeWord;
use crate::browser_session::session_end::SessionEndView;

fn awaiting(session: &SessionRef) -> IntakeNotice {
    IntakeNotice::EpisodeChange {
        session: session.clone(),
        word: PageEpisodeWord::AwaitingNextEpisode(NextEpisodePatience::from_millis(60_000)),
    }
}

fn retitled(session: &SessionRef, title: &str) -> IntakeNotice {
    IntakeNotice::EpisodeChange {
        session: session.clone(),
        word: PageEpisodeWord::TitleChanged(PageTitle::new(title).expect("valid title")),
    }
}

/// Сообщения странице, кроме часов (их медиа-часть шлёт по своему расписанию).
fn sent_besides_clock(harness: &Harness) -> Vec<PlayerSessionMessage> {
    harness
        .page
        .take_sent()
        .into_iter()
        .filter(|message| !matches!(message, PlayerSessionMessage::Clock { .. }))
        .collect()
}

#[test]
fn the_wait_ends_when_the_next_generation_is_shown_and_touches_nothing_else() {
    let (mut harness, current, _first_writer) = loaded_harness();
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::NeverWaited
    );
    harness
        .media
        .on_intake(vec![awaiting(&current), awaiting(&current)], None);
    harness.media.drive(Instant::now());
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::Waiting
    );
    // Слово страницы — только надпись: плееру ни команды, странице ничего, конец не начат.
    assert_eq!(harness.player.take_calls(), Vec::new());
    assert_eq!(
        sent_besides_clock(&harness)
            .into_iter()
            .filter(|message| !matches!(message, PlayerSessionMessage::BufferState { .. }))
            .collect::<Vec<_>>(),
        Vec::new()
    );
    assert_eq!(harness.media.session_end_view(), SessionEndView::Silent);

    // Следующая серия: поколение 2 загружено, но ещё ставится — надпись держится.
    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    harness.drive_until_loaded();
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::Waiting
    );
    // Плеер показал поколение 2: надпись снята этим показом.
    let second_load = harness.player.load_ticket(1);
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(Instant::now());
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::GenerationShown)
    );
}

#[test]
fn the_wait_ends_with_the_session_and_a_late_word_does_not_restart_it() {
    let (mut harness, current, _writer) = loaded_harness();
    harness.media.on_intake(vec![awaiting(&current)], None);
    harness.media.on_session_ended(
        &current,
        SessionEndReason::TabGone {
            reason: TabGoneReason::Closed,
        },
    );
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::SessionEnded)
    );
    // Конец сеанса обогнал данные: запоздалое слово страницы надпись не возвращает.
    harness.media.on_intake(vec![awaiting(&current)], None);
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::SessionEnded)
    );
}

#[test]
fn a_replacing_tab_ends_the_wait_and_the_old_tab_cannot_start_a_new_one() {
    let (mut harness, old_tab, _writer) = loaded_harness();
    harness.media.on_intake(
        vec![awaiting(&old_tab), retitled(&old_tab, "Old episode")],
        None,
    );
    // Вкладку заменили, пока ядро не забрало заголовок: он к новой странице не относится.
    let new_tab = session(2);
    harness.media.on_session_opened(&new_tab);
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::SessionEnded)
    );
    assert_eq!(harness.media.take_page_retitle(), None);
    // Запоздалые слова старой вкладки не исполняются.
    harness.media.on_intake(
        vec![awaiting(&old_tab), retitled(&old_tab, "Old again")],
        None,
    );
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::SessionEnded)
    );
    assert_eq!(harness.media.take_page_retitle(), None);
}

#[test]
fn the_latest_page_title_is_handed_once_and_moves_nothing_else() {
    let (mut harness, current, _writer) = loaded_harness();
    assert_eq!(harness.media.take_page_retitle(), None);
    harness.media.on_intake(
        vec![retitled(&current, "Серія 2"), retitled(&current, "Серія 3")],
        None,
    );
    assert_eq!(
        harness.media.take_page_retitle(),
        Some(PageTitle::new("Серія 3").expect("valid title"))
    );
    assert_eq!(harness.media.take_page_retitle(), None);
    // Заголовок — только подпись окна: ни ожидания, ни команд плееру.
    assert_eq!(
        harness.media.next_episode_wait(),
        NextEpisodeWaitView::NeverWaited
    );
    assert_eq!(harness.player.take_calls(), Vec::new());
}
