//! Строки «что после возврата»: замена из второй вкладки (первая на паузе на своей секунде), плеер
//! страницы полностью рабочий после «Return», живой эфир возвращается к краю.

use std::time::{Duration, Instant};

use super::facts::PROCESS_FINISHED_LINE;
use super::judge::{
    describe, judge_returned_paused_between, judge_returned_playing, judge_silent_viewer_end,
};
use super::rows_return::{click_return_and_observe, close_and_expect_returned};
use super::steps::{
    Begun, RETURN_KIT_PAGE, RETURN_WINDOW_GONE_TIMEOUT, Session, begin, query_starting_at, verdict,
};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{parse_summary, read_host_evidence};
use crate::scenarios::capture::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES};
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::handoff_run::{HANDOFF_MODE_QUERY, PageReadiness};
use crate::scenarios::outcome::Verdict;
use crate::site::RunId;

/// С какой секунды вторая вкладка начинает сама: заведомо дальше, чем успеет уйти первая (её перед
/// открытием второй отматывают к 2-й секунде, к замене она на ~4-й), чтобы «пауза на своей секунде» не
/// спутать с чужой, и заведомо раньше конца ролика (30 с): вторая ещё ~15 с играет в плеере до закрытия.
const SECOND_TAB_START_S: u32 = 10;
/// Куда отмотать первую вкладку перед открытием второй, микросекунды.
const FIRST_TAB_REWIND_TO_MICROS: i64 = 2_000_000;
/// Сколько ждём второй пропуск посредника (замена — новый `open_session` того же плеера).
const SECOND_GRANT_TIMEOUT: Duration = Duration::from_secs(45);
/// Шаг опроса журнала посредника.
const GRANT_POLL: Duration = Duration::from_millis(250);
/// Метка периодической сводки плеера: у сводок нового сеанса счёт начинается заново, а итоговая
/// сводка прошлого сеанса (`kind=final`) в рост кадров второй вкладки не годится.
const PERIODIC_SUMMARY_MARK: &str = "Browser playback summary kind=periodic";
/// Хуки страницы после «Return»: своя перемотка, смена качества, следующая серия.
const AFTER_RETURN_HOOKS: &[&str] = &["hook:own-seek", "hook:quality-switch", "hook:next-episode"];
/// Окно плеера, ушедшее после «Return», не должно вернуться: проверяем с этим сроком.
const NO_WINDOW_CHECK: Duration = Duration::from_secs(1);

/// Строка 4: вкладка A передана, вкладку B открыли и передали → A на паузе на своей секунде,
/// B показывает кадры в том же окне плеера.
pub(super) fn run_replace(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(
        stand,
        environment,
        browser,
        RETURN_KIT_PAGE,
        HANDOFF_MODE_QUERY,
    )? {
        Begun::Running(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let first_run = session.current_run();
    let second = replace_steps(&mut session, &first_run);
    let (end, mut failures, notes) = match second {
        Some(second_run) => session.close(&[(&second_run, RETURN_KIT_PAGE)])?,
        None => session.close(&[])?,
    };
    if end.host.session_grants != 2 {
        failures.push(format!(
            "expected exactly two sessions (A, then B), the host granted {}",
            end.host.session_grants
        ));
    }
    Ok(verdict(&end, failures, &notes))
}

/// Шаги замены; возвращает прогон второй вкладки, если до неё дошло (её отчёт тоже судится).
/// В конце текущей остаётся вкладка A (её отчёт — главный), вкладка B отложена.
fn replace_steps(session: &mut Session<'_, '_>, first_run: &RunId) -> Option<RunId> {
    let first_prefix = session.current_prefix();
    let shown = session.wait_placeholder_shown(&first_prefix);
    session.record(shown)?;
    // A отматывается к началу: к замене она уйдёт на секунды вперёд, но не дойдёт до секунды B.
    let rewound = session
        .control
        .position_micros()
        .and_then(|position| {
            session
                .control
                .seek_by(FIRST_TAB_REWIND_TO_MICROS - position)
        })
        .map_err(|error| format!("rewinding tab A: {error}"));
    session.record(rewound)?;
    let opened = session
        .run()
        .park_and_open(
            RETURN_KIT_PAGE,
            "return-b",
            &query_starting_at(SECOND_TAB_START_S),
        )
        .map_err(|error| error.to_string());
    let parked_first = session.record(opened)?;
    let second_run = session.current_run();
    let ready = match session.run().wait_page_ready() {
        PageReadiness::Ready => Ok(()),
        _ => Err("tab B never became ready".to_owned()),
    };
    session.record(ready)?;
    // Отметка A прямо перед передачей B: дальше A идёт, пока плеер не заменит её сеанс.
    let marked = session.mark_return(first_run);
    session.record(marked)?;
    let marked_at = Instant::now();
    let handed = match session.run().hand_off_this_run() {
        Ok(HandOffOutcome::Started) => Ok(()),
        other => Err(format!("hand_off_tab for tab B: {other:?}")),
    };
    let replaced_after = marked_at.elapsed();
    session.record(handed)?;
    session.run().tell_page("handed-off");
    let second_plays = second_tab_presents_frames(session);
    session.record(second_plays);
    // A снова текущая и активная: её таймеры не душатся, наблюдение честное.
    let parked_second = session.run().swap_with_parked(parked_first);
    let activated = session
        .run()
        .activate_page_tab()
        .map_err(|error| error.to_string());
    session.record(activated);
    let observed = session.observe_return(first_run);
    if let Some(facts) = session.record(observed) {
        session.failures.extend(judge_returned_paused_between(
            &facts,
            facts.mark_s,
            facts.mark_s + replaced_after.as_secs_f64(),
        ));
        session.notes.push(format!(
            "tab A after the replacement ({:.1} s after its mark): {}",
            replaced_after.as_secs_f64(),
            describe(&facts)
        ));
    }
    // B: закрытие окна возвращает её на паузе, потом она играет сама (иначе её отчёт честно провалил
    // бы «страница видит игру»).
    let parked_first = session.run().swap_with_parked(parked_second);
    let activated = session
        .run()
        .activate_page_tab()
        .map_err(|error| format!("activating tab B: {error}"));
    session.record(activated);
    let closed = close_and_expect_returned(session);
    session.record(closed);
    session.run().swap_with_parked(parked_first);
    Some(second_run)
}

/// Второй пропуск посредника, затем кадры второго сеанса в ТОМ ЖЕ процессе плеера (окно не уходило).
fn second_tab_presents_frames(session: &mut Session<'_, '_>) -> Result<(), String> {
    let deadline = Instant::now() + SECOND_GRANT_TIMEOUT;
    loop {
        let grants = read_host_evidence(&session.run().stderr_log)
            .map_err(|error| error.to_string())?
            .session_grants;
        if grants >= 2 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the host granted no session to tab B within {SECOND_GRANT_TIMEOUT:?}"
            ));
        }
        std::thread::sleep(GRANT_POLL);
    }
    let frames_deadline = Instant::now() + FRAMES_TIMEOUT;
    loop {
        let remaining = frames_deadline.saturating_duration_since(Instant::now());
        let line = session
            .run()
            .player_log
            .wait_for_line(PERIODIC_SUMMARY_MARK, remaining)
            .map_err(|error| format!("tab B: no player summary: {error}"))?;
        if parse_summary(&line)
            .is_some_and(|summary| summary.presented_frames >= MIN_PRESENTED_FRAMES)
        {
            break;
        }
    }
    let process_finished = session
        .run()
        .player_log
        .snapshot_forbidden(&[PROCESS_FINISHED_LINE])
        .map_err(|error| error.to_string())?;
    if !process_finished.is_empty() {
        return Err(format!(
            "the player process exited between the tabs: {process_finished:?}"
        ));
    }
    session
        .control
        .wait_until_controllable(Duration::from_secs(5))
        .map_err(|error| format!("the player window is not there for tab B: {error}"))?;
    session
        .notes
        .push("tab B presents frames in the same player window".to_owned());
    Ok(())
}

/// Строка 6: после «Return» плеер страницы полностью рабочий — своя перемотка, смена качества,
/// следующая серия; браузер декодирует сам, нового сеанса и окна нет.
pub(super) fn run_works_after_return(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(
        stand,
        environment,
        browser,
        RETURN_KIT_PAGE,
        HANDOFF_MODE_QUERY,
    )? {
        Begun::Running(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let run = session.current_run();
    let prefix = session.current_prefix();
    if let Some(facts) = click_return_and_observe(&mut session, &run, &prefix) {
        session.failures.extend(judge_returned_playing(&facts));
        for hook in AFTER_RETURN_HOOKS {
            let done = session.tell_and_wait(hook);
            session.record(done.map_err(|problem| format!("{hook}: {problem}")));
        }
        let no_window = session
            .control
            .wait_until_gone(NO_WINDOW_CHECK)
            .map_err(|error| {
                format!("a player window is open after the page's own actions: {error}")
            });
        session.record(no_window);
        session.notes.push(format!(
            "after Return the page ran {AFTER_RETURN_HOOKS:?} by itself"
        ));
    }
    let (end, mut failures, notes) = session.close(&[])?;
    failures.extend(judge_silent_viewer_end(&end.player));
    if end.host.session_grants != 1 || end.player.sessions_opened != 1 {
        failures.push(format!(
            "the page's own actions after Return started a new session: {} grants, {} sessions opened",
            end.host.session_grants, end.player.sessions_opened
        ));
    }
    Ok(verdict(&end, failures, &notes))
}

/// Строка 8: живой эфир HLS (только Chromium) → «Return» → элемент играет сам у края эфира
/// (решение 234), окно закрылось молча.
pub(super) fn run_live_edge(
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
    let returned = live_return_steps(&mut session, &run, &prefix);
    session.record(returned);
    let (end, mut failures, notes) = session.close(&[])?;
    failures.extend(judge_silent_viewer_end(&end.player));
    Ok(verdict(&end, failures, &notes))
}

fn live_return_steps(
    session: &mut Session<'_, '_>,
    run: &RunId,
    prefix: &str,
) -> Result<(), String> {
    let aim = session.aim_at_return(run, prefix)?;
    session.tell_and_wait("hook:mark-before-return")?;
    session
        .hand
        .click(aim.button_point)
        .map_err(|error| error.to_string())?;
    let gone = session.wait_window_gone(RETURN_WINDOW_GONE_TIMEOUT);
    // Хук страницы эфира: элемент перезагрузил эфир, играет сам и стоит у края (`hls-live-page.js`).
    let at_edge = session.tell_and_wait("hook:expect-returned-live");
    let placeholder_gone = session.wait_placeholder_gone(prefix);
    if let Ok(took) = &gone {
        session.notes.push(format!(
            "live: window gone {:.1} s after Return",
            took.as_secs_f64()
        ));
    }
    gone?;
    at_edge?;
    placeholder_gone
}
