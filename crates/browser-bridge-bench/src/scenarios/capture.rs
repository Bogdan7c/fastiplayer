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

pub(super) mod file_evidence;
mod judge;
mod refusals;

use judge::{CaptureEvidence, HookEvidence, browser_does_not_decode, judge_capture};

use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, page_failures,
    wait_for_session_granted,
};
use super::handoff_run::{ACK_TIMEOUT, ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_catalog::PAGE_SCENARIOS;
use super::page_filter::PageFilter;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::read_host_evidence;
use crate::site::{PageReport, PageStatus, SiteError};

/// Сколько кадров должен показать плеер: 2 с видео при 25 к/с — «играет», а не «открылся».
pub(super) const MIN_PRESENTED_FRAMES: u64 = 50;
/// Сводки плеера приходят раз в 10 с, плюс запуск плеера и сеанса.
pub(super) const FRAMES_TIMEOUT: Duration = Duration::from_secs(60);
/// На сколько должны вырасти кадры плеера между двумя сводками ПОСЛЕ действия страницы
/// (интервал сводок 10 с, 25 к/с: целый интервал даёт ~250; берём меньше четверти).
const MIN_FRAMES_GAINED_AFTER_HOOK: u64 = 60;
/// Сколько ждать сводки после действия страницы: сводка-точка отсчёта и ещё одна.
const HOOK_FRAMES_TIMEOUT: Duration = Duration::from_secs(45);

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
    // Д-1: тот же обмен, но старый источник снят иначе (так делает YouTube перед рекламой).
    CapturePage {
        id: "mse-generation-swap-srcobject-null",
        hook: Some("swap-generation"),
    },
    CapturePage {
        id: "mse-generation-swap-reload",
        hook: Some("swap-generation"),
    },
    // Д-4: данные нового поколения приходят через 5 с; пока их нет, часы стоят у начала.
    CapturePage {
        id: "mse-generation-swap-delayed",
        hook: Some("swap-generation"),
    },
    // Д-9: страница кэшировала геттер currentTime и play ДО передачи и зовёт их под тенью.
    CapturePage {
        id: "cached-descriptors",
        hook: Some("call-cached-api"),
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
    // 09b: MSE в модульном Dedicated Worker (Chromium; Firefox честно `unsupported`), под тенью —
    // новый MediaSource в воркере (`srcObject = null`, новый handle) → новое поколение в плеере.
    CapturePage {
        id: "mse-worker",
        hook: Some("swap-generation"),
    },
    // 09b: то же в классическом воркере, как у Twitch.
    CapturePage {
        id: "mse-worker-classic",
        hook: None,
    },
    // 09b: `<video src>` — фон качает файл по `fetch_range`, источник элемента снят.
    CapturePage {
        id: "video-src",
        hook: None,
    },
    // 09b: тот же файл за входом на CDN — фон несёт куку сайта.
    CapturePage {
        id: "video-src-cdn",
        hook: None,
    },
];

/// Все MSE-страницы одного браузера, каждая в свежем окружении (свой плеер и журналы).
/// Первым идёт контроль линейки: браузер без экстеншена играет сам, и счётчик это видит.
/// С фильтром `--page` идут только выбранные страницы (и их контроли, если выбраны).
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let mut results: Vec<StepResult> = CONTROL_PAGES
        .iter()
        .filter(|page| filter.admits(page.id))
        .map(|page| run_decoder_control(stand, browser, *page))
        .collect();
    results.extend(
        CAPTURE_PAGES
            .iter()
            .filter(|page| filter.admits(page.id))
            .map(|page| run_page(stand, browser, "capture", *page)),
    );
    // 09b: формы, которые экстеншен честно отказывается передавать (видео остаётся у браузера).
    results.extend(
        refusals::REFUSAL_PAGES
            .iter()
            .filter(|page| filter.admits(page.id))
            .map(|page| refusals::run_refusal(stand, browser, *page)),
    );
    results
}

/// Идентификаторы страниц `capture` (контрольные страницы и честные отказы входят в тот же список).
pub(super) fn capture_page_ids() -> Vec<&'static str> {
    CAPTURE_PAGES
        .iter()
        .map(|page| page.id)
        .chain(refusals::REFUSAL_PAGES.iter().map(|page| page.id))
        .collect()
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
    // Для `<video src>`: с этого мига файл должен качать только фон (сервер это видит).
    let handed_off_at = Instant::now();
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
    let file = file_evidence::file_page(page.id).map(|file_page| {
        let requests =
            file_evidence::sort_requests(file_page, &stand.site.media_requests(), handed_off_at);
        (
            file_evidence::file_failures(file_page, &requests),
            file_evidence::file_summary(&requests),
        )
    });
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
        file,
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

/// Страница отчиталась до передачи: `unsupported` честен только там, где таблица его разрешает.
pub(super) fn judge_early_report(
    page_id: &str,
    browser: BrowserKind,
    report: &PageReport,
) -> Verdict {
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

#[cfg(test)]
mod tests {
    use super::judge::tests::report_with;
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::PageStatus;

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
        assert_eq!(CAPTURE_PAGES.len(), 18);
    }
}
