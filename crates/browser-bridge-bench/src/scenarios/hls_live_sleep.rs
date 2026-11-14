//! Сценарий `hls-live-sleep` (сессия 16d4): HLS-эфир под тенью переживает «сон» сервера.
//!
//! Страница `hls-live-sleep` кладёт в `<video src>` плейлист маршрута `/cdn/hls-live-sleep/<прогон>/`:
//! через 30 с после первого запроса плейлиста сервер на 20 с отвечает 503, а окно эфира едет дальше.
//! Это первая проверка механизмов 16d3 (ожидание сети и склейка окна) с НАСТОЯЩИМ экстеншеном,
//! посредником и плеером: процессные тесты 16d3 шли с поддельной страницей на Rust.
//!
//! Порядок:
//!   1. страница сама играет эфир, робот отдаёт вкладку, плеер показывает первые кадры — всё до сна;
//!   2. робот ждёт в журнале плеера строку склейки (сон + запас);
//!   3. после склейки — рост кадров между двумя свежими сводками (плеер идёт дальше по новому окну);
//!   4. конец прогона: страница отчитывается (жива под тенью, часы идут, буфер растёт).
//!
//! Судья [`judge::judge_hls_live_sleep`] чистый: журнал сервера (503 фону, сегменты нового окна),
//! журнал плеера (ожидание сети, склейка, кадры, откаты, отказы) и отчёт страницы.
//!
//! Firefox на Linux нативного HLS не играет — честное `unsupported` (каталог страниц).

mod journal;
mod judge;

use std::time::{Duration, Instant};

use self::journal::{REJOIN_MARKER, journal_needles};
use self::judge::{
    HLS_LIVE_SLEEP_PAGE, MIN_FRAMES_AFTER_REJOIN, SleepEvidence, StepEvidence, judge_hls_live_sleep,
};
use super::capture::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, judge_early_report};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{PlayerLog, read_host_evidence};
use crate::site::HLS_LIVE_SLEEP_SCHEDULE;

/// Запас ожидания строки склейки сверх момента пробуждения сервера: плееру нужно дождаться
/// очередного повтора (до 8 с), скачать плейлист и дочитать буфер до разрыва.
const REJOIN_GRACE: Duration = Duration::from_secs(40);
/// Сколько ждём рост кадров после склейки: две сводки по 10 с плюс запас.
const AFTER_REJOIN_WATCH: Duration = Duration::from_secs(35);

/// Прогон сценария в свежем окружении.
pub(super) fn run(stand: &Stand, browser: BrowserKind) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| drive(stand, environment, browser, started));
    StepResult::new(
        "hls-live-sleep",
        browser,
        HLS_LIVE_SLEEP_PAGE,
        verdict,
        started.elapsed(),
    )
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    run_started_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        HLS_LIVE_SLEEP_PAGE,
        "hlssleep",
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(judge_early_report(
                HLS_LIVE_SLEEP_PAGE,
                browser_kind,
                &report,
            ));
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("the page never became ready\n{log}")));
        }
    }
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("hand_off_tab: {other:?}\n{log}")));
        }
    }
    let handed_off_at = Instant::now();
    run.tell_page("handed-off");

    let mut steps: Vec<StepEvidence> = Vec::new();
    let mut first_frames_at: Option<Instant> = None;
    let granted = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    let granted_ok = granted.is_ok();
    steps.push(StepEvidence {
        name: "session-granted",
        result: granted,
    });
    if granted_ok {
        // Отдельный читатель журнала для строки склейки: общий курсор ждёт сводки кадров.
        let mut whole_log = PlayerLog::open(environment.player_log());
        run_steps(&mut run, &mut whole_log, &mut steps, &mut first_frames_at);
    }

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let journal = run
        .player_log
        .snapshot_lines_containing_any(&journal_needles())?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    let mut requests = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    Ok(judge_hls_live_sleep(&SleepEvidence {
        steps,
        requests,
        handed_off_at,
        first_frames_at,
        journal,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
    }))
}

/// Шаги под тенью; первый проваленный шаг останавливает остальные (итог соберёт судья).
fn run_steps(
    run: &mut HandoffRun<'_>,
    whole_log: &mut PlayerLog,
    steps: &mut Vec<StepEvidence>,
    first_frames_at: &mut Option<Instant>,
) {
    let mut record = |name: &'static str, result: Result<(), String>| -> bool {
        let ok = result.is_ok();
        steps.push(StepEvidence { name, result });
        ok
    };
    let presented = run
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
        .map(|_| ())
        .map_err(|error| format!("player did not present frames: {error}"));
    if presented.is_ok() {
        *first_frames_at = Some(Instant::now());
    }
    if !record("player-presents-frames", presented) {
        return;
    }
    // Строка склейки появится не раньше пробуждения сервера; часы сна идут от первого запроса
    // плейлиста страницей, то есть заведомо раньше этого момента — ожидание с запасом.
    let rejoin_timeout = HLS_LIVE_SLEEP_SCHEDULE.wakes_at() + REJOIN_GRACE;
    let rejoined = whole_log
        .wait_for_line(REJOIN_MARKER, rejoin_timeout)
        .map(|_| ())
        .map_err(|error| format!("no rejoin line in the player log: {error}"));
    if !record("player-rejoins-the-live-window", rejoined) {
        return;
    }
    // После склейки плеер идёт дальше: рост кадров между двумя свежими сводками.
    let grown = run
        .player_log
        .skip_history()
        .map_err(|error| error.to_string())
        .and_then(|()| {
            run.player_log
                .wait_for_frame_growth(MIN_FRAMES_AFTER_REJOIN, AFTER_REJOIN_WATCH)
                .map(|_| ())
                .map_err(|error| format!("frames did not grow after the rejoin: {error}"))
        });
    record("player-plays-after-the-rejoin", grown);
}

#[cfg(test)]
mod tests;
