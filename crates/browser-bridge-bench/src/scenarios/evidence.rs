//! Общие звенья доказательств для сценариев передачи (`smoke`, `capture`,
//! `player-commands`): пропуск посредника, журнал плеера, отчёт страницы.
//!
//! Всё здесь — чистые функции над уже собранными данными (кроме ожидания файла), поэтому
//! каждое звено проверяется тестами отдельно: при провале видно, где цепь порвалась.

use std::path::Path;
use std::time::{Duration, Instant};

use super::context::ScenarioError;
use crate::observe::{HostLogEvidence, read_host_evidence};
use crate::site::{BrowserLogLine, PageReport, PageStatus, SiteError};

/// Сколько ждать пропуск на сеанс от посредника: запуск браузера, шина будит плеер, hello.
pub(super) const SESSION_GRANTED_TIMEOUT: Duration = Duration::from_secs(45);
/// Шаг опроса журнала браузера.
const SESSION_POLL_STEP: Duration = Duration::from_millis(200);
/// Подстроки журнала плеера, которых быть не должно.
pub(super) const FORBIDDEN_PLAYER_LINES: &[&str] = &["panic", "ERROR"];
/// Известный безвредный шум: на частной шине нет портала настроек рабочего стола, и плеер при
/// старте пишет ERROR про цветовую схему (тема берётся по умолчанию). К мосту не относится.
const BENIGN_PLAYER_ERRORS: &[&str] = &["XDG Settings Portal did not return response in time"];
/// Сколько последних строк журнала страницы показывать при провале.
const FAILURE_LOG_TAIL: usize = 80;

/// Ждёт в stderr браузера отметку `session_granted` от посредника.
///
/// Внешний `Result` — ошибка чтения файла, внутренний — «за срок отметки не было».
pub(super) fn wait_for_session_granted(
    stderr_log: &Path,
    timeout: Duration,
) -> Result<Result<(), String>, ScenarioError> {
    let deadline = Instant::now() + timeout;
    loop {
        if read_host_evidence(stderr_log)?.session_granted {
            return Ok(Ok(()));
        }
        if Instant::now() >= deadline {
            return Ok(Err(format!(
                "host did not grant the session within {timeout:?}"
            )));
        }
        std::thread::sleep(SESSION_POLL_STEP);
    }
}

/// Хвост журнала страницы для сообщения о провале.
pub(super) fn format_browser_log(lines: &[BrowserLogLine]) -> String {
    let skipped = lines.len().saturating_sub(FAILURE_LOG_TAIL);
    let mut text = format!(
        "page log ({} lines, last {}):",
        lines.len(),
        lines.len() - skipped
    );
    for entry in &lines[skipped..] {
        text.push_str(&format!("\n  [{:?}] {}", entry.source, entry.line));
    }
    text
}

/// Запрещённые строки журнала плеера без известного безвредного шума.
pub(super) fn unexpected_player_lines(lines: &[String]) -> impl Iterator<Item = &String> {
    lines.iter().filter(|line| {
        !BENIGN_PLAYER_ERRORS
            .iter()
            .any(|known| line.contains(known))
    })
}

/// Все три отметки посредника обязательны.
pub(super) fn host_failures(host: &HostLogEvidence) -> Vec<String> {
    [
        (host.caller_accepted, "host did not accept the caller"),
        (host.hello_accepted, "host did not answer hello_accepted"),
        (host.session_granted, "host did not grant the session"),
    ]
    .into_iter()
    .filter(|(ok, _)| !ok)
    .map(|(_, message)| message.to_owned())
    .collect()
}

/// Отчёт страницы должен быть именно от ожидаемой страницы и со статусом `pass`.
pub(super) fn page_failures(
    report: &Result<PageReport, SiteError>,
    expected_page: &str,
) -> Vec<String> {
    match report {
        // Отчёт должен быть от нужной страницы, а не от чужой с тем же прогоном.
        Ok(report) if report.scenario != expected_page => vec![format!(
            "the page reported itself as '{}', expected '{expected_page}'",
            report.scenario
        )],
        Ok(report) if report.status == PageStatus::Pass => Vec::new(),
        Ok(report) => {
            let failed: Vec<String> = report
                .checks
                .iter()
                .filter(|check| !check.ok)
                .map(|check| format!("{} ({})", check.name, check.detail))
                .collect();
            vec![format!(
                "page report is {:?}: {}",
                report.status,
                failed.join(", ")
            )]
        }
        Err(error) => vec![format!("page: {error}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{LogSource, ReportCheck, RunId};

    fn report(scenario: &str, status: PageStatus, checks: Vec<ReportCheck>) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: scenario.into(),
            status,
            checks,
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }
    }

    #[test]
    fn every_missing_host_mark_is_reported_separately() {
        let none = host_failures(&HostLogEvidence::default());
        assert_eq!(none.len(), 3);
        let all = HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        };
        assert!(host_failures(&all).is_empty());
        let partial = HostLogEvidence {
            session_granted: false,
            session_grants: 0,
            ..all
        };
        assert_eq!(
            host_failures(&partial),
            vec!["host did not grant the session"]
        );
    }

    #[test]
    fn benign_portal_error_is_ignored_but_other_errors_are_not() {
        let lines = vec![
            "T ERROR XDG Settings Portal did not return response in time: timeout: 100ms"
                .to_owned(),
            "T ERROR decoder exploded".to_owned(),
            "thread main panicked at x".to_owned(),
        ];
        let unexpected: Vec<&String> = unexpected_player_lines(&lines).collect();
        assert_eq!(unexpected, vec![&lines[1], &lines[2]]);
    }

    #[test]
    fn page_must_pass_and_name_its_failed_checks() {
        let failing = report(
            "mse-fmp4",
            PageStatus::Fail,
            vec![ReportCheck {
                name: "plays".into(),
                ok: false,
                detail: "stuck".into(),
            }],
        );
        let messages = page_failures(&Ok(failing.clone()), "mse-fmp4");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("plays (stuck)"));
        let passing = PageReport {
            status: PageStatus::Pass,
            ..failing
        };
        assert!(page_failures(&Ok(passing), "mse-fmp4").is_empty());
    }

    #[test]
    fn report_from_another_page_is_a_failure_even_if_it_passed() {
        let other = report("mse-webm", PageStatus::Pass, Vec::new());
        let messages = page_failures(&Ok(other), "mse-fmp4");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("'mse-webm', expected 'mse-fmp4'"));
    }

    #[test]
    fn page_log_tail_is_limited_and_counted() {
        let lines: Vec<BrowserLogLine> = (0..FAILURE_LOG_TAIL + 5)
            .map(|index| BrowserLogLine {
                source: LogSource::Page,
                run: RunId::new("r-1").expect("run id"),
                line: format!("line {index}"),
            })
            .collect();
        let text = format_browser_log(&lines);
        assert!(text.contains(&format!("({} lines, last {FAILURE_LOG_TAIL})", lines.len())));
        assert!(!text.contains("line 0\n") && text.contains("line 5"));
    }

    #[test]
    fn session_wait_succeeds_when_the_log_has_the_grant_and_times_out_otherwise() {
        let directory = tempfile::tempdir().expect("tempdir");
        let granted = directory.path().join("granted.log");
        std::fs::write(
            &granted,
            "fastiplayer-browser-host: answered request=open_session bytes=240 reply=player:session_granted\n",
        )
        .expect("write log");
        assert_eq!(
            wait_for_session_granted(&granted, Duration::from_secs(1)).expect("readable"),
            Ok(())
        );
        let silent = directory.path().join("silent.log");
        std::fs::write(&silent, "noise\n").expect("write log");
        let timed_out =
            wait_for_session_granted(&silent, Duration::from_millis(300)).expect("readable");
        assert!(timed_out.expect_err("no grant").contains("did not grant"));
    }
}
