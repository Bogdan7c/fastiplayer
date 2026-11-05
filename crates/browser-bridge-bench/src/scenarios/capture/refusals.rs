//! Честные отказы `capture`: страница играет видео, которое экстеншен не передаёт, и ожидается
//! не кадр в плеере, а наоборот:
//! - сессия 09b: форма, которую экстеншен пока не умеет (решение 185: m3u8 прямо в `src`) —
//!   `not_supported_yet{form}`;
//! - сессия 09c: видео под DRM (`drm-clearkey`) — `protected` (инвариант 4); на странице только
//!   баннер и превью (`banner-autoplay`) — `no_video`;
//! - в любом случае сеанс с плеером даже не открывается;
//! - видео остаётся у браузера: страница играет сама, настоящий счётчик кадров растёт.
//!
//! Если браузер сам такую форму не играет (Firefox на Linux и HLS), страница честно
//! отчитывается `unsupported` до передачи — это разрешено таблицей страниц.

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{format_browser_log, page_failures};
use super::super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::super::outcome::{StepResult, Verdict};
use super::judge_early_report;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{HostLogEvidence, read_host_evidence};
use crate::site::{PageReport, SiteError};

/// Какой отказ ждём от фона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::scenarios) enum ExpectedRefusal {
    /// `not_supported_yet{form}` (09b).
    NotSupportedYet { form: &'static str },
    /// `protected`: лучшее видео под DRM (09c).
    Protected,
    /// `no_video`: передавать нечего, баннеры и превью не в счёт (09c).
    NoVideo,
}

impl ExpectedRefusal {
    /// Совпадает ли исход фона с ожидаемым отказом.
    fn matches(self, outcome: &HandOffOutcome) -> bool {
        match (self, outcome) {
            (Self::NotSupportedYet { form }, HandOffOutcome::NotSupportedYet { form: actual }) => {
                form == actual
            }
            (Self::Protected, HandOffOutcome::Protected) => true,
            (Self::NoVideo, HandOffOutcome::NoVideo) => true,
            _ => false,
        }
    }
}

/// Страница, которую экстеншен честно отказывается передавать.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios) struct RefusalPage {
    /// Идентификатор страницы стенда.
    pub id: &'static str,
    /// Какой отказ ждём.
    pub expected: ExpectedRefusal,
}

/// Страницы честного отказа.
pub(in crate::scenarios) const REFUSAL_PAGES: &[RefusalPage] = &[
    RefusalPage {
        id: "m3u8-in-src",
        expected: ExpectedRefusal::NotSupportedYet { form: "native_hls" },
    },
    // DRM (ClearKey): ключи и `encrypted` на элементе; браузер играет сам, сеанса нет.
    RefusalPage {
        id: "drm-clearkey",
        expected: ExpectedRefusal::Protected,
    },
    // Только баннер и превью по наведению: главного видео на странице нет.
    RefusalPage {
        id: "banner-autoplay",
        expected: ExpectedRefusal::NoVideo,
    },
];

/// Сколько страница играет сама после отказа, прежде чем отчитаться (ролик HLS стенда — 12 с,
/// до готовности страница уже сыграла около двух; 5 с оставляют запас до конца ролика).
pub(in crate::scenarios) const REFUSAL_WINDOW: Duration = Duration::from_secs(5);
/// Сколько настоящих кадров в секунду должен насчитать браузер, который играет сам
/// (ролики стенда — 25 к/с; как у контроля линейки `capture`).
const MIN_REAL_FRAMES_PER_SECOND: f64 = 12.0;

pub(in crate::scenarios) fn run_refusal(
    stand: &Stand,
    browser: BrowserKind,
    page: RefusalPage,
) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| drive(stand, environment, browser, page));
    StepResult::new("capture", browser, page.id, verdict, started.elapsed())
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page: RefusalPage,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser,
        page.id,
        page.id,
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(page.id, browser, &report);
            run.browser.shutdown()?;
            return Ok(verdict);
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!(
                "the page never became ready for the hand-off\n{log}"
            )));
        }
    }
    let outcome = run.hand_off().map_err(|error| error.to_string());
    // Отметка окна «браузер играет сам» — сразу после отказа.
    run.tell_page("handed-off");
    std::thread::sleep(REFUSAL_WINDOW);
    let report = run.finish_page();
    let page_log = format_browser_log(&run.take_page_log());
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_refusal(&RefusalEvidence {
        page,
        outcome,
        host,
        report,
        page_log,
    }))
}

/// Метрики страницы: длина окна наблюдения, с, и сколько настоящих кадров браузер насчитал за него.
pub(in crate::scenarios) fn browser_real_frame_metrics(
    report: &Result<PageReport, SiteError>,
) -> (Option<f64>, Option<f64>) {
    let metric = |name: &str| {
        report
            .as_ref()
            .ok()
            .and_then(|report| report.metrics.get(name).copied())
    };
    (metric("shadow_window_s"), metric("real_frames_in_window"))
}

/// После отказа видео осталось у страницы: настоящий счётчик насчитал обычное воспроизведение.
pub(in crate::scenarios) fn browser_keeps_playing_failures(
    window_s: Option<f64>,
    real_frames: Option<f64>,
) -> Vec<String> {
    match (window_s, real_frames) {
        (Some(window_s), Some(real_frames)) => {
            let needed = window_s * MIN_REAL_FRAMES_PER_SECOND;
            if real_frames < needed {
                vec![format!(
                    "after the refusal the browser decoded only {real_frames} frames in {window_s:.1} s (need {needed:.0}): the video did not stay with the page"
                )]
            } else {
                Vec::new()
            }
        }
        _ => vec!["the page report has no real-frame metrics".to_owned()],
    }
}

/// Вход чистого вердикта.
struct RefusalEvidence {
    page: RefusalPage,
    outcome: Result<HandOffOutcome, String>,
    host: HostLogEvidence,
    report: Result<PageReport, SiteError>,
    page_log: String,
}

fn judge_refusal(evidence: &RefusalEvidence) -> Verdict {
    let mut failures = Vec::new();
    match &evidence.outcome {
        Ok(outcome) if evidence.page.expected.matches(outcome) => {}
        Ok(other) => failures.push(format!(
            "hand_off_tab outcome {other:?}, expected {:?}",
            evidence.page.expected
        )),
        Err(error) => failures.push(format!("hand_off_tab: {error}")),
    }
    if evidence.host.session_granted {
        failures.push("the player granted a session for a video the extension refuses".to_owned());
    }
    failures.extend(page_failures(&evidence.report, evidence.page.id));
    let (window_s, real_frames) = browser_real_frame_metrics(&evidence.report);
    failures.extend(browser_keeps_playing_failures(window_s, real_frames));
    let summary = format!(
        "outcome={:?}; browser_real_frames={real_frames:?} in {window_s:?} s; session_granted={}",
        evidence.outcome, evidence.host.session_granted
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
    use crate::site::{PageStatus, RunId};

    fn report(real_frames: f64) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "m3u8-in-src".into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: [
                ("shadow_window_s".to_owned(), 5.0),
                ("real_frames_in_window".to_owned(), real_frames),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn evidence(outcome: HandOffOutcome, granted: bool, real_frames: f64) -> RefusalEvidence {
        evidence_for(REFUSAL_PAGES[0], outcome, granted, real_frames)
    }

    fn evidence_for(
        page: RefusalPage,
        outcome: HandOffOutcome,
        granted: bool,
        real_frames: f64,
    ) -> RefusalEvidence {
        RefusalEvidence {
            page,
            outcome: Ok(outcome),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: granted,
                session_grants: u32::from(granted),
            },
            report: Ok(PageReport {
                scenario: page.id.into(),
                ..report(real_frames)
            }),
            page_log: String::new(),
        }
    }

    #[test]
    fn honest_refusal_and_the_browser_keeps_playing_passes() {
        let verdict = judge_refusal(&evidence(
            HandOffOutcome::NotSupportedYet {
                form: "native_hls".into(),
            },
            false,
            120.0,
        ));
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    }

    #[test]
    fn started_session_or_frozen_browser_fails() {
        for (outcome, granted, frames) in [
            (HandOffOutcome::Started, true, 120.0),
            (
                HandOffOutcome::NotSupportedYet {
                    form: "blob_file".into(),
                },
                false,
                120.0,
            ),
            (
                HandOffOutcome::NotSupportedYet {
                    form: "native_hls".into(),
                },
                true,
                120.0,
            ),
            (
                HandOffOutcome::NotSupportedYet {
                    form: "native_hls".into(),
                },
                false,
                3.0,
            ),
        ] {
            let verdict = judge_refusal(&evidence(outcome.clone(), granted, frames));
            assert_eq!(
                verdict.outcome,
                Outcome::Fail,
                "{outcome:?} granted={granted} frames={frames}"
            );
        }
    }

    fn page(id: &str) -> RefusalPage {
        *REFUSAL_PAGES
            .iter()
            .find(|page| page.id == id)
            .expect("a known refusal page")
    }

    #[test]
    fn drm_must_be_refused_as_protected_and_banners_as_no_video() {
        for (id, honest) in [
            ("drm-clearkey", HandOffOutcome::Protected),
            ("banner-autoplay", HandOffOutcome::NoVideo),
        ] {
            let ok = judge_refusal(&evidence_for(page(id), honest.clone(), false, 120.0));
            assert_eq!(ok.outcome, Outcome::Pass, "{id}: {}", ok.detail);
            // Тот же исход у чужой страницы, сеанс, замёрзший браузер и «началась передача» — провалы.
            for (outcome, granted, frames) in [
                (HandOffOutcome::Started, true, 120.0),
                (HandOffOutcome::Started, false, 120.0),
                (honest.clone(), true, 120.0),
                (honest.clone(), false, 2.0),
                (HandOffOutcome::AdPlaying, false, 120.0),
            ] {
                let verdict =
                    judge_refusal(&evidence_for(page(id), outcome.clone(), granted, frames));
                assert_eq!(
                    verdict.outcome,
                    Outcome::Fail,
                    "{id}: {outcome:?} granted={granted} frames={frames}"
                );
            }
        }
        // Баннер — не DRM: исход `protected` у баннера не тот, что нужен.
        let swapped = judge_refusal(&evidence_for(
            page("banner-autoplay"),
            HandOffOutcome::Protected,
            false,
            120.0,
        ));
        assert_eq!(swapped.outcome, Outcome::Fail);
    }
}
