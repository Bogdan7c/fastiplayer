//! Отказ входа без попапа: итог человеку показывает плашка на странице, а на значке вкладки
//! появляется «!». Сеанса нет, видео остаётся у браузера (его настоящий счётчик кадров растёт).

use super::super::capture::refusals::{
    REFUSAL_WINDOW, browser_keeps_playing_failures, browser_real_frame_metrics,
};
use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{format_browser_log, page_failures};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{DEFAULT_QUERY, EarlyUnsupported, Opened, open_ready};
use super::extension_probe::read_badge_text;
use super::menu_entry::{BANNER_TIMEOUT, ClickSpot};
use super::start_rows::prepare_entry;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::observe::read_host_evidence;

/// Значок вкладки после отказа.
const REFUSAL_BADGE: &str = "!";

/// Строка отказа без попапа.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReportRow {
    pub(super) entry: Entry,
    pub(super) page_id: &'static str,
    pub(super) click: ClickSpot,
    /// Имя случая плашки.
    pub(super) banner: &'static str,
    /// Проверять ли, что браузер всё это время сам декодирует видео. У DRM-ролика на стенде это
    /// неверно: ролик короткий и без цикла, а вход рукой (калибровка, меню) дольше его длины, поэтому
    /// к началу окна он уже доиграл. Для него «видео осталось у браузера» доказывает отсутствие
    /// сеанса (плеер не был разбужен) и плашка отказа.
    pub(super) check_browser_decodes: bool,
}

pub(super) fn run_report(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: ReportRow,
) -> Result<Verdict, ScenarioError> {
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        row.page_id,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    prepare_entry(&mut ready, row.entry)?;
    // Окно «браузер играет сам» открываем ДО входа (как в строках отказов попапа).
    ready.run.tell_page("handed-off");
    ready.enter_at(row.entry, row.click)?;
    let mut failures = Vec::new();
    if let Err(problem) = ready.wait_for_banner(row.banner, BANNER_TIMEOUT) {
        failures.push(problem);
    }
    let prefix = ready.run.page_url_prefix();
    let badge = read_badge_text(ready.run.browser.as_mut(), &prefix);
    match &badge {
        Ok(text) if text == REFUSAL_BADGE => {}
        Ok(text) => failures.push(format!(
            "the tab badge says {text:?}, expected {REFUSAL_BADGE:?}"
        )),
        Err(problem) => failures.push(problem.clone()),
    }
    std::thread::sleep(REFUSAL_WINDOW);
    let report = ready.run.finish_page();
    let page_log = format_browser_log(&ready.run.take_page_log());
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    if host.session_granted {
        failures.push("the player granted a session although nothing may be handed off".to_owned());
    }
    failures.extend(page_failures(&report, row.page_id));
    let (window_s, real_frames) = browser_real_frame_metrics(&report);
    if row.check_browser_decodes {
        failures.extend(browser_keeps_playing_failures(window_s, real_frames));
    }
    let summary = format!(
        "banner={:?} badge={badge:?}; browser_real_frames={real_frames:?} in {window_s:?} s; session_granted={}",
        row.banner, host.session_granted
    );
    Ok(if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        Verdict::fail(format!("{}\n{summary}\n{page_log}", failures.join("; ")))
    })
}
