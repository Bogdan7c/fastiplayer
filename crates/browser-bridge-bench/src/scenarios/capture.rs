//! `capture` и `smoke`: страница играет, робот «отдаёт вкладку», и плеер показывает кадры,
//! а браузер при этом НЕ декодирует (сессия 09a).
//!
//! Цепочка проверяется по звеньям, чтобы при провале было видно, где она порвалась:
//! страница сыграла и готова → фон принял команду и вернул `started` → посредник выдал
//! пропуск → плеер показал кадры (`presented_frames` в журнале плеера) → если у страницы
//! есть действие под тенью (смена качества, смена MediaSource), плеер показывает кадры и
//! после него → страница под тенью жива, её часы идут, а НАСТОЯЩИЙ счётчик кадров браузера
//! (мимо тени) почти не растёт. Улики — журналы посредника и плеера и отчёт страницы; журнала
//! экстеншена в продукте нет.

use std::time::{Duration, Instant};

use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines, wait_for_session_granted,
};
use super::handoff_run::{ACK_TIMEOUT, ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_catalog::PAGE_SCENARIOS;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameEvidence, FrameGrowth, HostLogEvidence, read_host_evidence};
use crate::site::{PageReport, PageStatus, SiteError};

/// Сколько кадров должен показать плеер: 2 с видео при 25 к/с — «играет», а не «открылся».
pub(super) const MIN_PRESENTED_FRAMES: u64 = 50;
/// Сводки плеера приходят раз в 10 с, плюс запуск плеера и сеанса.
const FRAMES_TIMEOUT: Duration = Duration::from_secs(60);
/// На сколько должны вырасти кадры плеера между двумя сводками ПОСЛЕ действия страницы
/// (интервал сводок 10 с, 25 к/с: целый интервал даёт ~250; берём меньше четверти).
const MIN_FRAMES_GAINED_AFTER_HOOK: u64 = 60;
/// Сколько ждать сводки после действия страницы: сводка-точка отсчёта и ещё одна.
const HOOK_FRAMES_TIMEOUT: Duration = Duration::from_secs(45);
/// Самое короткое окно «под тенью», на котором счётчик браузера что-то доказывает, с.
const MIN_SHADOW_WINDOW_S: f64 = 6.0;
/// Потолок настоящих кадров браузера под тенью, кадров в секунду. В 00c браузер под тенью
/// декодировал 0–116 кадров за 60 с (до ~2 к/с из-за перестановки кадров), обычное
/// воспроизведение роликов стенда — 25 к/с. 5 к/с с запасом над худшим замеренным и в пять
/// раз ниже обычного воспроизведения.
const MAX_REAL_FRAMES_PER_SECOND: f64 = 5.0;
/// Запас кадров на момент перехода: команда вернула `started` чуть раньше, чем тень
/// полностью заняла элемент, и пара кадров браузера успевает проскочить.
const HANDOVER_SLACK_FRAMES: f64 = 10.0;

/// Страница, которую проверяет `capture`, и её действие под тенью (если есть).
#[derive(Debug, Clone, Copy)]
pub(super) struct CapturePage {
    /// Идентификатор страницы стенда.
    pub(super) id: &'static str,
    /// Имя действия страницы по команде робота (`hook:<имя>`), см. `site/pages`.
    pub(super) hook: Option<&'static str>,
}

/// Страница дымка: самая простая MSE-страница, без действий под тенью.
pub(super) const SMOKE_PAGE: CapturePage = CapturePage {
    id: "mse-fmp4",
    hook: None,
};
/// ABR: смена качества в тот же SourceBuffer под тенью, играет дальше.
const ABR_PAGE: CapturePage = CapturePage {
    id: "mse-abr",
    hook: Some("switch-quality"),
};
/// Реклама → фильм: новый MediaSource на том же элементе уже под тенью.
const GENERATION_SWAP_PAGE: CapturePage = CapturePage {
    id: "mse-generation-swap",
    hook: Some("swap-generation"),
};

/// MSE-страницы, которые должны дойти до кадра в рендере плеера (список задания 09a).
pub(super) const CAPTURE_PAGES: &[CapturePage] = &[
    SMOKE_PAGE,
    CapturePage {
        id: "mse-webm",
        hook: None,
    },
    CapturePage {
        id: "mse-av1",
        hook: None,
    },
    CapturePage {
        id: "mse-muxed",
        hook: None,
    },
    // ABR: смена качества в тот же SourceBuffer под тенью, играет дальше.
    CapturePage {
        id: "mse-abr",
        hook: Some("switch-quality"),
    },
    CapturePage {
        id: "mse-stall-reload",
        hook: None,
    },
    // Реклама → фильм: новый MediaSource на том же элементе уже под тенью.
    CapturePage {
        id: "mse-generation-swap",
        hook: Some("swap-generation"),
    },
    CapturePage {
        id: "shadow-dom",
        hook: None,
    },
    CapturePage {
        id: "iframe-cross-origin",
        hook: None,
    },
    CapturePage {
        id: "cdn-guard",
        hook: None,
    },
];

/// Все MSE-страницы одного браузера, каждая в свежем окружении (свой плеер и журналы).
/// Первым идёт контроль линейки: браузер без экстеншена играет сам, и счётчик это видит.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind) -> Vec<StepResult> {
    let mut results: Vec<StepResult> = CONTROL_PAGES
        .iter()
        .map(|page| run_decoder_control(stand, browser, *page))
        .collect();
    results.extend(
        CAPTURE_PAGES
            .iter()
            .map(|page| run_page(stand, browser, "capture", *page)),
    );
    results
}

/// Сколько секунд контрольный прогон даёт браузеру играть самому, пока идёт «окно».
const CONTROL_WINDOW: Duration = Duration::from_secs(8);
/// Не меньше стольких настоящих кадров в секунду должен насчитать счётчик у играющего
/// браузера (ролики стенда — 25 к/с; запас на фоновые вкладки и нагрузку машины).
const MIN_CONTROL_REAL_FRAMES_PER_SECOND: f64 = 12.0;

/// Страницы контроля: простая и обе с действием под тенью (контроль заодно доказывает, что
/// действия страницы — смена качества, смена MediaSource — сами по себе работают).
const CONTROL_PAGES: &[CapturePage] = &[SMOKE_PAGE, ABR_PAGE, GENERATION_SWAP_PAGE];

/// Контроль линейки: тот же сценарий, но БЕЗ экстеншена. Браузер играет сам, поэтому
/// настоящий счётчик обязан насчитать нормальное воспроизведение и приговор
/// «браузер не декодирует» обязан не пройти. Если контроль не проходит, измерение
/// под тенью (`capture`) ничего не доказывает.
fn run_decoder_control(stand: &Stand, browser: BrowserKind, page: CapturePage) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| {
        let run = HandoffRun::open(
            stand,
            environment,
            browser,
            page.id,
            "control",
            ExtensionMode::ControlWithoutExtension,
        )?;
        let readiness = run.wait_page_ready();
        let verdict = match readiness {
            PageReadiness::Ready => {
                run.tell_page("handed-off");
                let hook = page.hook.map(|name| {
                    let command = format!("hook:{name}");
                    run.tell_page(&command);
                    run.wait_ack(&command, ACK_TIMEOUT)
                });
                std::thread::sleep(CONTROL_WINDOW);
                let report = run.finish_page();
                let verdict = judge_decoder_control(&report, page.id);
                match hook {
                    Some(Err(message)) => Verdict::fail(format!(
                        "the page did not finish its action: {message}\n{}",
                        verdict.detail
                    )),
                    _ => verdict,
                }
            }
            PageReadiness::ReportedEarly(_) | PageReadiness::TimedOut => {
                Verdict::fail("the control page never became ready")
            }
        };
        run.browser.shutdown()?;
        Ok(verdict)
    });
    StepResult::new(
        "capture",
        browser,
        format!("(control) {}", page.id),
        verdict,
        started.elapsed(),
    )
}

/// Чистый приговор контроля: честный браузер насчитал нормальное воспроизведение, и
/// пороговая проверка «не декодирует» его бы не пропустила.
fn judge_decoder_control(report: &Result<PageReport, SiteError>, page_id: &str) -> Verdict {
    let mut failures = page_failures(report, page_id);
    let report = match report {
        Ok(report) => report,
        Err(error) => return Verdict::fail(format!("control page: {error}")),
    };
    let metric = |name: &str| report.metrics.get(name).copied();
    let (Some(window_s), Some(real_frames)) =
        (metric("shadow_window_s"), metric("real_frames_in_window"))
    else {
        return Verdict::fail("the control page report has no real-frame metrics");
    };
    let needed = window_s * MIN_CONTROL_REAL_FRAMES_PER_SECOND;
    if real_frames < needed {
        failures.push(format!(
            "the real counter saw only {real_frames} frames in {window_s:.1} s of ordinary playback (need {needed:.0}): the ruler is broken"
        ));
    }
    if browser_does_not_decode(report).is_empty() {
        failures.push(
            "the 'browser does not decode' check accepted a browser that plays by itself"
                .to_owned(),
        );
    }
    Verdict::from_failures(
        &failures,
        format!("control: {real_frames} real frames in {window_s:.1} s without the extension"),
    )
}

/// Одна страница; `command` — имя шага в таблице (`capture` или `smoke`).
pub(super) fn run_page(
    stand: &Stand,
    browser: BrowserKind,
    command: &'static str,
    page: CapturePage,
) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| drive(stand, environment, browser, page));
    StepResult::new(command, browser, page.id, verdict, started.elapsed())
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page: CapturePage,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        page.id,
        page.id,
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(page.id, browser_kind, &report);
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

    // Критическая развилка: если фон не принял передачу, дальше ждать нечего.
    let outcome = run.hand_off();
    let hand_off = match outcome {
        Ok(HandOffOutcome::Started) => Ok(()),
        Ok(other) => Err(format!("hand_off_tab outcome: {other:?}")),
        Err(error) => Err(format!("hand_off_tab: {error}")),
    };
    if let Err(message) = &hand_off {
        let log = format_browser_log(&run.take_page_log());
        run.browser.shutdown()?;
        return Ok(Verdict::fail(format!("{message}\n{log}")));
    }
    run.tell_page("handed-off");

    // Сначала пропуск от НАШЕГО посредника: без него «кадры есть» ничего бы не доказывало.
    let session = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    let frames = match &session {
        Ok(()) => run
            .player_log
            .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
            .map_err(|error| format!("player did not present frames: {error}")),
        Err(message) => Err(format!("{message}; frames were not awaited")),
    };
    let hook = page.hook.map(|name| run_hook(&mut run, name));

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let player_log = &mut run.player_log;
    let forbidden = player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    // Журнал посредника лежит в stderr браузера: читаем после мягкого закрытия.
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_capture(&CaptureEvidence {
        page_id: page.id,
        frames,
        hook,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
    }))
}

/// Действие страницы под тенью: команда, подтверждение и рост кадров плеера после него.
fn run_hook(run: &mut HandoffRun<'_>, name: &str) -> HookEvidence {
    let command = format!("hook:{name}");
    // Точка отсчёта кадров — сводки ПОСЛЕ этого места журнала (старые уже учтены).
    let skipped = run.player_log.skip_history();
    run.tell_page(&command);
    let acknowledged = run.wait_ack(&command, ACK_TIMEOUT);
    let growth = match (skipped, &acknowledged) {
        (Err(error), _) => Err(format!("cannot read the player log: {error}")),
        (Ok(()), Err(message)) => Err(format!("hook was not acknowledged: {message}")),
        (Ok(()), Ok(())) => run
            .player_log
            .wait_for_frame_growth(MIN_FRAMES_GAINED_AFTER_HOOK, HOOK_FRAMES_TIMEOUT)
            .map_err(|error| format!("player frames did not keep growing after the hook: {error}")),
    };
    HookEvidence {
        name: name.to_owned(),
        growth,
    }
}

/// Результат действия страницы под тенью.
struct HookEvidence {
    name: String,
    /// Рост показанных кадров плеера между двумя сводками после действия.
    growth: Result<FrameGrowth, String>,
}

/// Всё, что собрал прогон, без привязки к браузеру и файлам: вход чистого вердикта.
struct CaptureEvidence {
    page_id: &'static str,
    /// Сводка показанных кадров или причина, почему её нет.
    frames: Result<FrameEvidence, String>,
    /// Действие под тенью, если у страницы оно есть.
    hook: Option<HookEvidence>,
    /// Отметки посредника.
    host: HostLogEvidence,
    /// Отчёт страницы или ошибка ожидания.
    page_report: Result<PageReport, SiteError>,
    /// Запрещённые строки журнала плеера (с шумом).
    forbidden_player_lines: Vec<String>,
    /// Хвост журнала страницы (прикладывается при провале).
    page_log: String,
}

/// Страница отчиталась до передачи: `unsupported` честен только там, где таблица его разрешает.
fn judge_early_report(page_id: &str, browser: BrowserKind, report: &PageReport) -> Verdict {
    let allowed = PAGE_SCENARIOS
        .iter()
        .find(|scenario| scenario.id == page_id)
        .is_some_and(|scenario| scenario.may_be_unsupported_in(browser));
    let failed: Vec<String> = report
        .checks
        .iter()
        .filter(|check| !check.ok)
        .map(|check| format!("{} ({})", check.name, check.detail))
        .collect();
    match report.status {
        PageStatus::Unsupported if allowed => Verdict::unsupported("the page is unsupported here"),
        PageStatus::Unsupported => Verdict::fail(format!(
            "unsupported is not allowed for {page_id} in {browser:?}"
        )),
        _ => Verdict::fail(format!(
            "the page reported {:?} before the hand-off: {}",
            report.status,
            failed.join(", ")
        )),
    }
}

/// Сколько настоящих кадров браузера допустимо под тенью за окно `window_s` секунд.
fn max_real_frames(window_s: f64) -> f64 {
    window_s * MAX_REAL_FRAMES_PER_SECOND + HANDOVER_SLACK_FRAMES
}

/// Проверки «браузер не декодирует» по метрикам страницы.
fn browser_does_not_decode(report: &PageReport) -> Vec<String> {
    let metric = |name: &str| report.metrics.get(name).copied();
    let (Some(available), Some(window_s), Some(real_frames)) = (
        metric("real_probe_available"),
        metric("shadow_window_s"),
        metric("real_frames_in_window"),
    ) else {
        return vec!["the page report has no real-frame metrics".to_owned()];
    };
    if available < 1.0 {
        return vec!["the page could not read the real frame counter".to_owned()];
    }
    if window_s < MIN_SHADOW_WINDOW_S {
        return vec![format!(
            "the shadow window was {window_s:.1} s, too short to judge the browser decoder (need {MIN_SHADOW_WINDOW_S} s)"
        )];
    }
    let allowed = max_real_frames(window_s);
    if real_frames > allowed {
        return vec![format!(
            "the browser decoded {real_frames} frames in {window_s:.1} s under the shadow (allowed {allowed:.0}): it still plays by itself"
        )];
    }
    Vec::new()
}

/// Чистое решение «страница дошла до кадра в рендере без декодирования браузером».
fn judge_capture(evidence: &CaptureEvidence) -> Verdict {
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
    if let Some(hook) = &evidence.hook
        && let Err(message) = &hook.growth
    {
        failures.push(format!("after '{}': {message}", hook.name));
    }
    failures.extend(page_failures(&evidence.page_report, evidence.page_id));
    if let Ok(report) = &evidence.page_report {
        failures.extend(browser_does_not_decode(report));
    }
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );

    let presented = evidence
        .frames
        .as_ref()
        .map_or(0, |found| found.presented_frames);
    let gained = evidence
        .hook
        .as_ref()
        .and_then(|hook| hook.growth.as_ref().ok())
        .map(FrameGrowth::frames_gained);
    let real_frames = evidence
        .page_report
        .as_ref()
        .ok()
        .and_then(|report| report.metrics.get("real_frames_in_window").copied());
    let summary = format!(
        "presented_frames={presented} frames_gained_after_hook={gained:?} browser_real_frames={real_frames:?}; host: caller_accepted={} hello_accepted={} session_granted={}",
        evidence.host.caller_accepted, evidence.host.hello_accepted, evidence.host.session_granted,
    );
    // При провале журнал страницы — главная улика.
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
    use crate::site::{ReportCheck, RunId};

    fn report_with(metrics: &[(&str, f64)]) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "mse-fmp4".into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: metrics
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect(),
        }
    }

    fn healthy_metrics() -> Vec<(&'static str, f64)> {
        vec![
            ("real_probe_available", 1.0),
            ("shadow_window_s", 12.0),
            ("real_frames_in_window", 8.0),
        ]
    }

    fn full_host() -> HostLogEvidence {
        HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        }
    }

    fn healthy_evidence() -> CaptureEvidence {
        CaptureEvidence {
            page_id: "mse-fmp4",
            frames: Ok(FrameEvidence {
                presented_frames: 240,
                position_regressions: 0,
                summary_line: "summary".into(),
            }),
            hook: None,
            host: full_host(),
            page_report: Ok(report_with(&healthy_metrics())),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".into(),
        }
    }

    fn growth(before: u64, after: u64) -> FrameGrowth {
        let evidence = |frames| FrameEvidence {
            presented_frames: frames,
            position_regressions: 0,
            summary_line: String::new(),
        };
        FrameGrowth {
            before: evidence(before),
            after: evidence(after),
        }
    }

    #[test]
    fn healthy_capture_passes_and_reports_the_numbers() {
        let verdict = judge_capture(&healthy_evidence());
        assert_eq!(verdict.outcome, Outcome::Pass);
        assert!(verdict.detail.contains("presented_frames=240"));
        assert!(verdict.detail.contains("browser_real_frames=Some(8.0)"));
    }

    #[test]
    fn a_browser_that_keeps_decoding_under_the_shadow_fails() {
        let mut evidence = healthy_evidence();
        evidence.page_report = Ok(report_with(&[
            ("real_probe_available", 1.0),
            ("shadow_window_s", 12.0),
            ("real_frames_in_window", 290.0),
        ]));
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("still plays by itself"));
    }

    #[test]
    fn the_decode_threshold_is_exact_at_the_edge() {
        let window_s = 10.0;
        let edge = max_real_frames(window_s);
        let metrics = |frames: f64| {
            report_with(&[
                ("real_probe_available", 1.0),
                ("shadow_window_s", window_s),
                ("real_frames_in_window", frames),
            ])
        };
        assert!(browser_does_not_decode(&metrics(edge)).is_empty());
        assert_eq!(browser_does_not_decode(&metrics(edge + 1.0)).len(), 1);
    }

    #[test]
    fn missing_metrics_unavailable_probe_and_short_window_are_failures_not_passes() {
        assert_eq!(browser_does_not_decode(&report_with(&[])).len(), 1);
        let unavailable = report_with(&[
            ("real_probe_available", 0.0),
            ("shadow_window_s", 12.0),
            ("real_frames_in_window", 0.0),
        ]);
        assert!(browser_does_not_decode(&unavailable)[0].contains("could not read"));
        let short = report_with(&[
            ("real_probe_available", 1.0),
            ("shadow_window_s", 2.0),
            ("real_frames_in_window", 0.0),
        ]);
        assert!(browser_does_not_decode(&short)[0].contains("too short"));
    }

    #[test]
    fn no_frames_in_the_player_fails_with_the_reason() {
        let mut evidence = healthy_evidence();
        evidence.frames = Err("player did not present frames: timeout".into());
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("did not present frames"));
        assert!(
            verdict.detail.contains("page log"),
            "the failure carries the page log"
        );
    }

    #[test]
    fn position_regressions_fail() {
        let mut evidence = healthy_evidence();
        evidence.frames = Ok(FrameEvidence {
            presented_frames: 240,
            position_regressions: 2,
            summary_line: "summary".into(),
        });
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("went backwards 2 time(s)"));
    }

    #[test]
    fn missing_host_marks_fail() {
        let mut evidence = healthy_evidence();
        evidence.host = HostLogEvidence::default();
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("host did not grant the session"));
    }

    #[test]
    fn a_failed_page_report_fails_even_when_the_player_shows_frames() {
        let mut evidence = healthy_evidence();
        let mut report = report_with(&healthy_metrics());
        report.status = PageStatus::Fail;
        report.checks = vec![ReportCheck {
            name: "page-clock-advances".into(),
            ok: false,
            detail: "currentTime 3 -> 3".into(),
        }];
        evidence.page_report = Ok(report);
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("page-clock-advances (currentTime 3 -> 3)")
        );
    }

    #[test]
    fn hook_needs_the_player_frames_to_keep_growing() {
        let mut evidence = healthy_evidence();
        evidence.hook = Some(HookEvidence {
            name: "switch-quality".into(),
            growth: Ok(growth(300, 560)),
        });
        assert_eq!(judge_capture(&evidence).outcome, Outcome::Pass);
        evidence.hook = Some(HookEvidence {
            name: "switch-quality".into(),
            growth: Err("player frames did not keep growing".into()),
        });
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("after 'switch-quality'"));
    }

    #[test]
    fn forbidden_player_lines_fail_but_the_known_portal_noise_does_not() {
        let mut evidence = healthy_evidence();
        evidence.forbidden_player_lines =
            vec!["T ERROR XDG Settings Portal did not return response in time".into()];
        assert_eq!(judge_capture(&evidence).outcome, Outcome::Pass);
        evidence
            .forbidden_player_lines
            .push("T ERROR decoder exploded".into());
        assert_eq!(judge_capture(&evidence).outcome, Outcome::Fail);
    }

    #[test]
    fn early_unsupported_is_honest_only_where_the_table_allows_it() {
        let mut report = report_with(&[]);
        report.status = PageStatus::Unsupported;
        report.scenario = "mse-av1".into();
        // В таблице страниц `unsupported` не разрешён нигде для mse-av1 → провал.
        let verdict = judge_early_report("mse-av1", BrowserKind::Firefox, &report);
        assert_eq!(verdict.outcome, Outcome::Fail);
        // Для mse-worker в Firefox таблица его разрешает.
        let verdict = judge_early_report("mse-worker", BrowserKind::Firefox, &report);
        assert_eq!(verdict.outcome, Outcome::Unsupported);
        report.status = PageStatus::Fail;
        assert_eq!(
            judge_early_report("mse-fmp4", BrowserKind::Chromium, &report).outcome,
            Outcome::Fail
        );
    }

    fn control_report(window_s: f64, real_frames: f64) -> Result<PageReport, SiteError> {
        Ok(report_with(&[
            ("real_probe_available", 1.0),
            ("shadow_window_s", window_s),
            ("real_frames_in_window", real_frames),
        ]))
    }

    #[test]
    fn control_passes_only_when_the_ruler_sees_ordinary_playback() {
        // 8 с обычной игры по 25 к/с: линейка видит ~200 кадров, проверка «не декодирует» их отвергает.
        assert_eq!(
            judge_decoder_control(&control_report(8.0, 200.0), "mse-fmp4").outcome,
            Outcome::Pass
        );
        // Линейка, которая молчит у играющего браузера, сломана.
        let broken = judge_decoder_control(&control_report(8.0, 2.0), "mse-fmp4");
        assert_eq!(broken.outcome, Outcome::Fail);
        assert!(broken.detail.contains("the ruler is broken"));
        // Нет метрик — это не успех.
        assert_eq!(
            judge_decoder_control(&Ok(report_with(&[])), "mse-fmp4").outcome,
            Outcome::Fail
        );
    }

    #[test]
    fn capture_table_lists_only_known_pages_with_distinct_ids() {
        let mut seen = std::collections::HashSet::new();
        for page in CAPTURE_PAGES {
            assert!(
                PAGE_SCENARIOS.iter().any(|known| known.id == page.id),
                "{} is not a stand page",
                page.id
            );
            assert!(seen.insert(page.id), "{} listed twice", page.id);
        }
        assert_eq!(CAPTURE_PAGES.len(), 10);
    }
}
