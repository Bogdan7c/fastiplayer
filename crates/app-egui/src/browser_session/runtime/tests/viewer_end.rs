//! Концы сеанса для окна (04b) глазами главного цикла: прощание заменённой вкладке
//! раньше открытия новой, «окно ждёт зрителя» без срока, закрытие окна во время
//! доигрывания = «доиграно».

use super::*;

/// Прощание заменённой вкладке приходит медиа-части в том же шаге и РАНЬШЕ открытия
/// нового сеанса: открытие ставит видео на паузу и забывает позицию старой вкладки.
#[test]
fn the_replaced_tab_hears_the_farewell_before_the_new_session_opens() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let first = harness.open(1, "tab-1-video-1");
    let replacement = harness.open(2, "tab-2-video-1");
    assert_eq!(replacement.outcome, GrantOutcome::ReplacedPrevious);

    let first_ref = harness.media.opened[0].clone();
    let second_ref = harness.media.opened[1].clone();
    assert_eq!(first_ref.key, first.session_key);
    assert_eq!(
        harness.media.opening_calls,
        vec![
            OpeningCall::Opened(first_ref.clone()),
            OpeningCall::FarewellToReplaced(first_ref),
            OpeningCall::Opened(second_ref),
        ]
    );
}

/// Повтор того же сеанса — не замена: прощаться не с кем.
#[test]
fn reusing_the_same_session_says_no_farewell() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    harness.open(1, "tab-1-video-1");
    harness.open(2, "tab-1-video-1");

    assert!(
        harness
            .media
            .opening_calls
            .iter()
            .all(|call| matches!(call, OpeningCall::Opened(_))),
        "{:?}",
        harness.media.opening_calls
    );
}

/// Окно на паузе ждёт человека: процесс жив, а срока пробуждения от доигрывания нет —
/// продолжит действие зрителя (событие окна), а не таймер.
#[test]
fn a_window_waiting_for_the_viewer_keeps_the_process_alive_without_a_deadline() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);
    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");
    harness.step_until(|harness| harness.media.drain_questions >= 1);
    harness.media.drain_answer = Some(DrainProgress::AwaitingViewer);

    for _ in 0..3 {
        assert_eq!(
            harness.runtime.step(Instant::now(), &mut harness.media),
            LifecycleVerdict::KeepRunning { wake_at: None }
        );
    }
}

/// Зритель закрыл окно во время доигрывания: «доиграно» по его воле — выход с причиной
/// конца сеанса, медиа-часть больше не спрашивают.
#[test]
fn closing_the_window_during_the_drain_exits_with_the_end_of_the_session() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);
    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");
    harness.step_until(|harness| harness.media.drain_questions >= 1);
    harness.media.drain_answer = Some(DrainProgress::AwaitingViewer);

    let verdict = harness.runtime.finish_drain_for_viewer(Instant::now());

    let tab_gone = ExitCause::SessionEnded(SessionEndReason::ClosedByPlayer(
        SessionCloseReason::TabGone,
    ));
    assert_eq!(verdict, LifecycleVerdict::Exit(tab_gone));
    let questions = harness.media.drain_questions;
    assert_eq!(
        harness.runtime.step(Instant::now(), &mut harness.media),
        LifecycleVerdict::Exit(tab_gone)
    );
    assert_eq!(harness.media.drain_questions, questions);
}

/// Вне доигрывания «зритель закрыл окно» решение машины не меняет (активный сеанс живёт,
/// пока его не закроет сервер): метод не трогает чужое состояние.
#[test]
fn finishing_a_drain_that_is_not_running_changes_nothing() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    harness.open(1, "tab-1-video-1");

    assert_eq!(
        harness.runtime.finish_drain_for_viewer(Instant::now()),
        LifecycleVerdict::KeepRunning { wake_at: None }
    );
}
