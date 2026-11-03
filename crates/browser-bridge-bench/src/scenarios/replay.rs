//! `replay`: повтор записи потока страницы (`*.fprec`) на странице стенда `replay-recording`.
//!
//! Цепочка: файл разобран и проверен → сайт отдаёт его странице → страница повторяет события
//! настоящими вызовами MSE на своём `<video>` в исходном темпе → в момент `handoff_began`
//! записи робот «отдаёт вкладку» настоящему экстеншену (служебная страница, как в `capture`) →
//! посредник будит плеер → плеер показывает кадры. Прогон держится до конца записи + запас и
//! печатает сводки плеера (`presented_frames`, `position_ms`, `state`).
//!
//! Судья — по образцу `capture/judge.rs`: чистая функция [`judge_replay`] над собранными
//! уликами; порог кадров задаёт `--min-frames`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines, wait_for_session_granted,
};
use super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameEvidence, HostLogEvidence, SummaryDigest, read_host_evidence};
use crate::recording::Recording;
use crate::site::{PageReport, PageStatus, SiteError};

/// Идентификатор страницы повтора (`site/pages/replay-recording.html`).
const REPLAY_PAGE_ID: &str = "replay-recording";
/// Отметка журнала страницы: наступил `handoff_began` записи.
const HANDOFF_POINT_MARKER: &str = "replay-handoff-point";
/// Запас после последнего события записи, который страница ждёт перед отчётом, мс.
const TAIL_AFTER_RECORDING_MS: u64 = 8000;
/// Запас на загрузку страницы, воркеров и ожидание `sourceopen` до момента передачи.
const PAGE_START_SLACK: Duration = Duration::from_secs(60);
/// Запас на отчёт страницы сверх длительности записи (барьеры `updateend` могут растянуть повтор).
const REPORT_SLACK: Duration = Duration::from_secs(90);
/// Сколько ждать нужную сводку плеера после отчёта страницы (сводки раз в 10 с).
const FRAMES_AFTER_REPORT_TIMEOUT: Duration = Duration::from_secs(30);
/// Порог кадров по умолчанию: 2 с видео при 25 к/с (как у `capture`).
pub const DEFAULT_MIN_FRAMES: u64 = 50;

/// Что просит пользователь командой `replay`.
#[derive(Debug, Clone)]
pub struct ReplayOptions {
    /// Файл `*.fprec`.
    pub recording: PathBuf,
    /// Сколько кадров обязан показать плеер.
    pub min_frames: u64,
}

/// Улики прогона для чистого судьи.
struct ReplayEvidence {
    min_frames: u64,
    frames: Result<FrameEvidence, String>,
    host: HostLogEvidence,
    page_report: Result<PageReport, SiteError>,
    forbidden_player_lines: Vec<String>,
    /// Выжимки сводок плеера по порядку.
    summaries: Vec<SummaryDigest>,
    /// Хвост журнала страницы (прикладывается при провале).
    page_log: String,
}

/// Повтор записи в одном браузере.
pub fn run(stand: &Stand, browser: BrowserKind, options: &ReplayOptions) -> StepResult {
    let started = Instant::now();
    let subject = options.recording.file_name().map_or_else(
        || "recording".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let verdict = match load_recording(&options.recording) {
        Ok(recording) => {
            stand.site.serve_recording(options.recording.clone());
            let mut verdict = stand.with_environment(|environment| {
                drive(stand, environment, browser, &recording, options)
            });
            if !recording.warnings().is_empty() {
                verdict.detail.push_str(&format!(
                    "\nrecording warnings ({}): {}",
                    recording.warnings().len(),
                    recording.warnings().join("; ")
                ));
            }
            verdict
        }
        Err(message) => Verdict::fail(message),
    };
    StepResult::new("replay", browser, subject, verdict, started.elapsed())
}

/// Читает и разбирает файл; запись без `handoff_began` бесполезна для повтора передачи.
fn load_recording(path: &Path) -> Result<Recording, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let recording = Recording::parse(&bytes)
        .map_err(|error| format!("{} is not a valid recording: {error}", path.display()))?;
    if recording.handoff_began_at_ms().is_none() {
        return Err(format!(
            "{} has no 'handoff_began' event: there is no moment to hand the tab off",
            path.display()
        ));
    }
    Ok(recording)
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    recording: &Recording,
    options: &ReplayOptions,
) -> Result<Verdict, ScenarioError> {
    let handoff_at = millis(recording.handoff_began_at_ms().unwrap_or(0.0));
    let after_handoff = millis(recording.duration_ms()).saturating_sub(handoff_at);
    let query = format!("&tail_ms={TAIL_AFTER_RECORDING_MS}");
    let mut run = HandoffRun::open_with_query(
        stand,
        environment,
        browser_kind,
        REPLAY_PAGE_ID,
        REPLAY_PAGE_ID,
        ExtensionMode::Real,
        &query,
    )?;
    match run.wait_page_marker(HANDOFF_POINT_MARKER, handoff_at + PAGE_START_SLACK) {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(&report, recording.uses_worker_context());
            run.browser.shutdown()?;
            return Ok(verdict);
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!(
                "the replay never reached the recorded hand-off moment ({handoff_at:?} into the recording)\n{log}"
            )));
        }
    }
    // Передача ровно в записанный момент: дальше страница продолжает повтор под тенью.
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        Ok(other) => return fail_and_close(run, format!("hand_off_tab outcome: {other:?}")),
        Err(error) => return fail_and_close(run, format!("hand_off_tab: {error}")),
    }
    let session = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    // Отчёт страницы приходит после конца записи + запас: до него плеер уже играл.
    let page_report = run.wait_final_report(
        after_handoff + Duration::from_millis(TAIL_AFTER_RECORDING_MS) + REPORT_SLACK,
    );
    let frames = match &session {
        Ok(()) => run
            .player_log
            .wait_for_presented_frames(options.min_frames, FRAMES_AFTER_REPORT_TIMEOUT)
            .map_err(|error| {
                format!(
                    "player did not present {} frames: {error}",
                    options.min_frames
                )
            }),
        Err(message) => Err(format!("{message}; frames were not awaited")),
    };
    let summaries: Vec<SummaryDigest> = run
        .player_log
        .snapshot_summary_lines()?
        .iter()
        .filter_map(|line| SummaryDigest::parse(line))
        .collect();
    let page_log = format_browser_log(&run.take_page_log());
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_replay(&ReplayEvidence {
        min_frames: options.min_frames,
        frames,
        host,
        page_report,
        forbidden_player_lines,
        summaries,
        page_log,
    }))
}

fn fail_and_close(run: HandoffRun<'_>, message: String) -> Result<Verdict, ScenarioError> {
    let log = format_browser_log(&run.take_page_log());
    run.browser.shutdown()?;
    Ok(Verdict::fail(format!("{message}\n{log}")))
}

fn millis(value: f64) -> Duration {
    Duration::from_millis(value.max(0.0).round() as u64)
}

/// Страница отчиталась до момента передачи: `unsupported` честен только для записи с воркерами
/// (в Firefox MSE в воркере нет), для записи из одного главного потока это провал повтора.
fn judge_early_report(report: &PageReport, uses_worker_context: bool) -> Verdict {
    let details = |only_failed: bool| {
        report
            .checks
            .iter()
            .filter(|check| !only_failed || !check.ok)
            .map(|check| format!("{} ({})", check.name, check.detail))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match report.status {
        PageStatus::Unsupported if uses_worker_context => Verdict::unsupported(format!(
            "the recording uses worker contexts and this browser cannot replay them: {}",
            details(false)
        )),
        PageStatus::Unsupported => Verdict::fail(format!(
            "the page reported unsupported, but the recording uses only the main thread: {}",
            details(false)
        )),
        _ => Verdict::fail(format!(
            "the page reported {:?} before the hand-off: {}",
            report.status,
            details(true)
        )),
    }
}

/// Чистое решение: повтор дошёл до кадров в плеере, не сломав страницу и плеер.
fn judge_replay(evidence: &ReplayEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    match &evidence.frames {
        Ok(found) if found.position_regressions > 0 => failures.push(format!(
            "playback position went backwards {} time(s): {}",
            found.position_regressions, found.summary_line
        )),
        Ok(_) => {}
        Err(message) => failures.push(message.clone()),
    }
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, REPLAY_PAGE_ID));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let presented = evidence
        .frames
        .as_ref()
        .map_or(0, |found| found.presented_frames);
    let summaries = if evidence.summaries.is_empty() {
        "  (no playback summaries in the player log)".to_owned()
    } else {
        evidence
            .summaries
            .iter()
            .map(|digest| format!("  {digest}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut text = format!(
        "presented_frames={presented} (need {})\nplayer summaries:\n{summaries}",
        evidence.min_frames
    );
    if let Ok(report) = &evidence.page_report {
        let metric = |name: &str| report.metrics.get(name).copied().unwrap_or(f64::NAN);
        text.push_str(&format!(
            "\npage: events_executed={} op_errors={} max_lateness_ms={}",
            metric("events_executed"),
            metric("op_errors"),
            metric("max_lateness_ms")
        ));
    }
    if !failures.is_empty() {
        text.push_str(&format!("\n{}", evidence.page_log));
    }
    Verdict::from_failures(&failures, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{ReportCheck, RunId};

    fn page_report(status: PageStatus, checks: Vec<ReportCheck>) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: REPLAY_PAGE_ID.into(),
            status,
            checks,
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: [("events_executed".to_owned(), 42.0)].into_iter().collect(),
        }
    }

    fn check(name: &str, ok: bool, detail: &str) -> ReportCheck {
        ReportCheck {
            name: name.into(),
            ok,
            detail: detail.into(),
        }
    }

    fn frames(presented_frames: u64, position_regressions: u64) -> FrameEvidence {
        FrameEvidence {
            presented_frames,
            position_regressions,
            summary_line: "Browser playback summary".into(),
        }
    }

    fn healthy_host() -> HostLogEvidence {
        HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        }
    }

    fn evidence() -> ReplayEvidence {
        ReplayEvidence {
            min_frames: 50,
            frames: Ok(frames(260, 0)),
            host: healthy_host(),
            page_report: Ok(page_report(
                PageStatus::Pass,
                vec![check("replay-ops-ok", true, "ok")],
            )),
            forbidden_player_lines: Vec::new(),
            summaries: vec![SummaryDigest {
                kind: "periodic".into(),
                presented_frames: 260,
                position_ms: 10_000,
                state: "playing".into(),
            }],
            page_log: "page log".into(),
        }
    }

    #[test]
    fn healthy_replay_passes_and_prints_the_player_summaries() {
        let verdict = judge_replay(&evidence());
        assert_eq!(verdict.outcome, Outcome::Pass);
        assert!(
            verdict.detail.contains("presented_frames=260 (need 50)"),
            "{}",
            verdict.detail
        );
        assert!(
            verdict
                .detail
                .contains("periodic: presented_frames=260 position_ms=10000 state=playing")
        );
        assert!(verdict.detail.contains("events_executed=42"));
    }

    #[test]
    fn missing_frames_fail_the_replay_and_attach_the_page_log() {
        let mut case = evidence();
        case.frames = Err("player did not present 50 frames: timeout".into());
        let verdict = judge_replay(&case);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("player did not present 50 frames"));
        assert!(
            verdict.detail.contains("page log"),
            "the log explains a failure"
        );
    }

    #[test]
    fn a_failing_page_a_rolled_back_position_or_a_player_error_each_fail_the_replay() {
        let mut broken_page = evidence();
        broken_page.page_report = Ok(page_report(
            PageStatus::Fail,
            vec![check(
                "replay-ops-ok",
                false,
                "#3 append: QuotaExceededError",
            )],
        ));
        let verdict = judge_replay(&broken_page);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("QuotaExceededError"));

        let mut regression = evidence();
        regression.frames = Ok(frames(260, 2));
        assert!(
            judge_replay(&regression)
                .detail
                .contains("went backwards 2 time(s)")
        );

        let mut panicked = evidence();
        panicked.forbidden_player_lines = vec!["thread 'x' panicked".into()];
        assert!(
            judge_replay(&panicked)
                .detail
                .contains("forbidden player line")
        );

        let mut no_grant = evidence();
        no_grant.host = HostLogEvidence::default();
        assert_eq!(judge_replay(&no_grant).outcome, Outcome::Fail);
    }

    #[test]
    fn early_unsupported_is_honest_only_for_recordings_with_workers() {
        let report = page_report(
            PageStatus::Unsupported,
            vec![check(
                "unsupported-reason",
                true,
                "canConstructInDedicatedWorker=false",
            )],
        );
        let honest = judge_early_report(&report, true);
        assert_eq!(honest.outcome, Outcome::Unsupported);
        assert!(
            honest
                .detail
                .contains("canConstructInDedicatedWorker=false")
        );
        // Запись только из главного потока обязана воспроизводиться везде.
        assert_eq!(judge_early_report(&report, false).outcome, Outcome::Fail);
        let failed = page_report(
            PageStatus::Fail,
            vec![check("replay-ops-ok", false, "boom")],
        );
        let verdict = judge_early_report(&failed, true);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("replay-ops-ok (boom)"));
    }

    #[test]
    fn load_recording_rejects_garbage_and_a_recording_without_a_hand_off() {
        let directory = tempfile::tempdir().expect("tempdir");
        let garbage = directory.path().join("garbage.fprec");
        std::fs::write(&garbage, b"not a recording at all").expect("write");
        assert!(
            load_recording(&garbage)
                .expect_err("garbage")
                .contains("bad magic")
        );
        assert!(
            load_recording(&directory.path().join("absent.fprec"))
                .expect_err("absent")
                .contains("cannot read")
        );

        let mut builder = crate::recording::RecordingBuilder::new("chrome", "https://x.test");
        builder.event(
            0.0,
            "main",
            "media_source_created",
            serde_json::json!({ "ms": "ms-1" }),
        );
        let without_handoff = directory.path().join("no-handoff.fprec");
        std::fs::write(&without_handoff, builder.finish().expect("builds")).expect("write");
        assert!(
            load_recording(&without_handoff)
                .expect_err("no hand-off")
                .contains("handoff_began")
        );
    }
}
