//! Сценарий `hls-live` (сессия 10a3): родной HLS LIVE под тенью экстеншена.
//!
//! Страница `hls-live` кладёт в `<video src>` плейлист «живого эфира» (маршрут стенда
//! `/cdn/hls-live/<прогон>/stream.m3u8`: скользящее окно сегментов, без `ENDLIST`). Порядок:
//!   1. страница сама играет эфир, проверяет `duration === Infinity` и ход `seekable`;
//!   2. робот «отдаёт вкладку» плееру, страница запоминает точку отсчёта (`hook:mark-live`);
//!   3. плеер показывает кадры, пока эфир идёт: рост кадров между двумя сводками ЗА [`LIVE_WATCH`]
//!      (окно эфира за это время уезжает далеко от начального);
//!   4. страница под тенью всё ещё видит эфир: `duration` бесконечна, конец `seekable` растёт,
//!      часы страницы идут (`hook:expect-live-window`);
//!   5. закрытие окна плеера (`Quit`): настоящий элемент сам снова играет (без play страницы, решение
//!      234) и стоит у края эфира, а не на «старой секунде» тени (`hook:expect-returned-live`).
//!
//! Судья [`judge::judge_hls_live`] сверяет всё это с журналом сервера: элемент под тенью не
//! качал, фон обновлял плейлист и шёл вслед за эфиром, после возврата элемент перезагрузил `src`.
//!
//! Firefox на Linux нативного HLS не играет — честное `unsupported` (каталог страниц).

mod judge;

use std::time::{Duration, Instant};

use self::judge::{HlsLiveEvidence, MIN_FRAMES_WHILE_LIVE, StepEvidence, judge_hls_live};
use super::capture::{MIN_PRESENTED_FRAMES, judge_early_report};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::player_commands::{CONTROLLABLE_TIMEOUT, FRAMES_TIMEOUT, tell_and_wait};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameGrowth, read_host_evidence};
use crate::player_control::PlayerControl;

/// Страница сценария.
pub(super) const HLS_LIVE_PAGE: &str = "hls-live";
/// Сколько ждём рост кадров между двумя сводками плеера (сводки раз в 10 с): это и есть время,
/// которое эфир идёт под тенью, пока окно уезжает от начального.
const LIVE_WATCH: Duration = Duration::from_secs(25);

/// Прогон сценария в свежем окружении.
pub(super) fn run(stand: &Stand, browser: BrowserKind) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| drive(stand, environment, browser, started));
    StepResult::new(
        "hls-live",
        browser,
        HLS_LIVE_PAGE,
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
        HLS_LIVE_PAGE,
        "hlslive",
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(judge_early_report(HLS_LIVE_PAGE, browser_kind, &report));
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
    let mut frame_growth: Result<FrameGrowth, String> =
        Err("the live frames step did not run".to_owned());
    let mut returned_at: Option<Instant> = None;
    let granted = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    let granted_ok = granted.is_ok();
    steps.push(StepEvidence {
        name: "session-granted",
        result: granted,
    });
    if granted_ok {
        let control = PlayerControl::on_bus(environment.bus_address()?);
        run_live_steps(
            &mut run,
            &control,
            &mut steps,
            &mut frame_growth,
            &mut returned_at,
        );
    }

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    let mut requests = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    Ok(judge_hls_live(&HlsLiveEvidence {
        steps,
        frame_growth,
        requests,
        handed_off_at,
        returned_at,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
    }))
}

/// Шаги под тенью; первый проваленный шаг останавливает остальные (кроме рост-кадров: он итог).
fn run_live_steps(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    steps: &mut Vec<StepEvidence>,
    frame_growth: &mut Result<FrameGrowth, String>,
    returned_at: &mut Option<Instant>,
) {
    let mut record = |name: &'static str, result: Result<(), String>| -> bool {
        let ok = result.is_ok();
        steps.push(StepEvidence { name, result });
        ok
    };
    // Точка отсчёта страницы сразу после передачи: конец seekable и часы страницы.
    if !record("mark-live", tell_and_wait(run, "hook:mark-live")) {
        return;
    }
    let presented = run
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
        .map(|_| ())
        .map_err(|error| format!("player did not present frames: {error}"));
    if !record("player-presents-frames", presented) {
        return;
    }
    let controllable = control
        .wait_until_controllable(CONTROLLABLE_TIMEOUT)
        .map_err(|error| error.to_string());
    if !record("window-mpris-controllable", controllable) {
        return;
    }
    // Эфир идёт под тенью: рост кадров между двумя сводками за LIVE_WATCH. Журнал до этого места
    // (первые кадры) в счёт не идёт: нужна именно непрерывная игра.
    *frame_growth = run
        .player_log
        .skip_history()
        .map_err(|error| format!("player log: {error}"))
        .and_then(|()| {
            run.player_log
                .wait_for_frame_growth(MIN_FRAMES_WHILE_LIVE, LIVE_WATCH)
                .map_err(|error| format!("player frames did not keep growing while live: {error}"))
        });
    // Страница под тенью видит эфир — проверяем и при провале кадров (разбор нужен весь).
    record(
        "page-sees-live-window",
        tell_and_wait(run, "hook:expect-live-window"),
    );
    // Возврат: страница запоминает «старую секунду», окно плеера закрывается.
    if !record(
        "mark-before-return",
        tell_and_wait(run, "hook:mark-before-return"),
    ) {
        return;
    }
    if !record(
        "close-player-window",
        control
            .quit_window()
            .map_err(|error| format!("Quit: {error}")),
    ) {
        return;
    }
    *returned_at = Some(Instant::now());
    record(
        "returned-to-the-live-edge",
        tell_and_wait(run, "hook:expect-returned-live"),
    );
}

#[cfg(test)]
mod tests {
    use super::super::page_catalog::PAGE_SCENARIOS;
    use super::*;

    fn page_source() -> String {
        let site = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site");
        let mut source = std::fs::read_to_string(site.join("pages/hls-live.html")).expect("page");
        source.push_str(
            &std::fs::read_to_string(site.join("lib/hls-live-page.js")).expect("page lib"),
        );
        source
    }

    /// Команды робота — те, что страница объявила; иначе стенд упал бы только на живом прогоне.
    #[test]
    fn every_hook_the_robot_sends_is_declared_by_the_page() {
        let source = page_source();
        for command in [
            "mark-live",
            "expect-live-window",
            "mark-before-return",
            "expect-returned-live",
        ] {
            assert!(
                source.contains(&format!("'{command}'")),
                "hls-live-page.js does not declare the hook '{command}'"
            );
        }
    }

    #[test]
    fn the_page_is_in_the_catalog_and_is_unsupported_only_in_firefox() {
        let page = PAGE_SCENARIOS
            .iter()
            .find(|scenario| scenario.id == HLS_LIVE_PAGE)
            .expect("hls-live is in the catalog");
        assert!(page.may_be_unsupported_in(BrowserKind::Firefox));
        assert!(!page.may_be_unsupported_in(BrowserKind::Chromium));
    }

    /// Страница берёт эфир из маршрута стенда с тем же префиксом, что знает судья.
    #[test]
    fn the_page_plays_the_stand_live_route() {
        assert!(page_source().contains(crate::site::HLS_LIVE_ROUTE_PREFIX));
    }
}
