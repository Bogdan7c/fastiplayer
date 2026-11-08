//! Строки «заглушка и возврат»: заглушка над видео и едет за ним, «Return» настоящей мышью,
//! закрытие окна, «Return» вместе с закрытием. Решения 236 и 51.

use std::time::Duration;

use super::facts::{RETURN_TIMELINE_LINE, ReturnFacts, VIDEO_CLICKS_LINE, parse_video_clicks};
use super::judge::{
    ReturnOutcome, box_contains, describe, judge_panel_over_video, judge_race,
    judge_returned_paused_between, judge_returned_playing, judge_scroll_moved_video,
    judge_silent_viewer_end,
};
use super::steps::{
    Begun, CLOSE_WINDOW_GONE_TIMEOUT, RETURN_WINDOW_GONE_TIMEOUT, Session, begin, verdict,
};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::entries::return_hand::ClientBox;
use crate::scenarios::handoff_run::HANDOFF_MODE_QUERY;
use crate::scenarios::outcome::Verdict;
use crate::site::RunId;

/// Сколько даём заглушке догнать видео после прокрутки: событие `scroll` и перемер каждые 250 мс.
const FOLLOW_SETTLE: Duration = Duration::from_millis(1000);
/// Сколько ждём после клика по панели, прежде чем спросить страницу о кликах по видео.
const CLICK_SETTLE: Duration = Duration::from_millis(800);
/// Куда кликать по панели мимо кнопки: левый верхний угол (доли ширины и высоты панели).
const PANEL_CORNER_SHARE: f64 = 0.12;
/// Сколько микросекунд в секунде (позиция MPRIS — микросекунды, страница — секунды).
const MICROS_PER_SECOND: f64 = 1_000_000.0;

/// Куда попал клик «Return» в гонке (для сводки строки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LateClick {
    /// Клик съела заглушка: до `<video>` страницы он не дошёл.
    StayedOnThePlaceholder,
    /// Клик упал на сам `<video>` страницы: щит заглушки его пропустил — провал.
    ReachedThePageVideo,
}

/// Кто в гонке «Return + закрытие окна» стартует первым.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RaceOrder {
    /// Настоящий клик «Return», сразу за ним `Quit` окна.
    ReturnThenClose,
    /// `Quit` окна, сразу за ним клик «Return».
    CloseThenReturn,
}

/// Строка 1: заглушка ровно над видео, глотает клики, едет за прокруткой, уходит с концом сеанса.
pub(super) fn run_geometry(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(stand, environment, browser, page_id, HANDOFF_MODE_QUERY)? {
        Begun::Running(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    geometry_steps(&mut session);
    let (end, failures, notes) = session.close(&[])?;
    Ok(verdict(&end, failures, &notes))
}

fn geometry_steps(session: &mut Session<'_, '_>) {
    let run = session.current_run();
    let prefix = session.current_prefix();
    let aim = session.aim_at_return(&run, &prefix);
    let Some(aim) = session.record(aim) else {
        return;
    };
    let mut failures =
        judge_panel_over_video("after the hand-off", aim.placeholder.panel, aim.video);
    // Клик по панели мимо кнопки: до `<video>` страницы он дойти не должен, сеанс идёт дальше.
    if let Some(panel) = aim.placeholder.panel {
        failures.extend(swallowed_click_failures(
            session,
            &run,
            panel,
            aim.placeholder.return_button,
        ));
    }
    // Прокрутка: видео уезжает вверх, заглушка обязана поехать за ним.
    if let Err(problem) = session.tell_and_wait("hook:scroll-down") {
        failures.push(problem);
    }
    std::thread::sleep(FOLLOW_SETTLE);
    match (
        session.hand.placeholder_on(&prefix),
        session.hand.locate(&run),
    ) {
        (Ok(after), Ok(place)) => {
            failures.extend(judge_scroll_moved_video(aim.video, place.video_rect()));
            failures.extend(judge_panel_over_video(
                "after the page scrolled",
                after.panel,
                place.video_rect(),
            ));
            session.notes.push(format!(
                "panel {:?} over video {:?}, after scroll {:?} over {:?}",
                aim.placeholder.panel,
                aim.video,
                after.panel,
                place.video_rect()
            ));
        }
        (probe, place) => failures.push(format!(
            "after the scroll: probe {probe:?}, calibration {:?}",
            place.err()
        )),
    }
    session.failures.extend(failures);
    // Конец сеанса (закрытие окна): страница на паузе на своей секунде, заглушки нет.
    let closed = close_and_check_paused(session, &run);
    session.record(closed);
    let gone = session.wait_placeholder_gone(&prefix);
    session.record(gone);
}

/// Клик по панели мимо кнопки «Return»: страница не видит его на видео, окно плеера живо.
fn swallowed_click_failures(
    session: &mut Session<'_, '_>,
    run: &RunId,
    panel: ClientBox,
    button: Option<ClientBox>,
) -> Vec<String> {
    let corner_x = panel.left + panel.width * PANEL_CORNER_SHARE;
    let corner_y = panel.top + panel.height * PANEL_CORNER_SHARE;
    if button.is_some_and(|button| box_contains(button, corner_x, corner_y)) {
        return vec![format!(
            "the panel corner ({corner_x}, {corner_y}) lies on the Return button {button:?}"
        )];
    }
    let result = (|| -> Result<i64, String> {
        session.tell_and_wait("count-video-clicks")?;
        let place = session
            .hand
            .locate(run)
            .map_err(|error| error.to_string())?;
        let point = place.screen_point_in(panel, PANEL_CORNER_SHARE, PANEL_CORNER_SHARE);
        session
            .hand
            .click(point)
            .map_err(|error| error.to_string())?;
        std::thread::sleep(CLICK_SETTLE);
        clicks_on_the_video(session, run)
    })();
    let mut failures = Vec::new();
    match result {
        Ok(0) => session
            .notes
            .push("a click on the panel did not reach the page video".to_owned()),
        Ok(count) => failures.push(format!(
            "a click on the placeholder reached the page video ({count} events)"
        )),
        Err(problem) => failures.push(format!("click on the panel: {problem}")),
    }
    if let Err(error) = session
        .control
        .wait_until_controllable(Duration::from_secs(2))
    {
        failures.push(format!(
            "the player window is gone after a click on the panel: {error}"
        ));
    }
    failures
}

/// Сколько кликов и нажатий дошло до `<video>` страницы с команды `count-video-clicks`.
fn clicks_on_the_video(session: &mut Session<'_, '_>, run: &RunId) -> Result<i64, String> {
    session.tell_and_wait("report-video-clicks")?;
    let line =
        session
            .hand
            .run_ref()
            .wait_page_log_line_of(run, VIDEO_CLICKS_LINE, CLICK_SETTLE)?;
    parse_video_clicks(&line)
}

/// Отметка → `Quit` окна → пауза на своей секунде (решение 51).
fn close_and_check_paused(session: &mut Session<'_, '_>, run: &RunId) -> Result<(), String> {
    session.mark_return(run)?;
    session
        .control
        .quit_window()
        .map_err(|error| format!("Quit: {error}"))?;
    let gone = session.wait_window_gone(CLOSE_WINDOW_GONE_TIMEOUT);
    let facts = session.observe_return(run)?;
    session
        .notes
        .push(format!("window closed: {}", describe(&facts)));
    gone?;
    let failures =
        judge_returned_paused_between(&facts, facts.mark_s, facts.mark_s + facts.elapsed_s);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Строки 2 и 7: «Return» настоящей мышью → страница играет сама со своей секунды N ± 1 с, окно
/// закрылось сразу и молча (4010), заглушки нет, нового сеанса нет.
pub(super) fn run_return_click(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(stand, environment, browser, page_id, HANDOFF_MODE_QUERY)? {
        Begun::Running(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let run = session.current_run();
    let prefix = session.current_prefix();
    if let Some(facts) = click_return_and_observe(&mut session, &run, &prefix) {
        session.failures.extend(judge_returned_playing(&facts));
    }
    let (end, mut failures, notes) = session.close(&[])?;
    failures.extend(judge_silent_viewer_end(&end.player));
    if end.host.session_grants != 1 || end.player.sessions_opened != 1 {
        failures.push(format!(
            "a new session started after Return: {} grants, {} sessions opened",
            end.host.session_grants, end.player.sessions_opened
        ));
    }
    Ok(verdict(&end, failures, &notes))
}

/// Отметка, настоящий клик «Return», окно ушло, факты страницы, заглушки нет. `None` — шаг не прошёл
/// (провал уже записан).
pub(super) fn click_return_and_observe(
    session: &mut Session<'_, '_>,
    run: &RunId,
    prefix: &str,
) -> Option<ReturnFacts> {
    let aim = session.aim_at_return(run, prefix);
    let aim = session.record(aim)?;
    let marked = session.mark_return(run);
    session.record(marked)?;
    let clicked = session
        .hand
        .click(aim.button_point)
        .map_err(|error| error.to_string());
    session.record(clicked)?;
    let gone = session.wait_window_gone(RETURN_WINDOW_GONE_TIMEOUT);
    let facts = session.observe_return(run);
    let facts = session.record(facts);
    if let Ok(took) = &gone {
        session.notes.push(format!(
            "window gone {:.2} s after the click",
            took.as_secs_f64()
        ));
    }
    session.record(gone);
    let placeholder_gone = session.wait_placeholder_gone(prefix);
    session.record(placeholder_gone);
    if let Some(facts) = &facts {
        session.notes.push(format!("Return: {}", describe(facts)));
    }
    facts
}

/// Строка 3: закрытие окна (`Quit`) → страница на паузе на секунде закрытия, потом играет сама.
pub(super) fn run_close_window(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(stand, environment, browser, page_id, HANDOFF_MODE_QUERY)? {
        Begun::Running(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let prefix = session.current_prefix();
    let closed = close_and_expect_returned(&mut session);
    session.record(closed);
    let gone = session.wait_placeholder_gone(&prefix);
    session.record(gone);
    let (end, mut failures, notes) = session.close(&[])?;
    if end.player.ended_by_viewer_return() {
        failures.push(
            "closing the window ended the session as if Return was clicked (4010)".to_owned(),
        );
    }
    if !end.player.farewell {
        failures
            .push("the player closed without a farewell (return_to_page) to the page".to_owned());
    }
    Ok(verdict(&end, failures, &notes))
}

/// Позиция плеера → `Quit` → страница подтверждает паузу на ней и играет сама (`expect-returned`).
pub(super) fn close_and_expect_returned(session: &mut Session<'_, '_>) -> Result<(), String> {
    session.placeholder_ready()?;
    let position = session
        .control
        .position_micros()
        .map_err(|error| format!("Position: {error}"))?;
    session
        .control
        .quit_window()
        .map_err(|error| format!("Quit: {error}"))?;
    let position_s = position as f64 / MICROS_PER_SECOND;
    let command = format!("expect-returned:{position_s:.2}");
    let page_agrees = session.tell_and_wait(&command);
    let gone = session.wait_window_gone(CLOSE_WINDOW_GONE_TIMEOUT);
    session
        .notes
        .push(format!("window closed at {position_s:.2} s"));
    page_agrees?;
    gone.map(|_| ())
}

impl Session<'_, '_> {
    /// Заглушка текущей страницы на месте (передача видна странице до закрытия окна).
    fn placeholder_ready(&mut self) -> Result<(), String> {
        let prefix = self.current_prefix();
        self.wait_placeholder_shown(&prefix).map(|_| ())
    }
}

/// Строка 5: «Return» и закрытие окна почти разом → ровно один возврат, без дёрганья.
pub(super) fn run_race(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
    order: RaceOrder,
) -> Result<Verdict, ScenarioError> {
    // Страница гонки — с родными кнопками `<video>`: опоздавший клик «Return», дойди он до видео,
    // запустил бы игру сам (Chromium и Firefox оба так делают). Щит заглушки после конца передачи
    // обязан его съесть — это и проверяется.
    let mut session = match begin(stand, environment, browser, page_id, HANDOFF_MODE_QUERY)? {
        Begun::Running(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let run = session.current_run();
    let prefix = session.current_prefix();
    let mut outcome = None;
    if let Some((facts, late_click)) = race_steps(&mut session, &run, &prefix, order) {
        let (won, failures) = judge_race(&facts);
        session.notes.push(format!(
            "{order:?}: {}; click {late_click:?}",
            describe(&facts)
        ));
        session.failures.extend(failures);
        outcome = Some(won);
    }
    let (end, failures, mut notes) = session.close(&[])?;
    let winner = match outcome {
        Some(ReturnOutcome::Playing) => "Return won (page plays)",
        Some(ReturnOutcome::Paused) => "the window close won (page paused)",
        _ => "no winner",
    };
    notes.push(format!(
        "{winner}; player: 4010 end={}, farewell={}",
        end.player.ended_by_viewer_return(),
        end.player.farewell
    ));
    Ok(verdict(&end, failures, &notes))
}

/// Отметка, оба действия почти разом (`Quit` в своём потоке), окно ушло, факты страницы.
fn race_steps(
    session: &mut Session<'_, '_>,
    run: &RunId,
    prefix: &str,
    order: RaceOrder,
) -> Option<(ReturnFacts, LateClick)> {
    let aim = session.aim_at_return(run, prefix);
    let aim = session.record(aim)?;
    // Опоздавший клик (заглушку уже сняло закрытие) упадёт на само видео: счёт это покажет.
    let counting = session.tell_and_wait("count-video-clicks");
    session.record(counting)?;
    let marked = session.mark_return(run);
    session.record(marked)?;
    let hand = &session.hand;
    let control = &session.control;
    let (click, quit) = std::thread::scope(|scope| match order {
        RaceOrder::ReturnThenClose => {
            let click = hand.click(aim.button_point);
            let quit = scope.spawn(|| control.quit_window());
            (click, quit.join())
        }
        RaceOrder::CloseThenReturn => {
            let quit = scope.spawn(|| control.quit_window());
            let click = hand.click(aim.button_point);
            (click, quit.join())
        }
    });
    session.record(click.map_err(|error| format!("click on Return: {error}")))?;
    // `Quit` по уже ушедшему окну честно отказывает (имени на шине нет): это не провал гонки.
    match quit {
        Ok(Ok(())) => session.notes.push("Quit delivered".to_owned()),
        Ok(Err(error)) => session.notes.push(format!("Quit not delivered: {error}")),
        Err(_) => session.failures.push("the Quit thread panicked".to_owned()),
    }
    let gone = session.wait_window_gone(CLOSE_WINDOW_GONE_TIMEOUT);
    session.record(gone);
    let facts = session.observe_return(run);
    let facts = session.record(facts)?;
    let placeholder_gone = session.wait_placeholder_gone(prefix);
    session.record(placeholder_gone);
    // Лента событий страницы — в сводку: по ней видно, кто и в каком порядке переключил игру.
    let timeline = session
        .hand
        .run_ref()
        .log_lines_of(run, RETURN_TIMELINE_LINE);
    if let Some(line) = timeline.last() {
        session.notes.push(line.trim().to_owned());
    }
    let clicks = clicks_on_the_video(session, run);
    let late_click = match session.record(clicks)? {
        0 => LateClick::StayedOnThePlaceholder,
        _ => LateClick::ReachedThePageVideo,
    };
    // Щит заглушки гасит клики ещё `PLACEHOLDER_LEAVE_CLICK_SHIELD_MS` после конца передачи. Firefox
    // клики по родным кнопкам странице не показывает — там такой промах ловит суд паузы `judge_race`.
    if late_click == LateClick::ReachedThePageVideo {
        session.failures.push(
            "the late Return click reached the page video (placeholder shield missed it)"
                .to_owned(),
        );
    }
    Some((facts, late_click))
}
