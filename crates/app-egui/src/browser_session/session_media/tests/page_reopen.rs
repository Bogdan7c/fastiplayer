//! Возврат ушедшей страницы (протокол 1.4, «сирота», сессия 11b1): страница ушла, связь
//! держится; play зрителя просит её назад один раз; пока просьба в пути, конец скачанного
//! окно не закрывает; вернулась — тот же сеанс снова говорит со страницей; отказ, потеря
//! связи и срок — запасной путь 04b.

use browser_bridge_protocol::ProtocolVersion;
use browser_bridge_protocol::session::{PageReopenFailure, TabGoneReason};

use super::*;
use crate::browser_session::intake_notice::PageLinkChange;
use crate::browser_session::session_end::{DrainCause, DrainFinish, SessionEndView};
use crate::browser_session::session_media::page_reopen::PageReopenStage;
use crate::viewer_transport::ViewerTransportIntent;

/// Политика окна с коротким застоем (как у тестов досмотра в окне).
const WINDOW_POLICY: DrainPolicy = DrainPolicy {
    stall_timeout: Duration::from_millis(400),
    poll_interval: Duration::from_millis(250),
    paused: PausedDrain::WaitForViewer,
};

/// Позиция показанного поколения в начале каждого теста.
const SHOWN_AT: Duration = Duration::from_millis(1_500);

/// Окно с показанным поколением сеанса на протоколе 1.4, зритель поставил паузу, страница
/// ушла, оставив связь.
fn orphan_harness(start: Instant) -> (Harness, SessionRef) {
    let (mut harness, current, _writer) = loaded_harness();
    harness.media.drain_policy = WINDOW_POLICY;
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_4);
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, SHOWN_AT, PlaybackState::Playing);
    harness.media.drive(start);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, start);
    page_link(
        &mut harness,
        &current,
        PageLinkChange::Left(TabGoneReason::Closed),
    );
    harness.player.take_calls();
    harness.page.take_sent();
    (harness, current)
}

fn page_link(harness: &mut Harness, session: &SessionRef, change: PageLinkChange) {
    harness.media.on_intake(
        vec![IntakeNotice::PageLink {
            session: session.clone(),
            change,
        }],
        None,
    );
}

fn reopen_requests(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::ReopenPage { position_us } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect()
}

fn show_ended(harness: &Harness) {
    let mut observation = harness.player.observation.borrow_mut();
    observation.ended = true;
    observation.state = PlaybackState::Ended;
}

/// Зритель нажал play, цикл сделал проход: просьба ушла.
fn press_play(harness: &mut Harness, at: Instant) -> Vec<PlayerSessionMessage> {
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Play, at);
    harness.media.drive(at);
    harness.page.take_sent()
}

#[test]
fn after_page_left_the_window_waits_paused_and_the_orphan_still_hears_the_clock() {
    let start = Instant::now();
    let (mut harness, _current) = orphan_harness(start);
    assert_eq!(
        harness.media.playback_drain(start),
        DrainProgress::AwaitingViewer
    );
    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::WaitingForViewer(DrainCause::TabClosed)
    );
    // Пульс сироты: часы уходят и без страницы (после `tab_gone` их нет — тест drain.rs).
    harness.media.drive(start + Duration::from_millis(300));
    let sent = harness.page.take_sent();
    assert!(
        !clocks(&sent).is_empty(),
        "the orphan link must keep receiving the clock: {sent:?}"
    );
    assert!(reopen_requests(&sent).is_empty(), "nothing asked yet");
    assert_eq!(harness.media.page_reopen_stage(), PageReopenStage::Ready);
}

#[test]
fn viewer_play_asks_once_for_the_page_at_the_player_position() {
    let start = Instant::now();
    let (mut harness, _current) = orphan_harness(start);
    let sent = press_play(&mut harness, start);
    assert_eq!(
        reopen_requests(&sent),
        vec![u64::try_from(SHOWN_AT.as_micros()).unwrap()]
    );
    // Плеер играет скачанное сам, не дожидаясь страницы.
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Reopening
    );
    // Пауза и снова play — второй просьбы нет.
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, start);
    let again = press_play(&mut harness, start + Duration::from_millis(50));
    assert!(reopen_requests(&again).is_empty(), "one request per orphan");
    assert!(matches!(
        harness.media.page_reopen_stage(),
        PageReopenStage::Requested { .. }
    ));
}

#[test]
fn while_the_request_travels_the_end_of_the_shelf_does_not_close_the_window() {
    let start = Instant::now();
    let (mut harness, _current) = orphan_harness(start);
    press_play(&mut harness, start);
    show_ended(&harness);
    harness.media.drive(start);
    assert!(matches!(
        harness.media.playback_drain(start),
        DrainProgress::Draining { .. }
    ));
    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::ReopeningPage
    );
}

#[test]
fn a_refusal_lets_the_drain_finish_the_04b_way() {
    let start = Instant::now();
    let (mut harness, current) = orphan_harness(start);
    press_play(&mut harness, start);
    harness.media.take_page_reopen_notice();
    show_ended(&harness);
    page_link(
        &mut harness,
        &current,
        PageLinkChange::ReopenFailed(PageReopenFailure::NotTheSameVideo),
    );
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Refused(PageReopenRefusal::Extension(
            PageReopenFailure::NotTheSameVideo
        ))
    );
    harness.media.drive(start);
    assert_eq!(
        harness.media.playback_drain(start),
        DrainProgress::Drained(DrainEnd::PlayedToEnd)
    );
    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::Finished(DrainFinish {
            cause: DrainCause::TabClosed,
            end: DrainEnd::PlayedToEnd,
        })
    );
}

#[test]
fn the_returned_page_gets_its_messages_again_in_the_same_session() {
    let start = Instant::now();
    let (mut harness, current) = orphan_harness(start);
    press_play(&mut harness, start);
    page_link(&mut harness, &current, PageLinkChange::Returned);
    assert_eq!(harness.media.session_end_view(), SessionEndView::Silent);
    assert_eq!(
        harness.media.page_reopen_stage(),
        PageReopenStage::Unavailable
    );
    // Страница снова слушает: пауза зрителя уходит ей командой, как до ухода.
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, start);
    harness.media.drive(start + Duration::from_millis(300));
    let sent = harness.page.take_sent();
    assert!(
        sent.contains(&PlayerSessionMessage::Pause {}),
        "the page is back and must get the viewer's pause: {sent:?}"
    );
}

#[test]
fn losing_the_link_while_asked_is_a_refusal_and_the_drain_keeps_its_cause() {
    let start = Instant::now();
    let (mut harness, current) = orphan_harness(start);
    press_play(&mut harness, start);
    harness.media.take_page_reopen_notice();
    harness.media.on_session_ended(
        &current,
        SessionEndReason::ClosedByPage { code: Some(4002) },
    );
    // Событие сервера обгоняет данные: решает только «вход запечатан».
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Nothing
    );
    harness.media.on_intake(
        vec![IntakeNotice::InstanceSealed {
            session: current.clone(),
        }],
        None,
    );
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Refused(PageReopenRefusal::LinkLost)
    );
    show_ended(&harness);
    harness.media.drive(start);
    harness.media.playback_drain(start);
    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::Finished(DrainFinish {
            cause: DrainCause::TabClosed,
            end: DrainEnd::PlayedToEnd,
        })
    );
    // Связи нет — часов тоже.
    harness.page.take_sent();
    harness.media.drive(start + Duration::from_millis(300));
    assert!(clocks(&harness.page.take_sent()).is_empty());
}

#[test]
fn silence_of_the_extension_ends_the_wait_at_the_deadline() {
    let start = Instant::now();
    let (mut harness, _current) = orphan_harness(start);
    press_play(&mut harness, start);
    harness.media.take_page_reopen_notice();
    harness.media.drive(start + Duration::from_secs(44));
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Nothing
    );
    harness.media.drive(start + Duration::from_secs(45));
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Refused(PageReopenRefusal::NoAnswer)
    );
}

#[test]
fn after_tab_gone_without_a_link_play_asks_nothing() {
    let start = Instant::now();
    let (mut harness, current, _writer) = loaded_harness();
    harness.media.drain_policy = WINDOW_POLICY;
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_4);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, start);
    harness.media.on_session_ended(&current, TAB_GONE_END);
    harness.page.take_sent();
    let sent = press_play(&mut harness, start);
    assert!(reopen_requests(&sent).is_empty());
    assert_eq!(
        harness.media.page_reopen_stage(),
        PageReopenStage::Unavailable
    );
}

/// Гонка из сквозного теста: экстеншен сказал «не то видео» и закрыл связь, а событие
/// сервера о закрытии пришло раньше его слова. Причина отказа не теряется.
#[test]
fn the_extension_reason_wins_over_the_close_that_overtook_it() {
    let start = Instant::now();
    let (mut harness, current) = orphan_harness(start);
    press_play(&mut harness, start);
    harness.media.take_page_reopen_notice();
    harness.media.on_session_ended(
        &current,
        SessionEndReason::ClosedByPage { code: Some(4002) },
    );
    harness.media.on_intake(
        vec![
            IntakeNotice::PageLink {
                session: current.clone(),
                change: PageLinkChange::ReopenFailed(PageReopenFailure::NotTheSameVideo),
            },
            IntakeNotice::InstanceSealed {
                session: current.clone(),
            },
        ],
        None,
    );
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Refused(PageReopenRefusal::Extension(
            PageReopenFailure::NotTheSameVideo
        ))
    );
}

/// Просьбу нельзя отправить (общая версия ниже 1.4 — сторож сервера так не пустит, это
/// страховка): play зрителя не пропадает молча, окно говорит, что страницу не вернуть.
#[test]
fn a_request_that_cannot_be_sent_is_told_to_the_viewer() {
    let start = Instant::now();
    let (mut harness, current, _writer) = loaded_harness();
    harness.media.drain_policy = WINDOW_POLICY;
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_3);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, start);
    page_link(
        &mut harness,
        &current,
        PageLinkChange::Left(TabGoneReason::Closed),
    );
    harness.page.take_sent();
    let sent = press_play(&mut harness, start);
    assert!(reopen_requests(&sent).is_empty());
    assert_eq!(
        harness.media.take_page_reopen_notice(),
        PageReopenNotice::Refused(PageReopenRefusal::NotSent)
    );
}
