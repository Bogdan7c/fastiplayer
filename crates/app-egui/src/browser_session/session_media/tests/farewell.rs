//! Прощание со страницей (04b, решения 40, 51, 127) на медиа-части с подделками: позиция
//! T — по правилу часов, `resume` — всегда пауза; без честной позиции прощания нет;
//! чужой и ушедший сеанс не трогаются; отказ прощания не теряется молча.

use browser_bridge_protocol::session::player::ResumeState;

use super::*;
use crate::browser_session::test_log_capture::with_captured_log;

fn return_paused_at(position: Duration) -> PlayerSessionMessage {
    PlayerSessionMessage::ReturnToPage {
        position_us: MediaTime::from_micros(u64::try_from(position.as_micros()).expect("µs"))
            .expect("media time"),
        resume: ResumeState::Paused,
    }
}

/// Плеер показывает поколение на позиции `position` и играет; проход медиа-части
/// запоминает это наблюдение (как часы странице).
fn show_playing_at(harness: &mut Harness, position: Duration) {
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, position, PlaybackState::Playing);
    // Срок следующего тика часов уже наступил: этот проход точно шлёт часы.
    harness
        .media
        .drive(Instant::now() + crate::browser_session::outbound::CLOCK_INTERVAL);
}

/// Последние часы, ушедшие странице.
fn last_clock_us(harness: &Harness) -> u64 {
    clocks(&harness.page.take_sent())
        .last()
        .map(|(position, _)| *position)
        .expect("the page got a clock")
}

#[test]
fn the_replaced_tab_gets_return_to_page_at_the_clock_position_on_pause() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_millis(2_500));
    let clock_us = last_clock_us(&harness);
    harness.player.take_calls();

    harness.media.bid_farewell_to_replaced(&current);

    assert_eq!(
        *harness.closer.farewells.borrow(),
        vec![FarewellCall::ToReplaced(
            current.clone(),
            return_paused_at(Duration::from_micros(clock_us))
        )]
    );
    // Прощание не закрывает сеанс (коридор закроет сервер) и не командует плееру.
    assert!(harness.closer.closed.borrow().is_empty());
    assert!(harness.player.take_calls().is_empty());
    // Старой странице прощание ушло через сервер, а не очередью часов.
    assert!(harness.page.take_sent().is_empty());
}

/// Позиция берётся до того, как открытие нового сеанса поставит видео на паузу и
/// забудет поколение: после открытия прощаться уже нечем.
#[test]
fn after_the_new_session_opened_there_is_no_position_left_to_say() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    harness.media.on_session_opened(&session(2));

    harness.media.bid_farewell_to_replaced(&current);

    assert!(harness.closer.farewells.borrow().is_empty());
}

#[test]
fn nothing_loaded_means_no_farewell() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    harness.media.drive(Instant::now());

    harness.media.bid_farewell_to_replaced(&current);

    assert!(harness.closer.farewells.borrow().is_empty());
    assert!(harness.closer.closed.borrow().is_empty());
}

/// Плеер ещё ставит поколение: показанная позиция — чужого `MediaSource`, часы говорят
/// «жду» в 0. Прощание с нулём отмотало бы страницу в начало — его нет.
#[test]
fn a_generation_still_installing_gives_no_farewell_position() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, notice) = stream_generation(&current, 1, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    // Наблюдение ещё о прежнем медиа (позиция 7 с), квитанция не показана.
    harness.player.observation.borrow_mut().position = Duration::from_secs(7);
    harness.media.drive(Instant::now());
    assert!(
        clocks(&harness.page.take_sent())
            .iter()
            .all(|(position, state)| *position == 0 && *state == ClockState::Waiting),
        "the clock waits at zero while the generation installs"
    );

    harness.media.bid_farewell_to_replaced(&current);

    assert!(harness.closer.farewells.borrow().is_empty());
}

#[test]
fn a_session_that_is_not_the_current_one_hears_nothing() {
    let (mut harness, _current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));

    harness.media.bid_farewell_to_replaced(&session(9));

    assert!(harness.closer.farewells.borrow().is_empty());
}

/// Вкладка ушла (доигрываем): прощаться не с кем, сервер всё равно ответил бы
/// «коридор закрыт».
#[test]
fn a_tab_that_is_already_gone_hears_nothing() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    harness.media.on_session_ended(&current, TAB_GONE_END);

    harness.media.bid_farewell_to_replaced(&current);

    assert!(harness.closer.farewells.borrow().is_empty());
}

#[test]
fn a_refused_farewell_is_reported_in_the_log() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    *harness.closer.refuse_farewells.borrow_mut() = true;

    let ((), log) = with_captured_log(|| harness.media.bid_farewell_to_replaced(&current));

    assert!(
        log.contains("Farewell to the replaced tab was refused"),
        "{log}"
    );
    assert!(harness.closer.closed.borrow().is_empty());
}

// ---- зритель закрыл окно ---------------------------------------------------------

#[test]
fn closing_the_window_says_farewell_and_closes_with_player_closed() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_millis(3_250));
    let clock_us = last_clock_us(&harness);
    harness.player.take_calls();

    assert_eq!(
        harness.media.close_for_viewer(),
        ViewerCloseStep::SessionClosed
    );

    assert_eq!(
        *harness.closer.farewells.borrow(),
        vec![FarewellCall::CloseWithFarewell(
            current,
            return_paused_at(Duration::from_micros(clock_us)),
            SessionCloseReason::PlayerClosed
        )]
    );
    // Закрытие с прощанием — одно действие сервера, отдельного `close_session` нет;
    // плееру ничего (в том числе никакого «играть»).
    assert!(harness.closer.closed.borrow().is_empty());
    assert!(harness.player.take_calls().is_empty());
}

#[test]
fn closing_the_window_before_the_video_is_shown_closes_without_a_farewell() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);

    assert_eq!(
        harness.media.close_for_viewer(),
        ViewerCloseStep::SessionClosed
    );

    assert!(harness.closer.farewells.borrow().is_empty());
    assert_eq!(
        *harness.closer.closed.borrow(),
        vec![(current, SessionCloseReason::PlayerClosed)]
    );
}

/// Прощание не по протоколу (дефект) — сеанс всё равно закрывается, причина в журнале.
#[test]
fn a_refused_farewell_on_window_close_still_closes_the_session() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    *harness.closer.refuse_farewells.borrow_mut() = true;

    let (step, log) = with_captured_log(|| harness.media.close_for_viewer());

    assert_eq!(step, ViewerCloseStep::SessionClosed);
    assert_eq!(
        *harness.closer.closed.borrow(),
        vec![(current, SessionCloseReason::PlayerClosed)]
    );
    assert!(log.contains("Farewell to the page was refused"), "{log}");
}

/// Вкладка уже ушла (доигрываем): закрытие окна = «доиграно», странице и серверу ничего.
#[test]
fn closing_the_window_while_draining_touches_neither_the_page_nor_the_server() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    harness.media.on_session_ended(&current, TAB_GONE_END);
    harness.page.take_sent();

    assert_eq!(
        harness.media.close_for_viewer(),
        ViewerCloseStep::DrainFinishedByViewer
    );

    assert!(harness.closer.farewells.borrow().is_empty());
    assert!(harness.closer.closed.borrow().is_empty());
    assert!(harness.page.take_sent().is_empty());
}

#[test]
fn closing_the_window_without_a_session_reports_it() {
    let mut harness = Harness::new();

    assert_eq!(harness.media.close_for_viewer(), ViewerCloseStep::NoSession);
    assert!(harness.closer.closed.borrow().is_empty());
}

// ---- плеер сам оборвал поток ошибкой (4003, решение 132) ---------------------------

/// Просьба закрыть сеанс ошибкой потока от приёма (переполнение и т. п.).
fn stream_error_notice(session: &SessionRef) -> IntakeNotice {
    IntakeNotice::CloseSession {
        session: session.clone(),
        reason: SessionCloseReason::ProtocolViolation,
    }
}

#[test]
fn a_stream_error_says_farewell_on_pause_before_the_closing_frame() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_millis(1_750));
    let clock_us = last_clock_us(&harness);

    harness
        .media
        .on_intake(vec![stream_error_notice(&current)], None);

    // Одно действие сервера «прощание + кадр 4003»: порядок на проводе — его забота.
    assert_eq!(
        *harness.closer.farewells.borrow(),
        vec![FarewellCall::CloseWithFarewell(
            current,
            return_paused_at(Duration::from_micros(clock_us)),
            SessionCloseReason::ProtocolViolation
        )]
    );
    assert!(harness.closer.closed.borrow().is_empty());
}

#[test]
fn a_stream_error_before_the_video_is_shown_closes_without_a_farewell() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);

    harness
        .media
        .on_intake(vec![stream_error_notice(&current)], None);

    assert!(harness.closer.farewells.borrow().is_empty());
    assert_eq!(
        *harness.closer.closed.borrow(),
        vec![(current, SessionCloseReason::ProtocolViolation)]
    );
}

#[test]
fn a_stream_error_of_another_session_gets_no_farewell() {
    let (mut harness, _current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    let other = session(9);

    harness
        .media
        .on_intake(vec![stream_error_notice(&other)], None);

    assert!(harness.closer.farewells.borrow().is_empty());
    assert_eq!(
        *harness.closer.closed.borrow(),
        vec![(other, SessionCloseReason::ProtocolViolation)]
    );
}

#[test]
fn a_stream_error_while_draining_gets_no_farewell() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    harness.media.on_session_ended(&current, TAB_GONE_END);

    harness
        .media
        .on_intake(vec![stream_error_notice(&current)], None);

    assert!(harness.closer.farewells.borrow().is_empty());
}

#[test]
fn a_refused_farewell_on_a_stream_error_still_closes_with_4003() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing_at(&mut harness, Duration::from_secs(1));
    *harness.closer.refuse_farewells.borrow_mut() = true;

    harness
        .media
        .on_intake(vec![stream_error_notice(&current)], None);

    assert_eq!(
        *harness.closer.closed.borrow(),
        vec![(current, SessionCloseReason::ProtocolViolation)]
    );
}

/// Окно говорит про ошибку и когда связь оборвал плеер, и когда сервер; доигрывания
/// при этом нет (сообщение не пересекается с правилом доигрывания).
#[test]
fn the_window_sees_a_stream_error_end_but_other_silent_ends_stay_silent() {
    use crate::browser_session::session_end::SessionEndView;
    let (mut harness, current, _writer) = loaded_harness();
    harness.media.on_session_ended(
        &current,
        SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation),
    );
    assert_eq!(
        harness.media.session_end_view(),
        SessionEndView::StreamError
    );

    let (mut harness, current, _writer) = loaded_harness();
    harness.media.on_session_ended(
        &current,
        SessionEndReason::ClosedByPlayer(SessionCloseReason::PlayerClosed),
    );
    assert_eq!(harness.media.session_end_view(), SessionEndView::Silent);
}
