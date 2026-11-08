//! Строки честных концов 11a: плеер страницы сломался под тенью (решение 237) и «Return» на
//! вкладке входа по ссылке (решение 236: эта вкладка остаётся открытой).

use std::time::{Duration, Instant};

use super::facts::DRAIN_FINISHED_LINE;
use super::judge::{judge_page_player_failed_end, judge_returned_playing, judge_silent_viewer_end};
use super::rows_return::click_return_and_observe;
use super::steps::{Begun, CLOSE_WINDOW_GONE_TIMEOUT, RETURN_KIT_PAGE, Session, begin, verdict};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::player_control::PlayerControl;
use crate::scenarios::capture::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES};
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::entries::return_hand::{HandOpened, open_hand};
use crate::scenarios::evidence::{SESSION_GRANTED_TIMEOUT, wait_for_session_granted};
use crate::scenarios::handoff_run::HANDOFF_MODE_QUERY;
use crate::scenarios::outcome::Verdict;
use crate::site::{PageReport, SiteError};

/// Как ломается плеер страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageBreak {
    /// `endOfStream("decode")`: ошибка элемента держится, страница ничего не делает.
    DecodeError,
    /// Та же ошибка, а через секунду страница снимает источник (не «забрала видео», а умерла).
    DecodeErrorThenDrop,
}

impl PageBreak {
    /// Хук страницы `return-kit`.
    fn hook(self) -> &'static str {
        match self {
            Self::DecodeError => "hook:break-mse",
            Self::DecodeErrorThenDrop => "hook:break-mse-then-drop",
        }
    }
}

/// Сколько ждём сообщение после поломки: допуск сторожа (5 с) или пустого источника (5 с), затем
/// доигрывание скачанного (буфер страницы ~10 с) — с запасом на медленную машину.
const FAILURE_NOTICE_TIMEOUT: Duration = Duration::from_secs(60);
/// Страница-источник ссылок и ссылка, ведущая на файл `video-src` (вход по ссылке, 10c).
const LINK_SOURCE_PAGE: &str = "link-source";
const LINK_TARGET: &str = "link:autoplay";
const LINK_TARGET_PAGE: &str = "video-src";
/// Сколько вкладка по ссылке обязана оставаться открытой после «Return».
const LINK_TAB_MUST_STAY: Duration = Duration::from_secs(8);
/// Шаг проверки «вкладка открыта».
const LINK_TAB_POLL: Duration = Duration::from_millis(500);
/// Срок плашки «opened» на вкладке входа (она живёт 5 с, передача стартует секундами).
const OPENED_BANNER_TIMEOUT: Duration = Duration::from_secs(40);
/// Сколько ждём, пока окно плеера станет управляемым.
const CONTROLLABLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Строка 9: плеер страницы сломался → плеер доигрывает скачанное и говорит своими словами
/// (`cause=page_player_failed`), окно закрывается после сообщения, заглушки нет.
pub(super) fn run_page_player_failed(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page_break: PageBreak,
) -> Result<Verdict, ScenarioError> {
    let mut session = match begin(
        stand,
        environment,
        browser,
        RETURN_KIT_PAGE,
        HANDOFF_MODE_QUERY,
    )? {
        Begun::Running(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    let broken = break_and_watch_the_end(&mut session, page_break);
    session.record(broken);
    let (end, failures, notes) = session.close(&[])?;
    Ok(verdict(&end, failures, &notes))
}

fn break_and_watch_the_end(
    session: &mut Session<'_, '_>,
    page_break: PageBreak,
) -> Result<(), String> {
    let prefix = session.current_prefix();
    session.wait_placeholder_shown(&prefix)?;
    session
        .run()
        .player_log
        .skip_history()
        .map_err(|error| format!("player log: {error}"))?;
    let broke_at = Instant::now();
    session.tell_and_wait(page_break.hook())?;
    let notice = session
        .run()
        .player_log
        .wait_for_end_notice(FAILURE_NOTICE_TIMEOUT)
        .map_err(|error| format!("no end notice after the page player broke: {error}"))?;
    let after_break = broke_at.elapsed();
    let placeholder_gone = session.wait_placeholder_gone(&prefix);
    let window_gone = session.wait_window_gone(CLOSE_WINDOW_GONE_TIMEOUT);
    let drained = !session
        .run()
        .player_log
        .snapshot_forbidden(&[DRAIN_FINISHED_LINE])
        .map_err(|error| error.to_string())?
        .is_empty();
    session.notes.push(format!(
        "{page_break:?}: {} after {:.1} s, drained={drained}",
        notice.line.trim(),
        after_break.as_secs_f64()
    ));
    session
        .failures
        .extend(judge_page_player_failed_end(&notice, after_break, drained));
    placeholder_gone?;
    window_gone.map(|_| ())
}

/// Строка 10: вход по ссылке (вкладка цели впереди) → «Return» на вкладке цели → её страница играет
/// сама, вкладка остаётся открытой, окно закрылось молча.
pub(super) fn run_link_return(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut hand = match open_hand(
        stand,
        environment,
        browser,
        LINK_SOURCE_PAGE,
        HANDOFF_MODE_QUERY,
    )? {
        HandOpened::Ready(hand) => *hand,
        HandOpened::Finished(verdict) => return Ok(verdict),
    };
    hand.watch_page_layer()?;
    let target_prefix = hand.run_ref().sibling_page_prefix(LINK_TARGET_PAGE);
    let entered = hand.enter_by_link(LINK_TARGET, &target_prefix)?;
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let mut session = Session {
        hand,
        control,
        failures: entered.failures,
        notes: vec![entered.note],
    };
    let target_run = entered.target_run;
    let banner = session
        .hand
        .wait_banner_on(&target_prefix, "opened", OPENED_BANNER_TIMEOUT);
    session.record(banner);
    let started = link_session_started(&mut session);
    if session.record(started).is_some() {
        session
            .hand
            .run_ref()
            .tell_page_of(&target_run, "handed-off");
        if let Some(facts) = click_return_and_observe(&mut session, &target_run, &target_prefix) {
            session.failures.extend(judge_returned_playing(&facts));
        }
        let stays = link_tab_stays(&mut session, &target_prefix);
        session.record(stays);
    }
    // Отчёт страницы цели судится здесь, а не общим `close`: одна её проверка до передачи у входа по
    // ссылке не имеет смысла (см. `LINK_TARGET_UNMEASURABLE_CHECK`).
    let target_report = session.hand.run_ref().finish_page_of(&target_run);
    session
        .failures
        .extend(link_target_report_failures(&target_report));
    let (end, mut failures, notes) = session.close(&[])?;
    failures.extend(judge_silent_viewer_end(&end.player));
    Ok(verdict(&end, failures, &notes))
}

/// Проверка страницы цели, которую вход по ссылке делает неизмеримой: вход передаёт видео в тот же
/// миг, когда оно заиграло (решение 217), и браузер не успевает декодировать кадры для пробы страницы
/// «настоящий счётчик видит кадры, пока браузер играет сам». Поэтому строки входа по ссылке в
/// `entries` отчёт цели не судят вовсе; здесь судится всё, кроме этой одной проверки.
const LINK_TARGET_UNMEASURABLE_CHECK: &str = "real-frame-probe";

/// Провалы отчёта страницы цели без неизмеримой проверки.
fn link_target_report_failures(report: &Result<PageReport, SiteError>) -> Vec<String> {
    match report {
        Ok(report) if report.scenario != LINK_TARGET_PAGE => vec![format!(
            "the link target reported itself as '{}', expected '{LINK_TARGET_PAGE}'",
            report.scenario
        )],
        Ok(report) => {
            let failed: Vec<String> = report
                .checks
                .iter()
                .filter(|check| !check.ok && check.name != LINK_TARGET_UNMEASURABLE_CHECK)
                .map(|check| format!("{} ({})", check.name, check.detail))
                .collect();
            if failed.is_empty() {
                Vec::new()
            } else {
                vec![format!("link target page: {}", failed.join(", "))]
            }
        }
        Err(error) => vec![format!("link target page: {error}")],
    }
}

/// Сеанс входа по ссылке идёт: пропуск, кадры, окно управляемо.
fn link_session_started(session: &mut Session<'_, '_>) -> Result<(), String> {
    wait_for_session_granted(&session.run().stderr_log, SESSION_GRANTED_TIMEOUT)
        .map_err(|error| error.to_string())??;
    session
        .run()
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
        .map_err(|error| format!("player did not present frames: {error}"))?;
    session
        .control
        .wait_until_controllable(CONTROLLABLE_TIMEOUT)
        .map_err(|error| format!("the player window never became controllable: {error}"))
}

/// Вкладка по ссылке открыта всё время наблюдения (её закрывает только конец со стороны плеера).
fn link_tab_stays(session: &mut Session<'_, '_>, target_prefix: &str) -> Result<(), String> {
    let deadline = Instant::now() + LINK_TAB_MUST_STAY;
    while Instant::now() < deadline {
        if let Some(problem) = session.hand.tab_problem(target_prefix) {
            return Err(format!("after Return: {problem}"));
        }
        std::thread::sleep(LINK_TAB_POLL);
    }
    session.notes.push(format!(
        "the link tab stayed open for {LINK_TAB_MUST_STAY:?} after Return"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{PageStatus, ReportCheck, RunId};

    fn report(scenario: &str, checks: &[(&str, bool)]) -> PageReport {
        PageReport {
            run: RunId::new("r-1-t").expect("run id"),
            scenario: scenario.into(),
            status: if checks.iter().all(|(_, ok)| *ok) {
                PageStatus::Pass
            } else {
                PageStatus::Fail
            },
            checks: checks
                .iter()
                .map(|(name, ok)| ReportCheck {
                    name: (*name).into(),
                    ok: *ok,
                    detail: "detail".into(),
                })
                .collect(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }
    }

    #[test]
    fn the_link_target_report_is_judged_except_its_unmeasurable_probe_check() {
        let only_probe = report(
            LINK_TARGET_PAGE,
            &[("real-frame-probe", false), ("playing", true)],
        );
        assert!(link_target_report_failures(&Ok(only_probe)).is_empty());
        let real_failure = report(
            LINK_TARGET_PAGE,
            &[("real-frame-probe", false), ("page-clock-advances", false)],
        );
        let failures = link_target_report_failures(&Ok(real_failure));
        assert!(
            failures[0].contains("page-clock-advances")
                && !failures[0].contains("real-frame-probe"),
            "{failures:?}"
        );
        let wrong_page = report("mse-fmp4", &[]);
        assert!(!link_target_report_failures(&Ok(wrong_page)).is_empty());
    }
}
