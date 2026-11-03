//! `pages`: каждая страница стенда в ЧИСТОМ браузере (без экстеншенов стенда).
//!
//! Доказывает, что сами страницы честны: видео в них реально играет силами браузера и их
//! собственные проверки проходят. Если страница плохая, дымок с перехватчиком ничего не
//! докажет — поэтому `pages` идёт первым.

use std::path::Path;
use std::time::{Duration, Instant};

use super::context::{ScenarioError, Stand};
use super::outcome::{Outcome, StepResult, Verdict};
use super::page_catalog::{PAGE_SCENARIOS, PageScenario};
use super::page_filter::PageFilter;
use crate::browsers::{BrowserKind, BrowserUnderTest};
use crate::site::{PageReport, PageStatus, RunId};

/// Сколько ждать отчёт одной страницы. Самые долгие страницы (перемотка, сериал) играют
/// десятки секунд; больше минуты — значит страница зависла.
const PAGE_REPORT_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn run(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let mut results = Vec::new();
    let started = Instant::now();
    // Окружение и браузер общие для всех страниц браузера: запуск kwin и браузера дорогой.
    let session_verdict = stand.with_environment(|environment| {
        let mut browser_session = stand.launch_browser(browser, environment, Vec::new())?;
        for scenario in PAGE_SCENARIOS
            .iter()
            .filter(|scenario| filter.admits(scenario.id))
        {
            results.push(check_page(
                stand,
                browser_session.as_mut(),
                browser,
                scenario,
            ));
        }
        // Провал страницы — повод сохранить журналы браузера: без `firefox.stderr.log` причину
        // не найти. Раньше `pages` удалял каталог даже при провалах, и каскад «0 кадров» в
        // Firefox (сессия 09b) пришлось ловить повторами вслепую.
        if mark_first_failure_with_kept_directory(&mut results, environment.work_dir()) {
            environment.keep_work_dir();
        }
        browser_session.shutdown()?;
        Ok(Verdict::pass(String::new()))
    });
    // Ошибка запуска/закрытия не должна потеряться, даже если страницы проверились.
    if session_verdict.outcome == Outcome::Fail {
        results.push(StepResult::new(
            "pages",
            browser,
            "(browser session)",
            session_verdict,
            started.elapsed(),
        ));
    }
    results
}

/// Открывает одну страницу, ждёт отчёт, закрывает вкладку.
fn check_page(
    stand: &Stand,
    browser_session: &mut dyn BrowserUnderTest,
    browser: BrowserKind,
    scenario: &PageScenario,
) -> StepResult {
    let started = Instant::now();
    let verdict = match drive_page(stand, browser_session, browser, scenario) {
        Ok(verdict) => verdict,
        Err(error) => Verdict::fail(error.to_string()),
    };
    StepResult::new("pages", browser, scenario.id, verdict, started.elapsed())
}

fn drive_page(
    stand: &Stand,
    browser_session: &mut dyn BrowserUnderTest,
    browser: BrowserKind,
    scenario: &PageScenario,
) -> Result<Verdict, ScenarioError> {
    let run = RunId::new_unique(scenario.id);
    let url = page_url(
        &stand.site.main_origin(),
        &stand.site.other_origin(),
        scenario.id,
        &run,
    );
    let tab = browser_session.open_tab(&url)?;
    let report = stand.site.wait_report(&run, PAGE_REPORT_TIMEOUT);
    // Вкладку закрываем при любом исходе, чтобы зависшая страница не мешала следующим.
    let close_result = browser_session.close_tab(tab);
    let mut verdict = judge_report(scenario, browser, report.map_err(ScenarioError::from));
    if let Err(error) = close_result {
        verdict
            .detail
            .push_str(&format!("\n(could not close the tab: {error})"));
    }
    Ok(verdict)
}

/// Дописывает к ПЕРВОЙ провалившейся странице, где лежит сохранённый каталог прогона.
///
/// Возвращает `true`, если провал есть (тогда вызывающий обязан сохранить каталог). Путь
/// пишем только к первому провалу: все страницы делят один браузер, и следующие провалы
/// часто лишь следствие первого — разбор начинается с него.
fn mark_first_failure_with_kept_directory(
    results: &mut [StepResult],
    run_directory: &Path,
) -> bool {
    let Some(first_failure) = results
        .iter_mut()
        .find(|result| result.outcome == Outcome::Fail)
    else {
        return false;
    };
    first_failure.detail.push_str(&format!(
        "\nrun directory kept for inspection: {}",
        run_directory.display()
    ));
    true
}

/// Адрес страницы: главный origin принимает отчёты, чужой нужен страницам с iframe.
pub(super) fn page_url(main_origin: &str, other_origin: &str, id: &str, run: &RunId) -> String {
    format!(
        "{main_origin}/site/pages/{id}.html?run={run}&report_origin={main_origin}&other_origin={other_origin}"
    )
}

/// Чистое решение «что значит этот отчёт для этой страницы в этом браузере».
fn judge_report(
    scenario: &PageScenario,
    browser: BrowserKind,
    report: Result<PageReport, ScenarioError>,
) -> Verdict {
    let report = match report {
        Ok(report) => report,
        Err(error) => return Verdict::fail(error.to_string()),
    };
    if report.scenario != scenario.id {
        return Verdict::fail(format!(
            "the page reported itself as '{}', expected '{}'",
            report.scenario, scenario.id
        ));
    }
    match report.status {
        PageStatus::Pass => Verdict::pass(String::new()),
        PageStatus::Unsupported if scenario.may_be_unsupported_in(browser) => {
            Verdict::unsupported(checks_summary(&report))
        }
        PageStatus::Unsupported => Verdict::fail(format!(
            "unsupported is not allowed for {} in {browser:?}: {}",
            scenario.id,
            checks_summary(&report)
        )),
        PageStatus::Fail => Verdict::fail(checks_summary(&report)),
    }
}

/// Проверки отчёта одной строкой: сначала провалившиеся.
fn checks_summary(report: &PageReport) -> String {
    let mut failed: Vec<String> = report
        .checks
        .iter()
        .filter(|check| !check.ok)
        .map(|check| format!("{} FAILED ({})", check.name, check.detail))
        .collect();
    if failed.is_empty() {
        failed.push(format!("{} checks, none failed", report.checks.len()));
    }
    failed.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::ReportCheck;

    fn report(scenario: &str, status: PageStatus, checks: Vec<ReportCheck>) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: scenario.to_owned(),
            status,
            checks,
            user_agent: String::new(),
            elapsed_ms: 1,
            metrics: Default::default(),
        }
    }

    fn scenario(id: &str) -> PageScenario {
        *PAGE_SCENARIOS
            .iter()
            .find(|s| s.id == id)
            .expect("known scenario")
    }

    fn step(subject: &str, verdict: Verdict) -> StepResult {
        StepResult::new(
            "pages",
            BrowserKind::Firefox,
            subject,
            verdict,
            Duration::from_secs(1),
        )
    }

    #[test]
    fn kept_directory_is_noted_on_the_first_failure_only() {
        let mut results = vec![
            step("mse-fmp4", Verdict::pass(String::new())),
            step("drm-probe", Verdict::fail("playing-after-probe FAILED")),
            step("series", Verdict::fail("episode-1-playing FAILED")),
        ];
        let keep = mark_first_failure_with_kept_directory(&mut results, Path::new("/w/run-1"));
        assert!(keep, "a failed page must ask to keep the run directory");
        assert_eq!(
            results[1].detail,
            "playing-after-probe FAILED\nrun directory kept for inspection: /w/run-1"
        );
        // Остальные строки не тронуты: проход без пути, следующий провал без дубля.
        assert_eq!(results[0].detail, "");
        assert_eq!(results[2].detail, "episode-1-playing FAILED");
    }

    #[test]
    fn nothing_is_kept_when_every_page_passed_or_is_unsupported() {
        let mut results = vec![
            step("mse-fmp4", Verdict::pass(String::new())),
            step("mse-worker", Verdict::unsupported("1 checks, none failed")),
        ];
        let before = results.clone();
        let keep = mark_first_failure_with_kept_directory(&mut results, Path::new("/w/run-2"));
        assert!(!keep);
        assert_eq!(results, before);
    }

    #[test]
    fn url_carries_run_report_origin_and_other_origin() {
        let run = RunId::new("abc-1").expect("run id");
        let url = page_url("http://127.0.0.1:1", "http://localhost:2", "mse-fmp4", &run);
        assert_eq!(
            url,
            "http://127.0.0.1:1/site/pages/mse-fmp4.html?run=abc-1&report_origin=http://127.0.0.1:1&other_origin=http://localhost:2"
        );
    }

    #[test]
    fn pass_report_passes() {
        let verdict = judge_report(
            &scenario("mse-fmp4"),
            BrowserKind::Chromium,
            Ok(report("mse-fmp4", PageStatus::Pass, vec![])),
        );
        assert_eq!(verdict.outcome, Outcome::Pass);
    }

    #[test]
    fn fail_report_names_the_failed_check() {
        let checks = vec![
            ReportCheck {
                name: "plays".into(),
                ok: true,
                detail: String::new(),
            },
            ReportCheck {
                name: "seek".into(),
                ok: false,
                detail: "no segments near 20s".into(),
            },
        ];
        let verdict = judge_report(
            &scenario("mse-fmp4"),
            BrowserKind::Firefox,
            Ok(report("mse-fmp4", PageStatus::Fail, checks)),
        );
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("seek FAILED (no segments near 20s)")
        );
        assert!(!verdict.detail.contains("plays"));
    }

    #[test]
    fn unsupported_is_ok_only_where_the_table_allows_it() {
        let allowed = judge_report(
            &scenario("mse-worker"),
            BrowserKind::Firefox,
            Ok(report("mse-worker", PageStatus::Unsupported, vec![])),
        );
        assert_eq!(allowed.outcome, Outcome::Unsupported);
        let forbidden = judge_report(
            &scenario("mse-worker"),
            BrowserKind::Chromium,
            Ok(report("mse-worker", PageStatus::Unsupported, vec![])),
        );
        assert_eq!(forbidden.outcome, Outcome::Fail);
    }

    #[test]
    fn report_from_another_scenario_fails() {
        let verdict = judge_report(
            &scenario("mse-fmp4"),
            BrowserKind::Chromium,
            Ok(report("mse-webm", PageStatus::Pass, vec![])),
        );
        assert_eq!(verdict.outcome, Outcome::Fail);
    }

    #[test]
    fn missing_report_fails_with_the_timeout_text() {
        let timeout = ScenarioError::Site(crate::site::SiteError::ReportTimeout {
            run: RunId::new("r-2").expect("run id"),
            waited: Duration::from_secs(60),
        });
        let verdict = judge_report(&scenario("mse-fmp4"), BrowserKind::Chromium, Err(timeout));
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("no report"));
    }
}
