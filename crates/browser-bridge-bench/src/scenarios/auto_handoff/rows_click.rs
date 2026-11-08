//! Строки «один человек, одна вкладка»: настоящий клик (или наведение) на странице стенда, и видео
//! либо остаётся у браузера (`StaysInBrowser`), либо уходит в плеер (`HandedOff`).
//!
//! Строка «нет передачи» судится ПОЛОЖИТЕЛЬНЫМ фактом: за окно ожидания страница правда играла силами
//! браузера (настоящие кадры и часы), настоящий клик правда дошёл до неё, посредник не выдал ни одного
//! пропуска, заглушки нет. Строка «передача» — кадрами в рендере плеера (`presented_frames`),
//! заглушкой с «Return» и тем, что браузер под тенью почти не декодирует (как судит `capture`).

use std::time::Duration;

use super::facts::{LIKE_CLICKED_LINE, PREVIEW_HOVER_LINE};
use super::judge::{
    browser_plays_failures, click_reached_page_failures, handed_off_failures,
    hover_reached_page_failures, infinite_duration_failures, placeholder_failures,
    report_plays_in_browser_failures, session_count_failures,
};
use super::settings::SettingsPreset;
use super::steps::{AutoSession, Begun, begin, verdict};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::scenarios::capture::browser_does_not_decode;
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::outcome::Verdict;
use crate::site::RunId;

/// Сколько страница играет после клика, когда передачи быть НЕ должно. Тут обязан уложиться весь путь
/// передачи, если бы экстеншен ошибся: клик → решение фона → пропуск посредника → запуск плеера (в
/// других строках это занимает единицы секунд, `await_handoff` ждёт до 45 с на пропуск). 12 с — с
/// запасом на медленный запуск; страница всё это время меряет настоящие кадры, и позднюю передачу
/// выдаст как отметка посредника, так и упавшее число кадров.
pub(super) const NO_HANDOFF_WINDOW: Duration = Duration::from_secs(12);
/// Окно наблюдения под тенью после того, как плеер показал кадры (не короче 6 с — порога судьи).
const SHADOW_WINDOW: Duration = Duration::from_secs(8);

/// Что делает человек на странице.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gesture {
    /// Настоящий клик по кнопке Play страницы.
    ClickPlay,
    /// Настоящее движение мыши на превью (без клика).
    HoverPreview,
    /// Настоящий клик по «Like» ПОД видео; ролик ленты страница запускает сама чуть позже (решение 262).
    ClickBesideVideo,
}

/// Чем должна кончиться строка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Expected {
    /// Видео остаётся у браузера и играет им.
    StaysInBrowser,
    /// Видео уходит в плеер сразу; `endless` — у страницы бесконечная длина (эфир, поток без длины).
    HandedOff { endless: bool },
}

/// Описание строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClickRow {
    pub(super) page_id: &'static str,
    pub(super) preset: SettingsPreset,
    pub(super) gesture: Gesture,
    pub(super) expected: Expected,
}

/// Одна строка: открыть страницу с настройкой, жест человека, приговор.
pub(super) fn run_click_row(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: ClickRow,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(stand, environment, browser, row.page_id, row.preset)? {
        Begun::Ready(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    match row.expected {
        Expected::StaysInBrowser => stays_in_browser_steps(&mut session, row),
        Expected::HandedOff { endless } => handed_off_steps(&mut session, row, endless),
    }
    let (end, mut failures, notes) = session.close(&[])?;
    match row.expected {
        Expected::StaysInBrowser => failures.extend(session_count_failures(&end.host, 0)),
        Expected::HandedOff { .. } => {
            failures.extend(session_count_failures(&end.host, 1));
            if let Ok(report) = &end.current_report {
                failures.extend(browser_does_not_decode(report));
            }
        }
    }
    Ok(verdict(&end, failures, &notes))
}

/// Жест человека над целью страницы: клик по Play или наведение на превью.
fn make_gesture(
    session: &mut AutoSession<'_, '_>,
    run: &crate::site::RunId,
    gesture: Gesture,
) -> Result<(), String> {
    match gesture {
        Gesture::ClickPlay => session.click_target(run, "play"),
        Gesture::HoverPreview => session.hover_target(run, "preview"),
        Gesture::ClickBesideVideo => session.click_target(run, "like"),
    }
}

/// Нет передачи: окно ожидания, потом факты страницы и заглушки.
fn stays_in_browser_steps(session: &mut AutoSession<'_, '_>, row: ClickRow) {
    let run = session.current_run();
    let prefix = session.current_prefix();
    let marked = session.mark_window(&run);
    if session.record(marked).is_none() {
        return;
    }
    let gesture = make_gesture(session, &run, row.gesture);
    if session.record(gesture).is_none() {
        return;
    }
    std::thread::sleep(NO_HANDOFF_WINDOW);
    let reported = session.report_window(&run);
    if let Some(facts) = session.record(reported) {
        session.failures.extend(browser_plays_failures(&facts));
        session.notes.push(format!(
            "the page played in the browser: {} real frames in {:.1} s, clock +{:.1} s",
            facts.real_frames, facts.window_s, facts.clock_advance_s
        ));
    }
    match row.gesture {
        Gesture::ClickPlay => {
            let lines = session.play_click_lines(&run);
            session
                .failures
                .extend(click_reached_page_failures("the Play click", &lines));
        }
        Gesture::HoverPreview => {
            let lines = session.page_lines(&run, PREVIEW_HOVER_LINE);
            session.failures.extend(hover_reached_page_failures(&lines));
        }
        Gesture::ClickBesideVideo => {
            let lines = session.page_lines(&run, LIKE_CLICKED_LINE);
            session
                .failures
                .extend(click_reached_page_failures("the Like click", &lines));
        }
    }
    let probe = session.placeholder(&prefix);
    if let Some(view) = session.record(probe) {
        session
            .failures
            .extend(placeholder_failures("after the window", &view, false));
    }
}

/// Передача: кадры в плеере, заглушка, браузер под тенью не декодирует.
fn handed_off_steps(session: &mut AutoSession<'_, '_>, row: ClickRow, endless: bool) {
    let run = session.current_run();
    let prefix = session.current_prefix();
    let gesture = make_gesture(session, &run, row.gesture);
    if session.record(gesture).is_none() {
        return;
    }
    handed_off_after_gesture_steps(session, &run, &prefix, endless);
}

/// Хвост строки «передача» после жеста человека (клик по Play или выбор на предложении «Ask»): кадры в
/// плеере, заглушка, браузер под тенью не декодирует.
pub(super) fn handed_off_after_gesture_steps(
    session: &mut AutoSession<'_, '_>,
    run: &RunId,
    prefix: &str,
    endless: bool,
) {
    let started = session.await_handoff(1);
    if session.record(started).is_none() {
        return;
    }
    let shown = session.wait_placeholder_shown(prefix);
    if let Some(view) = session.record(shown) {
        session
            .failures
            .extend(placeholder_failures("after the click", &view, true));
    }
    let lines = session.play_click_lines(run);
    session
        .failures
        .extend(click_reached_page_failures("the Play click", &lines));
    let marked = session.mark_window(run);
    if session.record(marked).is_none() {
        return;
    }
    std::thread::sleep(SHADOW_WINDOW);
    let reported = session.report_window(run);
    if let Some(facts) = session.record(reported) {
        session.failures.extend(handed_off_failures(&facts));
        if endless {
            session.failures.extend(infinite_duration_failures(&facts));
        }
        session.notes.push(format!(
            "under the shadow: {} real browser frames in {:.1} s, page clock +{:.1} s, duration {:?}",
            facts.real_frames, facts.window_s, facts.clock_advance_s, facts.duration
        ));
    }
}

/// Строка с автозапуском без клика: страница сама запускает беззвучный баннер (и превью), а режим 2
/// включён. Протокол страницы — режима 1 (`handoff-mode.js`): `handed-off` открывает окно, `finish`
/// закрывает его и приносит метрики настоящих кадров баннера. Передачи быть не должно, а баннер обязан
/// играть у браузера.
pub(super) fn run_banner_row(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
    preset: SettingsPreset,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(stand, environment, browser, page_id, preset)? {
        Begun::Ready(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let run = session.current_run();
    let prefix = session.current_prefix();
    let marked = session.command_and_wait(&run, "handed-off");
    if session.record(marked).is_some() {
        std::thread::sleep(NO_HANDOFF_WINDOW);
        let probe = session.placeholder(&prefix);
        if let Some(view) = session.record(probe) {
            session
                .failures
                .extend(placeholder_failures("after the window", &view, false));
        }
    }
    let (end, mut failures, notes) = session.close(&[])?;
    failures.extend(session_count_failures(&end.host, 0));
    match &end.current_report {
        Ok(report) => failures.extend(report_plays_in_browser_failures(report)),
        Err(error) => failures.push(format!("no final report from the page: {error}")),
    }
    Ok(verdict(&end, failures, &notes))
}
