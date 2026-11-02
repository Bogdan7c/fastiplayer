//! `smoke`: сквозной путь «байты из страницы → кадр на экране плеера».
//!
//! Цепочка: страница `mse-fmp4?handoff=1` играет сама, стендовый перехватчик копирует её
//! MSE-байты, НАСТОЯЩИЙ посредник (поставленный настоящим установщиком) будит НАСТОЯЩИЙ
//! плеер, плеер показывает кадры. Проверяем каждое звено отдельно, чтобы при провале было
//! видно, где цепь порвалась.

use std::path::Path;
use std::time::{Duration, Instant};

use super::context::{ScenarioError, Stand};
use super::outcome::{StepResult, Verdict};
use super::pages::page_url;
use super::stand_extension::StandExtension;
use crate::browsers::{BrowserKind, ExtensionSource};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameEvidence, HostLogEvidence, PlayerLog, read_host_evidence};
use crate::site::{BrowserLogLine, PageReport, PageStatus, RunId};

/// Страница дымка.
const SMOKE_PAGE: &str = "mse-fmp4";
/// Сколько кадров должен показать плеер (2 с видео при 25 к/с): «играет», а не «открылся».
const MIN_PRESENTED_FRAMES: u64 = 50;
/// Сколько ждать пропуск на сеанс от посредника: запуск браузера, шина будит плеер, hello.
const SESSION_GRANTED_TIMEOUT: Duration = Duration::from_secs(30);
/// Шаг опроса журнала браузера.
const SESSION_POLL_STEP: Duration = Duration::from_millis(200);
/// Сводки воспроизведения приходят раз в 10 с, плюс запуск плеера и сеанса.
const FRAMES_TIMEOUT: Duration = Duration::from_secs(40);
/// Отчёт страницы: она ещё проверяет паузу и перемотку.
const PAGE_REPORT_TIMEOUT: Duration = Duration::from_secs(60);
/// Подстроки журнала плеера, которых быть не должно.
const FORBIDDEN_PLAYER_LINES: &[&str] = &["panic", "ERROR"];
/// Известный безвредный шум: на частной шине нет портала настроек рабочего стола, и плеер при
/// старте пишет ERROR про цветовую схему (тема берётся по умолчанию). К мосту не относится.
const BENIGN_PLAYER_ERRORS: &[&str] = &["XDG Settings Portal did not return response in time"];
/// Что перехватчик пишет в журнал после подключения к плееру.
const EXTENSION_ATTACHED_MARKER: &str = "attached";

pub(super) fn run(stand: &Stand, browser: BrowserKind) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| drive(stand, environment, browser));
    StepResult::new("smoke", browser, SMOKE_PAGE, verdict, started.elapsed())
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    // Установщик ДО запуска браузера: манифест посредника должен лежать в профиле.
    environment.connect_browser(browser_kind)?;
    // Журнал открываем до старта: плеера ещё нет, его разбудит посредник.
    let mut player_log = PlayerLog::open(environment.player_log());
    let main_origin = stand.site.main_origin();
    let interceptor = StandExtension::prepare(&stand.settings.work_root, &main_origin)?;
    let mut browser = stand.launch_browser(
        browser_kind,
        environment,
        vec![ExtensionSource::UnpackedDir(
            interceptor.dir().to_path_buf(),
        )],
    )?;

    let run = RunId::new_unique("smoke");
    let mut url = page_url(&main_origin, &stand.site.other_origin(), SMOKE_PAGE, &run);
    url.push_str("&handoff=1");
    browser.open_tab(&url)?;

    // Сначала пропуск на сеанс от НАШЕГО посредника: без него кадры могли бы прийти
    // откуда угодно (чужой плеер, прошлый прогон), и «кадры есть» ничего бы не доказывало.
    // Только после `session_granted` ждём кадры — они привязаны к сеансу, выданному нами.
    let stderr_log = browser_kind.stderr_log_path(environment.work_dir());
    let session = wait_for_session_granted(&stderr_log, SESSION_GRANTED_TIMEOUT)?;
    // Главное доказательство: кадры дошли до показа. Нет сеанса — ждать кадры бессмысленно.
    let frames = match session {
        Ok(()) => player_log
            .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
            .map_err(|error| format!("player did not present frames: {error}")),
        Err(message) => Err(format!("{message}; frames were not awaited")),
    };
    let page_report = stand.site.wait_report(&run, PAGE_REPORT_TIMEOUT);
    // Журнал посредника лежит в stderr браузера: читаем после мягкого закрытия.
    let exit = browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    let extension_lines = stand.site.take_logs(&run);
    let forbidden = player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;

    Ok(judge_smoke(&SmokeEvidence {
        frames,
        host,
        extension_lines,
        page_report,
        forbidden_player_lines: forbidden,
    }))
}

/// Всё, что собрал дымовой прогон, без привязки к браузеру и файлам: вход чистого вердикта.
struct SmokeEvidence {
    /// Сводка показанных кадров или причина, почему её нет.
    frames: Result<FrameEvidence, String>,
    /// Отметки посредника.
    host: HostLogEvidence,
    /// Журнал перехватчика и страницы.
    extension_lines: Vec<BrowserLogLine>,
    /// Отчёт страницы или ошибка ожидания.
    page_report: Result<PageReport, crate::site::SiteError>,
    /// Запрещённые строки журнала плеера (с шумом).
    forbidden_player_lines: Vec<String>,
}

/// Чистое решение «дымок прошёл или нет»: каждое звено цепи проверяется отдельно.
fn judge_smoke(evidence: &SmokeEvidence) -> Verdict {
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
    failures.extend(extension_failures(&evidence.extension_lines));
    failures.extend(page_failures(&evidence.page_report));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );

    let (presented, regressions) = evidence.frames.as_ref().map_or((0, 0), |found| {
        (found.presented_frames, found.position_regressions)
    });
    let summary = format!(
        "presented_frames={presented} position_regressions={regressions}; host: caller_accepted={} hello_accepted={} session_granted={}; extension log lines={}",
        evidence.host.caller_accepted,
        evidence.host.hello_accepted,
        evidence.host.session_granted,
        evidence.extension_lines.len(),
    );
    // При провале журнал браузера (перехватчик и страница) — главная улика, прикладываем его.
    let summary = if failures.is_empty() {
        summary
    } else {
        format!(
            "{summary}\n{}",
            format_browser_log(&evidence.extension_lines)
        )
    };
    Verdict::from_failures(&failures, summary)
}

/// Ждёт в stderr браузера отметку `session_granted` от посредника.
///
/// Внешний `Result` — ошибка чтения файла, внутренний — «за срок отметки не было».
fn wait_for_session_granted(
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

/// Сколько последних строк журнала браузера показывать при провале.
const FAILURE_LOG_TAIL: usize = 80;

/// Хвост журнала браузера для сообщения о провале.
fn format_browser_log(lines: &[BrowserLogLine]) -> String {
    let skipped = lines.len().saturating_sub(FAILURE_LOG_TAIL);
    let mut text = format!(
        "browser log ({} lines, last {}):",
        lines.len(),
        lines.len() - skipped
    );
    for entry in &lines[skipped..] {
        text.push_str(&format!("\n  [{:?}] {}", entry.source, entry.line));
    }
    text
}

/// Запрещённые строки журнала плеера без известного безвредного шума.
fn unexpected_player_lines(lines: &[String]) -> impl Iterator<Item = &String> {
    lines.iter().filter(|line| {
        !BENIGN_PLAYER_ERRORS
            .iter()
            .any(|known| line.contains(known))
    })
}

/// Все три отметки посредника обязательны.
fn host_failures(host: &HostLogEvidence) -> Vec<String> {
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

/// Перехватчик должен был написать, что подключился к плееру.
fn extension_failures(lines: &[BrowserLogLine]) -> Vec<String> {
    if lines
        .iter()
        .any(|entry| entry.line.contains(EXTENSION_ATTACHED_MARKER))
    {
        Vec::new()
    } else {
        vec![format!(
            "the stand extension never logged '{EXTENSION_ATTACHED_MARKER}' ({} lines seen)",
            lines.len()
        )]
    }
}

/// Страница должна была сыграть сама: без отчёта или с `fail` — провал.
fn page_failures(report: &Result<PageReport, crate::site::SiteError>) -> Vec<String> {
    match report {
        // Отчёт должен быть именно от дымовой страницы, а не от чужой с тем же прогоном.
        Ok(report) if report.scenario != SMOKE_PAGE => vec![format!(
            "the page reported itself as '{}', expected '{SMOKE_PAGE}'",
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
    use crate::site::{LogSource, ReportCheck};

    fn log_line(text: &str) -> BrowserLogLine {
        BrowserLogLine {
            source: LogSource::Extension,
            run: RunId::new("r-1").expect("run id"),
            line: text.to_owned(),
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
        };
        assert!(host_failures(&all).is_empty());
        let partial = HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: false,
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
    fn attached_line_is_required_in_the_extension_log() {
        assert!(
            extension_failures(&[log_line("hook installed"), log_line("attached to player")])
                .is_empty()
        );
        assert_eq!(extension_failures(&[log_line("hook installed")]).len(), 1);
        assert_eq!(extension_failures(&[]).len(), 1);
    }

    #[test]
    fn page_must_pass() {
        let run = RunId::new("r-1").expect("run id");
        let failing = PageReport {
            run,
            scenario: "mse-fmp4".into(),
            status: PageStatus::Fail,
            checks: vec![ReportCheck {
                name: "plays".into(),
                ok: false,
                detail: "stuck".into(),
            }],
            user_agent: String::new(),
            elapsed_ms: 0,
        };
        let messages = page_failures(&Ok(failing.clone()));
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("plays (stuck)"));
        let passing = PageReport {
            status: PageStatus::Pass,
            ..failing
        };
        assert!(page_failures(&Ok(passing)).is_empty());
    }

    fn full_host() -> HostLogEvidence {
        HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
        }
    }

    fn passing_report() -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: SMOKE_PAGE.into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
        }
    }

    fn healthy_evidence() -> SmokeEvidence {
        SmokeEvidence {
            frames: Ok(FrameEvidence {
                presented_frames: 60,
                position_regressions: 0,
                summary_line: "summary".into(),
            }),
            host: full_host(),
            extension_lines: vec![log_line("attached to player")],
            page_report: Ok(passing_report()),
            forbidden_player_lines: Vec::new(),
        }
    }

    #[test]
    fn healthy_evidence_passes() {
        let verdict = judge_smoke(&healthy_evidence());
        assert_eq!(verdict.outcome, crate::scenarios::outcome::Outcome::Pass);
        assert!(verdict.detail.contains("presented_frames=60"));
    }

    #[test]
    fn position_regressions_fail_the_smoke() {
        let mut evidence = healthy_evidence();
        evidence.frames = Ok(FrameEvidence {
            presented_frames: 60,
            position_regressions: 2,
            summary_line: "summary".into(),
        });
        let verdict = judge_smoke(&evidence);
        assert_eq!(verdict.outcome, crate::scenarios::outcome::Outcome::Fail);
        assert!(verdict.detail.contains("went backwards 2 time(s)"));
    }

    #[test]
    fn missing_frames_fail_the_smoke_with_the_reason() {
        let mut evidence = healthy_evidence();
        evidence.frames = Err("player did not present frames: timeout".into());
        let verdict = judge_smoke(&evidence);
        assert_eq!(verdict.outcome, crate::scenarios::outcome::Outcome::Fail);
        assert!(verdict.detail.contains("did not present frames"));
    }

    #[test]
    fn report_from_another_page_is_a_failure_even_if_it_passed() {
        let mut other = passing_report();
        other.scenario = "mse-webm".into();
        let messages = page_failures(&Ok(other.clone()));
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("'mse-webm', expected 'mse-fmp4'"));
        let mut evidence = healthy_evidence();
        evidence.page_report = Ok(other);
        assert_eq!(
            judge_smoke(&evidence).outcome,
            crate::scenarios::outcome::Outcome::Fail
        );
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
