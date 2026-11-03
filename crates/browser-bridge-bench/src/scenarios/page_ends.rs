//! `page-ends`: страница сама заканчивает передачу — и плеер обязан честно, быстро и
//! правильно это показать (сессия 09a2, решения 179 и 180).
//!
//! Три страницы, три честных конца:
//!  * `source-withdrawn` (Д-3): страница сняла источник и ничего не поставила. Тень ждёт
//!    допуск на пустой источник (5 с) и кончает сеанс кодом 4009 → плеер: «The page took the
//!    video back»;
//!  * `source-to-file` (Д-8): страница поставила обычный mp4-файл (не MSE). Тот же конец, но
//!    сразу, без допуска, а браузер САМ играет файл;
//!  * `page-freeze` (Д-2): главный поток страницы замёрз на 15 с, пульс «тень жива» пропал.
//!    Через ~10 с тишины экстеншен закрывает связь кодом 4003 → плеер: «Stream error».
//!
//! Что общего у всех трёх: сообщение в журнале окна нужное и вовремя, окно плеера закрылось,
//! доигрывания не было (`Playback drain finished` в журнале нет), страница отчиталась `pass`.
//! Приговор [`judge_page_end`] чистый: на вход только собранные улики.

use std::time::{Duration, Instant};

use super::capture::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, judge_early_report};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines, wait_for_session_granted,
};
use super::handoff_run::{ACK_TIMEOUT, ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_filter::PageFilter;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{EndNotice, HostLogEvidence, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::site::{PageReport, SiteError};

/// Допуск продукта на пустой источник (`EMPTY_SOURCE_GRACE_MS` в экстеншене, решение 179), с.
/// Страница, которая поставила не-MSE источник, обязана получить конец БЫСТРЕЕ этого срока.
const EMPTY_SOURCE_GRACE: Duration = Duration::from_secs(5);
/// Сколько ждать сообщения о конце сеанса после команды странице. Самая долгая страница
/// (`page-freeze`) ждёт ~10 с тишины пульса, остальное — запас на медленную машину.
const NOTICE_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждать, пока окно плеера станет управляемым (чтобы потом заметить, что оно ушло).
const CONTROLLABLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько окно плеера может показывать сообщение до закрытия.
const WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(20);
/// Строка журнала плеера, которой при честном конце БЫТЬ НЕ ДОЛЖНО: доигрывание буфера.
const DRAIN_FINISHED_MARKER: &str = "Playback drain finished";

/// Что плеер обязан сказать зрителю (поля строки `Browser session end notice shown …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedEnd {
    pub(super) notice: &'static str,
    pub(super) cause: &'static str,
    pub(super) end: &'static str,
}

/// Страница честно забрала видео (код 4009).
const PAGE_TOOK_VIDEO_BACK: ExpectedEnd = ExpectedEnd {
    notice: "page_took_video_back",
    cause: "page_took_video_back",
    end: "none",
};
/// Сбой на стороне страницы (экстеншен закрыл связь кодом 4003).
const PAGE_STREAM_ERROR: ExpectedEnd = ExpectedEnd {
    notice: "stream_error",
    cause: "page_stream_error",
    end: "none",
};

/// В какие сроки (от команды странице) сообщение должно появиться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NoticeWindow {
    /// Раньше этого срока сообщение — подозрительно (допуск продукта не выдержан).
    pub(super) earliest: Duration,
    /// Позже этого срока — продукт не уложился.
    pub(super) latest: Duration,
}

impl NoticeWindow {
    fn contains(&self, elapsed: Duration) -> bool {
        elapsed >= self.earliest && elapsed <= self.latest
    }
}

/// Страница сценария и то, что должно из неё получиться.
#[derive(Debug, Clone, Copy)]
pub(super) struct EndPage {
    /// Идентификатор страницы стенда.
    pub(super) id: &'static str,
    /// Действие страницы по команде робота (`hook:<имя>`).
    pub(super) hook: &'static str,
    /// Какое сообщение должен показать плеер.
    pub(super) expected: ExpectedEnd,
    /// В какие сроки.
    pub(super) window: NoticeWindow,
}

/// Страницы сценария. Сроки — по решениям 179/180 с запасом на опрос команд (250 мс), пульс
/// (1 с) и медленную машину.
pub(super) const END_PAGES: &[EndPage] = &[
    // Допуск 5 с: раньше ~4 с значит, что продукт не стал ждать решения страницы.
    EndPage {
        id: "source-withdrawn",
        hook: "withdraw-source",
        expected: PAGE_TOOK_VIDEO_BACK,
        window: NoticeWindow {
            earliest: EMPTY_SOURCE_GRACE.saturating_sub(Duration::from_secs(1)),
            latest: EMPTY_SOURCE_GRACE.saturating_add(Duration::from_secs(7)),
        },
    },
    // Не-MSE источник: сразу, строго раньше допуска на пустой источник.
    EndPage {
        id: "source-to-file",
        hook: "switch-to-file",
        expected: PAGE_TOOK_VIDEO_BACK,
        window: NoticeWindow {
            earliest: Duration::ZERO,
            latest: EMPTY_SOURCE_GRACE.saturating_sub(Duration::from_millis(500)),
        },
    },
    // Тишина пульса 10 с (последний пульс мог быть за ≤ 1 с до заморозки) + опрос команд.
    EndPage {
        id: "page-freeze",
        hook: "freeze-main-thread",
        expected: PAGE_STREAM_ERROR,
        window: NoticeWindow {
            earliest: Duration::from_secs(8),
            latest: Duration::from_secs(15),
        },
    },
];

/// Идентификаторы страниц сценария (для проверки `--page` в командной строке).
pub fn end_page_ids() -> Vec<&'static str> {
    END_PAGES.iter().map(|page| page.id).collect()
}

/// Все страницы сценария одного браузера (каждая в свежем окружении), с учётом фильтра.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    END_PAGES
        .iter()
        .filter(|page| filter.admits(page.id))
        .map(|page| run_page(stand, browser, *page))
        .collect()
}

fn run_page(stand: &Stand, browser: BrowserKind, page: EndPage) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| drive(stand, environment, browser, page));
    StepResult::new("page-ends", browser, page.id, verdict, started.elapsed())
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page: EndPage,
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
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("hand_off_tab: {other:?}\n{log}")));
        }
    }
    run.tell_page("handed-off");

    // Сначала доказываем, что передача состоялась: без неё «конец сеанса» ничего не значит.
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let observation = match start_of_session(&mut run, &control)? {
        Ok(()) => provoke_end(&mut run, &control, page),
        Err(message) => EndObservation::not_reached(&message),
    };

    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let drain_lines = run
        .player_log
        .snapshot_forbidden(&[DRAIN_FINISHED_MARKER])?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_page_end(
        &page,
        &PageEndEvidence {
            observation,
            drain_lines,
            host,
            page_report,
            forbidden_player_lines: forbidden,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Передача состоялась: пропуск от посредника, кадры в плеере, окно управляемо.
/// Внешний `Result` — ошибка стенда, внутренний — звено цепи не сработало.
fn start_of_session(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
) -> Result<Result<(), String>, ScenarioError> {
    if let Err(message) = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        return Ok(Err(message));
    }
    if let Err(error) = run
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
    {
        return Ok(Err(format!("player did not present frames: {error}")));
    }
    Ok(control
        .wait_until_controllable(CONTROLLABLE_TIMEOUT)
        .map_err(|error| format!("the player window never became controllable: {error}")))
}

/// Что стенд увидел после команды странице.
#[derive(Debug)]
struct EndObservation {
    /// Сообщение о конце сеанса и через сколько после команды оно появилось.
    notice: Result<(EndNotice, Duration), String>,
    /// Окно плеера закрылось (ушло с шины) после сообщения.
    window_closed: Result<(), String>,
    /// Страница подтвердила, что выполнила своё действие.
    hook_acknowledged: Result<(), String>,
}

impl EndObservation {
    /// Передача не состоялась, до конца дело не дошло.
    fn not_reached(reason: &str) -> Self {
        let skipped = format!("not awaited: {reason}");
        Self {
            notice: Err(skipped.clone()),
            window_closed: Err(skipped.clone()),
            hook_acknowledged: Err(skipped),
        }
    }
}

/// Команда странице → ждём сообщение плеера → ждём, пока окно уйдёт → ждём подтверждение.
fn provoke_end(run: &mut HandoffRun<'_>, control: &PlayerControl, page: EndPage) -> EndObservation {
    let command = format!("hook:{}", page.hook);
    // Строки журнала до команды (в том числе сообщения прошлых окон) в счёт не идут.
    if let Err(error) = run.player_log.skip_history() {
        return EndObservation::not_reached(&format!("cannot read the player log: {error}"));
    }
    let started = Instant::now();
    run.tell_page(&command);
    let notice = run
        .player_log
        .wait_for_end_notice(NOTICE_TIMEOUT)
        .map(|found| (found, started.elapsed()))
        .map_err(|error| format!("no end notice from the player: {error}"));
    let window_closed = control
        .wait_until_gone(WINDOW_GONE_TIMEOUT)
        .map_err(|error| format!("the player window did not close: {error}"));
    let hook_acknowledged = run
        .wait_ack(&command, ACK_TIMEOUT)
        .map_err(|message| format!("the page did not finish its action: {message}"));
    EndObservation {
        notice,
        window_closed,
        hook_acknowledged,
    }
}

/// Всё, что собрал прогон: вход чистого приговора.
struct PageEndEvidence {
    observation: EndObservation,
    /// Строки «Playback drain finished» (честный конец без доигрывания их не пишет).
    drain_lines: Vec<String>,
    host: HostLogEvidence,
    page_report: Result<PageReport, SiteError>,
    forbidden_player_lines: Vec<String>,
    page_log: String,
}

/// Чистый приговор: правильное сообщение, в срок, окно закрылось, доигрывания не было,
/// страница отчиталась `pass`.
fn judge_page_end(page: &EndPage, evidence: &PageEndEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    let observation = &evidence.observation;
    let mut timing = String::from("no notice");
    match &observation.notice {
        Ok((found, elapsed)) => {
            timing = format!("{} after {:.1} s", found.line.trim(), elapsed.as_secs_f64());
            failures.extend(notice_failures(&page.expected, found));
            failures.extend(timing_failures(&page.window, *elapsed));
        }
        Err(message) => failures.push(message.clone()),
    }
    if let Err(message) = &observation.window_closed {
        failures.push(message.clone());
    }
    if let Err(message) = &observation.hook_acknowledged {
        failures.push(message.clone());
    }
    if !evidence.drain_lines.is_empty() {
        failures.push(format!(
            "the player drained the buffer ('{DRAIN_FINISHED_MARKER}') after an honest end: {}",
            evidence.drain_lines.join(" | ")
        ));
    }
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, page.id));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let summary = format!(
        "{timing}; window {:.1}-{:.1} s; window closed: {}",
        page.window.earliest.as_secs_f64(),
        page.window.latest.as_secs_f64(),
        observation.window_closed.is_ok(),
    );
    // При провале журнал страницы — главная улика.
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Поля сообщения совпадают с ожидаемыми, каждое названо отдельно.
fn notice_failures(expected: &ExpectedEnd, found: &EndNotice) -> Vec<String> {
    [
        ("notice", expected.notice, found.notice.as_str()),
        ("cause", expected.cause, found.cause.as_str()),
        ("end", expected.end, found.end.as_str()),
    ]
    .into_iter()
    .filter(|(_, wanted, actual)| wanted != actual)
    .map(|(field, wanted, actual)| {
        format!(
            "end notice {field}={actual}, expected {wanted}: {}",
            found.line.trim()
        )
    })
    .collect()
}

/// Сообщение пришло в отведённое окно сроков.
fn timing_failures(window: &NoticeWindow, elapsed: Duration) -> Vec<String> {
    if window.contains(elapsed) {
        return Vec::new();
    }
    let kind = if elapsed < window.earliest {
        "too early (the product did not wait out its grace period)"
    } else {
        "too late"
    };
    vec![format!(
        "end notice came {:.1} s after the command, {kind}; allowed {:.1}-{:.1} s",
        elapsed.as_secs_f64(),
        window.earliest.as_secs_f64(),
        window.latest.as_secs_f64()
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;
    use crate::site::{PageStatus, ReportCheck, RunId};

    fn page(id: &str) -> EndPage {
        *END_PAGES
            .iter()
            .find(|page| page.id == id)
            .expect("a known end page")
    }

    fn notice(notice: &str, cause: &str, end: &str) -> EndNotice {
        EndNotice {
            notice: notice.into(),
            cause: cause.into(),
            end: end.into(),
            line: format!(
                "Browser session end notice shown notice={notice} cause={cause} end={end}"
            ),
        }
    }

    fn pass_report(id: &str) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: id.into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }
    }

    /// Здоровая улика для страницы: нужное сообщение в середине окна сроков.
    fn healthy(end_page: &EndPage) -> PageEndEvidence {
        let middle = (end_page.window.earliest + end_page.window.latest) / 2;
        PageEndEvidence {
            observation: EndObservation {
                notice: Ok((
                    notice(
                        end_page.expected.notice,
                        end_page.expected.cause,
                        end_page.expected.end,
                    ),
                    middle,
                )),
                window_closed: Ok(()),
                hook_acknowledged: Ok(()),
            },
            drain_lines: Vec::new(),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            page_report: Ok(pass_report(end_page.id)),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".into(),
        }
    }

    #[test]
    fn healthy_evidence_passes_for_every_end_page() {
        for end_page in END_PAGES {
            let verdict = judge_page_end(end_page, &healthy(end_page));
            assert_eq!(
                verdict.outcome,
                Outcome::Pass,
                "{}: {}",
                end_page.id,
                verdict.detail
            );
        }
    }

    #[test]
    fn a_wrong_notice_names_the_wrong_field() {
        let source = page("source-withdrawn");
        let mut evidence = healthy(&source);
        evidence.observation.notice = Ok((
            notice("tab_closed", "page_took_video_back", "none"),
            Duration::from_secs(6),
        ));
        let verdict = judge_page_end(&source, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("notice=tab_closed, expected page_took_video_back")
        );
        assert!(!verdict.detail.contains("end notice cause"));
    }

    #[test]
    fn the_freeze_page_wants_the_page_side_cause_not_the_player_side_one() {
        let freeze = page("page-freeze");
        let mut evidence = healthy(&freeze);
        // Тот же текст для зрителя, но причина — сбой СО СТОРОНЫ ПЛЕЕРА: это другая дыра.
        evidence.observation.notice = Ok((
            notice("stream_error", "protocol_violation", "none"),
            Duration::from_secs(11),
        ));
        let verdict = judge_page_end(&freeze, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("cause=protocol_violation, expected page_stream_error")
        );
    }

    #[test]
    fn the_grace_period_is_enforced_from_both_sides() {
        let source = page("source-withdrawn");
        let mut early = healthy(&source);
        early.observation.notice = Ok((
            notice("page_took_video_back", "page_took_video_back", "none"),
            Duration::from_secs(2),
        ));
        let early_verdict = judge_page_end(&source, &early);
        assert_eq!(early_verdict.outcome, Outcome::Fail);
        assert!(early_verdict.detail.contains("too early"));
        let mut late = healthy(&source);
        late.observation.notice = Ok((
            notice("page_took_video_back", "page_took_video_back", "none"),
            Duration::from_secs(20),
        ));
        let late_verdict = judge_page_end(&source, &late);
        assert_eq!(late_verdict.outcome, Outcome::Fail);
        assert!(late_verdict.detail.contains("too late"));
    }

    #[test]
    fn a_file_source_must_end_faster_than_the_empty_source_grace() {
        let file = page("source-to-file");
        assert!(file.window.latest < EMPTY_SOURCE_GRACE);
        let mut slow = healthy(&file);
        slow.observation.notice = Ok((
            notice("page_took_video_back", "page_took_video_back", "none"),
            EMPTY_SOURCE_GRACE,
        ));
        let verdict = judge_page_end(&file, &slow);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("too late"));
    }

    #[test]
    fn no_notice_at_all_is_a_failure_with_the_reason() {
        let freeze = page("page-freeze");
        let mut evidence = healthy(&freeze);
        evidence.observation.notice = Err("no end notice from the player: timed out".into());
        let verdict = judge_page_end(&freeze, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("no end notice from the player"));
    }

    #[test]
    fn draining_after_an_honest_end_is_a_failure() {
        let source = page("source-withdrawn");
        let mut evidence = healthy(&source);
        evidence.drain_lines = vec!["INFO Playback drain finished class=tab_closed".into()];
        let verdict = judge_page_end(&source, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("drained the buffer"));
    }

    #[test]
    fn an_open_window_an_unacknowledged_hook_and_a_failed_page_each_fail() {
        let file = page("source-to-file");
        let mut open_window = healthy(&file);
        open_window.observation.window_closed = Err("the player window did not close".into());
        assert_eq!(judge_page_end(&file, &open_window).outcome, Outcome::Fail);
        let mut silent_page = healthy(&file);
        silent_page.observation.hook_acknowledged =
            Err("the page did not finish its action".into());
        assert_eq!(judge_page_end(&file, &silent_page).outcome, Outcome::Fail);
        let mut broken_page = healthy(&file);
        broken_page.page_report = Ok(PageReport {
            status: PageStatus::Fail,
            checks: vec![ReportCheck {
                name: "page-plays-the-file-by-itself".into(),
                ok: false,
                detail: "frames 0 -> 0".into(),
            }],
            ..pass_report(file.id)
        });
        let verdict = judge_page_end(&file, &broken_page);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("page-plays-the-file-by-itself (frames 0 -> 0)")
        );
    }

    #[test]
    fn a_session_that_never_started_reports_every_step_as_not_awaited() {
        let source = page("source-withdrawn");
        let mut evidence = healthy(&source);
        evidence.observation = EndObservation::not_reached("host did not grant the session");
        let verdict = judge_page_end(&source, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("not awaited: host did not grant the session")
        );
    }

    /// Имя действия из таблицы объявлено в самой странице (иначе вживую страница ответит
    /// «no such hook», и сценарий упадёт только на стенде).
    #[test]
    fn every_end_page_declares_its_hook_in_the_page_source() {
        let pages_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site/pages");
        for end_page in END_PAGES {
            let source = std::fs::read_to_string(pages_dir.join(format!("{}.html", end_page.id)))
                .unwrap_or_else(|error| panic!("page {}: {error}", end_page.id));
            assert!(
                source.contains(&format!("'{}'", end_page.hook)),
                "page {} does not declare the hook '{}'",
                end_page.id,
                end_page.hook
            );
        }
    }

    #[test]
    fn end_pages_are_unique_known_pages_with_hooks_and_sane_windows() {
        let mut seen = std::collections::HashSet::new();
        for end_page in END_PAGES {
            assert!(seen.insert(end_page.id), "duplicate {}", end_page.id);
            assert!(
                PAGE_SCENARIOS
                    .iter()
                    .any(|scenario| scenario.id == end_page.id),
                "{} is not in the page catalog",
                end_page.id
            );
            assert!(!end_page.hook.is_empty());
            assert!(end_page.window.earliest < end_page.window.latest);
            assert!(end_page.window.latest < NOTICE_TIMEOUT);
        }
        assert_eq!(
            end_page_ids(),
            ["source-withdrawn", "source-to-file", "page-freeze"]
        );
    }
}
