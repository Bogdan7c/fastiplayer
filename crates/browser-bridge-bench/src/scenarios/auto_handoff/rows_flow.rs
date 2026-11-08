//! Строки «цепочка событий»: реклама перед фильмом по клику (`preroll`), «Return» и новый клик
//! (`after-return`), три вкладки подряд (`three-tabs`). Каждая — несколько действий человека подряд на
//! настоящей мыши и приговор по уликам КАЖДОГО звена.

use std::time::Duration;

use super::facts::PAUSE_CLICKED_LINE;
use super::judge::{
    browser_plays_failures, click_reached_page_failures, handed_off_failures, placeholder_failures,
    preroll_metric_failures, session_count_failures, still_paused_failures,
};
use super::settings::SettingsPreset;
use super::steps::{AutoSession, Begun, begin, verdict};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::scenarios::capture::FRAMES_TIMEOUT;
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::handoff_run::{HANDOFF_MODE_QUERY, PageReadiness};
use crate::scenarios::outcome::Verdict;
use crate::site::RunId;

/// Страница фильма с кнопками Play и Pause.
const FILM_PAGE: &str = "auto-film";
/// Страница «реклама → фильм» по клику.
const PREROLL_PAGE: &str = "auto-preroll";

/// Окно «после Return» страница играет сама и новой передачи нет: путь передачи (клик → фон → посредник
/// → плеер) в других строках занимает единицы секунд, 15 с — с запасом, и всё окно страница меряет
/// настоящие кадры браузера.
const NO_REHANDOFF_WINDOW: Duration = Duration::from_secs(15);
/// Окно плеера после «Return» закрывается сразу и молча (решение 236): 6 с — тот же срок, что у `page-return`.
const RETURN_WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(6);
/// Окно наблюдения последней вкладки под тенью (не короче 6 с — порога судьи).
const LAST_TAB_SHADOW_WINDOW: Duration = Duration::from_secs(8);
/// Сколько ждём, пока страница запустит фильм после рекламы (реклама 10 с + запас на медленную машину).
const FILM_STARTED_TIMEOUT: Duration = Duration::from_secs(120);
/// Метка страницы: фильм запущен.
const FILM_STARTED_MARKER: &str = "film-started";
/// Метка плеера: окно показало зрителю сообщение о конце сеанса (сеанс не пережил переезд).
const END_NOTICE_MARKER: &str = "Browser session end notice shown";
/// Пауза между двумя снимками состояния старой вкладки: за неё живая секунда ушла бы далеко.
const STATE_RECHECK_PAUSE: Duration = Duration::from_secs(2);
/// Пауза после настоящего клика по Pause: страница успевает поставить видео на паузу.
const PAUSE_SETTLE: Duration = Duration::from_millis(800);
/// Сколько ждём, пока страница сообщит о клике по Pause.
const PAUSE_CLICK_WAIT: Duration = Duration::from_secs(5);

/// Строка `preroll`: реклама перед фильмом по клику. Передача начинается на рекламе (короткий ролик,
/// который экстеншен узнал как рекламу), а потом сеанс переезжает на фильм в том же окне.
pub(super) fn run_preroll(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(
        stand,
        environment,
        browser,
        PREROLL_PAGE,
        SettingsPreset::On,
    )? {
        Begun::Ready(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    preroll_steps(&mut session);
    let max_loads = session.run().player_log.snapshot_max_loads()?;
    let end_notices = session
        .run()
        .player_log
        .snapshot_forbidden(&[END_NOTICE_MARKER])?;
    let (end, mut failures, notes) = session.close(&[])?;
    failures.extend(session_count_failures(&end.host, 1));
    if !end_notices.is_empty() {
        failures.push(format!(
            "the window showed a session end notice, the session did not survive the hop: {}",
            end_notices.join(" | ")
        ));
    }
    if max_loads.is_none_or(|loads| loads < 2) {
        failures.push(format!(
            "the player summaries show loads={max_loads:?}, expected at least 2 (the ad and the film)"
        ));
    }
    match &end.current_report {
        Ok(report) => failures.extend(preroll_metric_failures(report)),
        Err(error) => failures.push(format!("no final report from the page: {error}")),
    }
    Ok(verdict(&end, failures, &notes))
}

fn preroll_steps(session: &mut AutoSession<'_, '_>) {
    let run = session.current_run();
    let clicked = session.click_target(&run, "play");
    if session.record(clicked).is_none() {
        return;
    }
    // Передача на рекламе: пропуск посредника и кадры плеера, пока идёт ролик-реклама.
    let started = session.await_handoff(1);
    if session.record(started).is_none() {
        return;
    }
    let lines = session.play_click_lines(&run);
    session.failures.extend(click_reached_page_failures(
        "the Play click on the ad",
        &lines,
    ));
    // Страница отсчитывает «настоящие кадры рекламы под тенью» от этой команды (как у `capture`).
    let handed = session.command_and_wait(&run, "handed-off");
    session.record(handed);
    let film = session
        .run()
        .wait_page_log_line(FILM_STARTED_MARKER, FILM_STARTED_TIMEOUT);
    if session.record(film).is_none() {
        return;
    }
    // Точка отсчёта — сводки ПОСЛЕ запуска фильма страницей: кадры рекламы в счёт не идут.
    let growth = session.run().player_log.skip_history().and_then(|()| {
        session
            .run()
            .player_log
            .wait_for_frame_growth(60, FRAMES_TIMEOUT)
    });
    match growth {
        Ok(found) => session.notes.push(format!(
            "film frames grew by {} in the player after the ad",
            found.frames_gained()
        )),
        Err(error) => session.failures.push(format!(
            "player frames did not grow after the film started: {error}"
        )),
    }
}

/// Строка `after-return`: передача по клику → настоящий «Return» → страница играет сама, нового
/// сеанса нет → настоящая пауза и новый клик по Play → передача снова.
pub(super) fn run_after_return(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(stand, environment, browser, FILM_PAGE, SettingsPreset::On)? {
        Begun::Ready(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    after_return_steps(&mut session);
    let (end, mut failures, notes) = session.close(&[])?;
    // Ровно два пропуска за весь прогон: первый клик и второй клик. «Return» третьего не породил.
    failures.extend(session_count_failures(&end.host, 2));
    Ok(verdict(&end, failures, &notes))
}

fn after_return_steps(session: &mut AutoSession<'_, '_>) {
    let run = session.current_run();
    let prefix = session.current_prefix();
    let clicked = session.click_target(&run, "play");
    if session.record(clicked).is_none() {
        return;
    }
    let started = session.await_handoff(1);
    if session.record(started).is_none() {
        return;
    }
    // Настоящий «Return»: окно запоминает секунду страницы и считает кадры браузера с этого момента.
    let marked = session.mark_window(&run);
    if session.record(marked).is_none() {
        return;
    }
    let button = session.return_button_point(&run, &prefix);
    let Some(button) = session.record(button) else {
        return;
    };
    let returned = session.click_point(button);
    if session.record(returned).is_none() {
        return;
    }
    let closed = session.wait_window_gone(RETURN_WINDOW_GONE_TIMEOUT);
    if let Some(elapsed) = session.record(closed) {
        session.notes.push(format!(
            "the player window closed {elapsed:.1?} after Return"
        ));
    }
    // Страница играет сама; передачи нет всё окно. Положительный факт — кадры браузера.
    std::thread::sleep(NO_REHANDOFF_WINDOW);
    let reported = session.report_window(&run);
    if let Some(facts) = session.record(reported) {
        session.failures.extend(browser_plays_failures(&facts));
        session.notes.push(format!(
            "after Return the page played by itself: {} real frames in {:.1} s",
            facts.real_frames, facts.window_s
        ));
    }
    let host = session.host_now();
    if let Some(host) = session.record(host) {
        session.failures.extend(
            session_count_failures(&host, 1)
                .into_iter()
                .map(|problem| format!("after Return: {problem}")),
        );
    }
    let gone = session.wait_placeholder_gone(&prefix);
    session.record(gone);
    // Настоящая пауза зрителя, потом новый настоящий клик по Play.
    let paused = pause_with_real_click(session, &run);
    if session.record(paused).is_none() {
        return;
    }
    let played = session.click_target(&run, "play");
    if session.record(played).is_none() {
        return;
    }
    let again = session.await_handoff(2);
    if session.record(again).is_none() {
        return;
    }
    let shown = session.wait_placeholder_shown(&prefix);
    if let Some(view) = session.record(shown) {
        session
            .failures
            .extend(placeholder_failures("after the second click", &view, true));
    }
    let lines = session.play_click_lines(&run);
    if lines.len() < 2 {
        session.failures.push(format!(
            "the page saw {} click(s) on Play, expected two (before and after Return)",
            lines.len()
        ));
    }
}

/// Настоящий клик по Pause: страница подтверждает клик и стоит на паузе.
fn pause_with_real_click(session: &mut AutoSession<'_, '_>, run: &RunId) -> Result<(), String> {
    session.click_target(run, "pause")?;
    std::thread::sleep(PAUSE_SETTLE);
    let deadline = std::time::Instant::now() + PAUSE_CLICK_WAIT;
    while session.page_lines(run, PAUSE_CLICKED_LINE).is_empty() {
        if std::time::Instant::now() >= deadline {
            return Err("the page never saw the real click on Pause".to_owned());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let state = session.report_state(run)?;
    if state.paused {
        Ok(())
    } else {
        Err("the page is not paused after the real click on Pause".to_owned())
    }
}

/// Строка `three-tabs`: три вкладки подряд, настоящий клик в каждой. Играет последняя, две первые
/// стоят на паузе на своих секундах.
pub(super) fn run_three_tabs(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(stand, environment, browser, FILM_PAGE, SettingsPreset::On)? {
        Begun::Ready(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let runs = three_tabs_steps(&mut session);
    let others: Vec<(&RunId, &str)> = runs.iter().map(|run| (run, FILM_PAGE)).collect();
    let (end, mut failures, notes) = session.close(&others)?;
    failures.extend(session_count_failures(&end.host, 3));
    // Кадры браузера последней вкладки судит её окно наблюдения (`handed_off_failures`): к концу шагов
    // текущая страница — уже вторая вкладка, и её отчёт про последнюю ничего не знает.
    Ok(verdict(&end, failures, &notes))
}

/// Шаги трёх вкладок. Возвращает прогоны ВСЕХ вкладок, кроме той, что осталась текущей к концу шагов.
fn three_tabs_steps(session: &mut AutoSession<'_, '_>) -> Vec<RunId> {
    let first_run = session.current_run();
    let first_click = click_and_hand_off(session, &first_run, 1);
    if session.record(first_click).is_none() {
        return Vec::new();
    }
    // Вкладки 2 и 3 открываются новыми; прежняя откладывается вместе с её сеансом.
    let second = open_and_hand_off(session, "auto-b", 2);
    let Some((parked_first, second_run)) = second else {
        return Vec::new();
    };
    let third = open_and_hand_off(session, "auto-c", 3);
    let Some((parked_second, third_run)) = third else {
        return Vec::new();
    };
    // Последняя вкладка играет в плеере: кадры браузера под тенью почти нет, часы идут.
    let third_prefix = session.current_prefix();
    let marked = session.mark_window(&third_run);
    if session.record(marked).is_some() {
        std::thread::sleep(LAST_TAB_SHADOW_WINDOW);
        let reported = session.report_window(&third_run);
        if let Some(facts) = session.record(reported) {
            session.failures.extend(handed_off_failures(&facts));
        }
    }
    let shown = session.wait_placeholder_shown(&third_prefix);
    session.record(shown);
    // Первые две стоят на паузе на своих секундах и без заглушки (их сеансы заменены).
    let parked_third = session.run().swap_with_parked(parked_first);
    check_replaced_tab(session, "tab 1", &first_run);
    let parked_first = session.run().swap_with_parked(parked_second);
    check_replaced_tab(session, "tab 2", &second_run);
    // Текущей остаётся вкладка 2; вкладки 1 и 3 — в списке остальных для итоговых отчётов.
    drop((parked_first, parked_third));
    vec![first_run, third_run]
}

/// Вкладка (текущая) заменена: пауза на своей секунде в двух снимках подряд и никакой заглушки.
fn check_replaced_tab(session: &mut AutoSession<'_, '_>, label: &str, run: &RunId) {
    let activated = session
        .run()
        .activate_page_tab()
        .map_err(|error| format!("{label}: activating the tab: {error}"));
    if session.record(activated).is_none() {
        return;
    }
    let prefix = session.current_prefix();
    let first = session.report_state(run);
    std::thread::sleep(STATE_RECHECK_PAUSE);
    let second = session.report_state(run);
    if let (Some(first), Some(second)) = (session.record(first), session.record(second)) {
        session
            .failures
            .extend(still_paused_failures(label, &first, &second));
        session
            .notes
            .push(format!("{label} stands paused at {:.1} s", second.time_s));
    }
    // Сеанс вкладки заменён: заглушка уходит (не сразу, а за доли секунды).
    let gone = session
        .wait_placeholder_gone(&prefix)
        .map_err(|problem| format!("{label}: {problem}"));
    session.record(gone);
}

/// Настоящий клик по Play текущей вкладки и ожидание `grants`-й передачи.
fn click_and_hand_off(
    session: &mut AutoSession<'_, '_>,
    run: &RunId,
    grants: u32,
) -> Result<(), String> {
    session.click_target(run, "play")?;
    session.await_handoff(grants)?;
    let lines = session.play_click_lines(run);
    if let Some(problem) = click_reached_page_failures(&format!("tab {grants}"), &lines)
        .into_iter()
        .next()
    {
        return Err(problem);
    }
    Ok(())
}

/// Открывает следующую вкладку (прежняя откладывается), ждёт её готовности, кликает и ждёт передачи.
/// Возвращает отложенную прежнюю страницу и прогон новой.
fn open_and_hand_off(
    session: &mut AutoSession<'_, '_>,
    run_prefix: &str,
    grants: u32,
) -> Option<(crate::scenarios::handoff_run::ParkedPage, RunId)> {
    let opened = session
        .run()
        .park_and_open(FILM_PAGE, run_prefix, HANDOFF_MODE_QUERY)
        .map_err(|error| format!("opening tab {grants}: {error}"));
    let parked = session.record(opened)?;
    let ready = match session.run().wait_page_ready() {
        PageReadiness::Ready => Ok(()),
        _ => Err(format!("tab {grants} never became ready")),
    };
    session.record(ready)?;
    let run = session.current_run();
    let handed = click_and_hand_off(session, &run, grants);
    session.record(handed)?;
    Some((parked, run))
}
