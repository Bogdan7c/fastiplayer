//! Сценарий `live-ends` (сессия 16d1): эфир, который посреди игры становится записью.
//!
//! Две строки на браузер, обе под настоящим экстеншеном, посредником и плеером:
//!  * `mse-live-ends` (Chromium и Firefox): MSE-страница открывает эфир (`duration = Infinity`),
//!    докладывает короткий fMP4 стенда, а по команде робота ставит конечную длину и зовёт
//!    `endOfStream()` — как hls.js / Shaka / dash.js в конце прямого эфира;
//!  * `hls-live-ends` (только Chromium; в Firefox честное `unsupported`, как у `hls-live`): родной
//!    HLS в `<video src>`, маршрут стенда `/cdn/hls-live-ends/` дописывает `#EXT-X-ENDLIST` после
//!    фиксированного числа сегментов и замораживает окно. Под тенью длина эфира должна стать
//!    конечной ОДНИМ `durationchange` (сообщение плеера `recording_window`, протокол 1.11).
//!
//! Порядок шагов (общий): передача вкладки -> плеер показал кадры -> страница подтвердила, что под
//! тенью эфир идёт (`hook:mark-on-air`) -> [MSE: `hook:end-live`] -> страница ждёт конечную длину и
//! `ended` (`hook:expect-recording`) -> в журнале плеера строка перехода и конец воспроизведения.
//!
//! Судья [`judge::judge_live_ends`] чистый: журнал плеера (строка перехода, кадры после неё, конец
//! записи, отказы потока) и метрики страницы. `hls-live` и его судья не затронуты.

mod journal;
mod judge;

use std::time::{Duration, Instant};

use self::journal::{RECORDING_MARKER, journal_needles};
use self::judge::{LiveEndsEvidence, LiveEndsRow, StepEvidence, judge_live_ends};
use super::capture::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, judge_early_report};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::player_commands::tell_and_wait;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{PLAYBACK_ENDED_MARKER, PlayerLog, read_host_evidence};

/// Строки таблицы в порядке прогона.
const ROWS: [LiveEndsRow; 2] = [LiveEndsRow::MseLiveEnds, LiveEndsRow::HlsLiveEnds];
/// Сколько ждём в журнале плеера строку перехода и строку конца записи после ответа страницы.
const JOURNAL_LINE_TIMEOUT: Duration = Duration::from_secs(30);

/// Обе строки таблицы в одном браузере.
pub(super) fn run(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    ROWS.iter()
        .map(|&row| {
            let started = Instant::now();
            let verdict =
                stand.with_environment(|environment| drive(stand, environment, browser, row));
            StepResult::new(
                "live-ends",
                browser,
                row.page_id(),
                verdict,
                started.elapsed(),
            )
        })
        .collect()
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    row: LiveEndsRow,
) -> Result<Verdict, ScenarioError> {
    let page_id = row.page_id();
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        page_id,
        "liveends",
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(judge_early_report(page_id, browser_kind, &report));
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
    run.tell_page("handed-off");

    let mut steps: Vec<StepEvidence> = Vec::new();
    let granted = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    let granted_ok = granted.is_ok();
    steps.push(StepEvidence {
        name: "session-granted",
        result: granted,
    });
    if granted_ok {
        // Отдельный читатель журнала: общий курсор мог уйти за строки, которые ждём ниже.
        let mut whole_log = PlayerLog::open(environment.player_log());
        run_steps(&mut run, &mut whole_log, row, &mut steps);
    }

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let journal = run
        .player_log
        .snapshot_lines_containing_any(&journal_needles())?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_live_ends(&LiveEndsEvidence {
        row,
        steps,
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
    row: LiveEndsRow,
    steps: &mut Vec<StepEvidence>,
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
    if !record("player-presents-frames", presented) {
        return;
    }
    // Эфир под тенью ещё идёт: бесконечная длина видна странице.
    if !record(
        "page-sees-the-stream-on-air",
        tell_and_wait(run, "hook:mark-on-air"),
    ) {
        return;
    }
    // MSE: конец эфира делает сама страница. У HLS его делает сервер стенда по часам (ENDLIST).
    if row == LiveEndsRow::MseLiveEnds
        && !record(
            "page-ends-the-live-stream",
            tell_and_wait(run, "hook:end-live"),
        )
    {
        return;
    }
    // Страница ждёт конечную длину и `ended`; ответ приходит, когда оба факта наблюдены.
    let recording = tell_and_wait(run, "hook:expect-recording");
    let page_saw_the_end = recording.is_ok();
    record("page-sees-the-recording-and-its-end", recording);
    // Строки журнала плеера: переход и конец записи (ждём, даже если страница не дошла: так в
    // разборе видно, что сделал плеер).
    record(
        "player-logs-the-recording",
        wait_for_journal_line(whole_log, RECORDING_MARKER),
    );
    if page_saw_the_end {
        record(
            "player-plays-to-the-end",
            wait_for_journal_line(whole_log, PLAYBACK_ENDED_MARKER),
        );
    }
}

/// Ждёт в журнале плеера строку с `marker`; ошибка — текст для шага.
fn wait_for_journal_line(log: &mut PlayerLog, marker: &str) -> Result<(), String> {
    log.wait_for_line(marker, JOURNAL_LINE_TIMEOUT)
        .map(|_| ())
        .map_err(|error| format!("no '{marker}' line in the player log: {error}"))
}

#[cfg(test)]
mod tests;
