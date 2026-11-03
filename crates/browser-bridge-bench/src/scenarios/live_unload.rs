//! `player-commands`, страницы «живой эфир как Twitch» (решение 194, дефект 1 сессии 09b2).
//!
//! Страница (`site/lib/live-unload-page.js`) на свою паузу «выгружает» эфир (весь буфер удалён,
//! `currentTime = 0`) и загружает его заново ТОЛЬКО из обработчика Media Session `play`; голый
//! `play` элемента в выгруженном состоянии она тут же снова ставит на паузу — как Twitch.
//!
//! Порядок под тенью:
//!   1. плеер показал кадры, окно управляется по MPRIS;
//!   2. MPRIS `Pause` → страница выгрузила эфир (`hook:expect-unloaded`);
//!   3. точка отсчёта — первая сводка плеера на паузе (кадры не растут);
//!   4. MPRIS `Play` → страница загрузила эфир заново и играет (`hook:expect-reloaded`);
//!   5. ДО КАДРА: плеер показал не меньше [`MIN_FRAMES_AFTER_PLAY`] новых кадров.
//!
//! До решения 194 шаг 4 и 5 падали: тень показывала странице `play` на элементе, страница в
//! состоянии «выгружен» снова вставала на паузу, новых данных не было — эфир стоял навсегда.

use std::time::{Duration, Instant};

use super::capture::MIN_PRESENTED_FRAMES;
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines, wait_for_session_granted,
};
use super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_catalog::PAGE_SCENARIOS;
use super::page_filter::PageFilter;
use super::player_commands::{
    BASELINE_SUMMARY_TIMEOUT, CONTROLLABLE_TIMEOUT, FRAMES_TIMEOUT, tell_and_wait,
};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameEvidence, HostLogEvidence, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::site::{PageReport, PageStatus, SiteError};

/// Страницы «живой эфир как Twitch»: загрузка в том же MediaSource, новым MediaSource и в воркере.
pub(super) const LIVE_UNLOAD_PAGES: &[&str] =
    &["live-unload", "live-unload-new-ms", "live-unload-worker"];
/// Сколько новых кадров плеер обязан показать после play зрителя: «эфир ожил», а не «один кадр».
const MIN_FRAMES_AFTER_PLAY: u64 = 50;
/// Сколько ждём эти кадры. Сводки плеера идут раз в 10 с, поэтому окно — две сводки с запасом.
const FRAMES_AFTER_PLAY_TIMEOUT: Duration = Duration::from_secs(25);

/// Прогон страниц эфира, выбранных фильтром.
pub(super) fn run(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    LIVE_UNLOAD_PAGES
        .iter()
        .filter(|page| filter.admits(page))
        .map(|page| {
            let started = Instant::now();
            let verdict =
                stand.with_environment(|environment| drive(stand, environment, browser, page));
            StepResult::new(
                "player-commands",
                browser,
                *page,
                verdict,
                started.elapsed(),
            )
        })
        .collect()
}

/// Результат одного шага.
struct StepEvidence {
    name: &'static str,
    result: Result<(), String>,
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page_id: &'static str,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        page_id,
        "live",
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(early_report_verdict(&report, page_id, browser_kind));
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
    let mut frames: Option<FramesAroundPlay> = None;
    let granted = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    let granted_ok = granted.is_ok();
    steps.push(StepEvidence {
        name: "session-granted",
        result: granted,
    });
    if granted_ok {
        let control = PlayerControl::on_bus(environment.bus_address()?);
        frames = run_live_steps(&mut run, &control, &mut steps);
    }

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_live_unload(&LiveUnloadEvidence {
        page_id,
        steps,
        frames,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
    }))
}

/// Страница отчиталась до передачи: честное «не умею» там, где каталог это разрешает, иначе провал.
fn early_report_verdict(report: &PageReport, page_id: &str, browser: BrowserKind) -> Verdict {
    let allowed = PAGE_SCENARIOS
        .iter()
        .find(|scenario| scenario.id == page_id)
        .is_some_and(|scenario| scenario.may_be_unsupported_in(browser));
    match report.status {
        PageStatus::Unsupported if allowed => Verdict::unsupported("the page is unsupported here"),
        status => Verdict::fail(format!("the page reported {status:?} before the hand-off")),
    }
}

/// Кадры плеера вокруг play зрителя: точка отсчёта на паузе и итог после play.
struct FramesAroundPlay {
    baseline: FrameEvidence,
    after_play: Result<FrameEvidence, String>,
}

/// Шаги под тенью; останавливается на первом проваленном (кроме «до кадра» — он итог).
fn run_live_steps(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    steps: &mut Vec<StepEvidence>,
) -> Option<FramesAroundPlay> {
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
        return None;
    }
    let controllable = control
        .wait_until_controllable(CONTROLLABLE_TIMEOUT)
        .map_err(|error| error.to_string());
    if !record("window-mpris-controllable", controllable) {
        return None;
    }
    // 2. Пауза зрителя → страница выгружает эфир (как Twitch).
    let unloaded = control
        .pause()
        .map_err(|error| format!("Pause: {error}"))
        .and_then(|()| tell_and_wait(run, "hook:expect-unloaded"));
    if !record("pause-unloads-the-live-stream", unloaded) {
        return None;
    }
    // 3. Точка отсчёта: первая сводка ПОСЛЕ паузы вобрала все кадры, показанные до неё.
    let baseline = run
        .player_log
        .skip_history()
        .map_err(|error| format!("player log: {error}"))
        .and_then(|()| {
            run.player_log
                .wait_for_presented_frames(0, BASELINE_SUMMARY_TIMEOUT)
                .map_err(|error| format!("no player summary after the pause: {error}"))
        });
    let baseline = match baseline {
        Ok(baseline) => baseline,
        Err(message) => {
            record("baseline-summary-on-pause", Err(message));
            return None;
        }
    };
    // 4. Play зрителя → страница загружает эфир заново. Провал страницы не прерывает шаг 5:
    //    рост кадров плеера — главное доказательство, его итог нужен в любом случае.
    let reloaded = control
        .play()
        .map_err(|error| format!("Play: {error}"))
        .and_then(|()| tell_and_wait(run, "hook:expect-reloaded"));
    record("play-reloads-the-live-stream", reloaded);
    // 5. До кадра: плеер показывает новые кадры.
    let after_play = run
        .player_log
        .wait_for_presented_frames(
            baseline.presented_frames + MIN_FRAMES_AFTER_PLAY,
            FRAMES_AFTER_PLAY_TIMEOUT,
        )
        .map_err(|error| format!("player frames did not grow after the viewer's play: {error}"));
    Some(FramesAroundPlay {
        baseline,
        after_play,
    })
}

/// Всё, что собрал прогон, для чистого вердикта.
struct LiveUnloadEvidence {
    page_id: &'static str,
    steps: Vec<StepEvidence>,
    /// `None` — до паузы и точки отсчёта не дошли (причина — в проваленном шаге).
    frames: Option<FramesAroundPlay>,
    host: HostLogEvidence,
    page_report: Result<PageReport, SiteError>,
    forbidden_player_lines: Vec<String>,
    page_log: String,
}

/// Чистое решение: все шаги прошли, страница согласна, плеер показал новые кадры после play.
fn judge_live_unload(evidence: &LiveUnloadEvidence) -> Verdict {
    let mut failures: Vec<String> = evidence
        .steps
        .iter()
        .filter_map(|step| {
            step.result
                .as_ref()
                .err()
                .map(|message| format!("step '{}': {message}", step.name))
        })
        .collect();
    let gained = match &evidence.frames {
        Some(frames) => match &frames.after_play {
            Ok(after) => Some(
                after
                    .presented_frames
                    .saturating_sub(frames.baseline.presented_frames),
            ),
            Err(message) => {
                failures.push(message.clone());
                None
            }
        },
        None => None,
    };
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, evidence.page_id));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let done: Vec<&str> = evidence
        .steps
        .iter()
        .filter(|step| step.result.is_ok())
        .map(|step| step.name)
        .collect();
    let summary = format!(
        "steps done: {}; frames after the viewer's play: {gained:?} (need {MIN_FRAMES_AFTER_PLAY})",
        done.join(", ")
    );
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::RunId;

    fn frames(presented_frames: u64) -> FrameEvidence {
        FrameEvidence {
            presented_frames,
            position_regressions: 0,
            summary_line: String::new(),
        }
    }

    fn report(status: PageStatus) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "live-unload".into(),
            status,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }
    }

    fn healthy() -> LiveUnloadEvidence {
        let steps = [
            "session-granted",
            "player-presents-frames",
            "window-mpris-controllable",
            "pause-unloads-the-live-stream",
            "play-reloads-the-live-stream",
        ]
        .into_iter()
        .map(|name| StepEvidence {
            name,
            result: Ok(()),
        })
        .collect();
        LiveUnloadEvidence {
            page_id: "live-unload",
            steps,
            frames: Some(FramesAroundPlay {
                baseline: frames(200),
                after_play: Ok(frames(320)),
            }),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            page_report: Ok(report(PageStatus::Pass)),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".into(),
        }
    }

    #[test]
    fn a_live_stream_that_resumes_after_play_passes_and_counts_the_new_frames() {
        let verdict = judge_live_unload(&healthy());
        assert_eq!(verdict.outcome, Outcome::Pass);
        assert!(
            verdict
                .detail
                .contains("frames after the viewer's play: Some(120)")
        );
    }

    #[test]
    fn no_new_frames_after_play_fails_even_if_the_page_agrees() {
        let mut evidence = healthy();
        evidence.frames = Some(FramesAroundPlay {
            baseline: frames(200),
            after_play: Err("player frames did not grow after the viewer's play".into()),
        });
        let verdict = judge_live_unload(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("did not grow after the viewer's play")
        );
    }

    #[test]
    fn a_page_that_stays_unloaded_is_named_in_the_failure() {
        let mut evidence = healthy();
        for step in &mut evidence.steps {
            if step.name == "play-reloads-the-live-stream" {
                step.result = Err("state=unloaded, repauses=1".into());
            }
        }
        let verdict = judge_live_unload(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("step 'play-reloads-the-live-stream'")
        );
        assert!(
            verdict.detail.contains("page log"),
            "the page log explains the failure"
        );
    }

    #[test]
    fn an_early_stop_reports_the_step_not_missing_frames() {
        let mut evidence = healthy();
        evidence.steps = vec![StepEvidence {
            name: "session-granted",
            result: Err("host did not grant".into()),
        }];
        evidence.frames = None;
        let verdict = judge_live_unload(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("step 'session-granted'"));
        assert!(
            verdict
                .detail
                .contains("frames after the viewer's play: None")
        );
    }

    #[test]
    fn unsupported_before_the_hand_off_is_honest_only_where_the_catalog_allows_it() {
        let unsupported = report(PageStatus::Unsupported);
        let worker_in_firefox =
            early_report_verdict(&unsupported, "live-unload-worker", BrowserKind::Firefox);
        assert_eq!(worker_in_firefox.outcome, Outcome::Unsupported);
        let worker_in_chromium =
            early_report_verdict(&unsupported, "live-unload-worker", BrowserKind::Chromium);
        assert_eq!(worker_in_chromium.outcome, Outcome::Fail);
        let main_thread = early_report_verdict(&unsupported, "live-unload", BrowserKind::Firefox);
        assert_eq!(main_thread.outcome, Outcome::Fail);
    }

    #[test]
    fn every_live_page_is_in_the_catalog_and_declares_both_hooks() {
        let library = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site/lib/live-unload-page.js"),
        )
        .expect("read live-unload-page.js");
        for hook in ["'expect-unloaded'", "'expect-reloaded'"] {
            assert!(
                library.contains(hook),
                "live-unload-page.js lacks the hook {hook}"
            );
        }
        for page in LIVE_UNLOAD_PAGES {
            assert!(
                PAGE_SCENARIOS.iter().any(|scenario| scenario.id == *page),
                "{page} is not in the page catalog"
            );
        }
    }
}
