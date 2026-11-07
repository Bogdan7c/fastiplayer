//! Строки отказов: человек вошёл, а передавать нечего или нельзя. Ожидаем не кадр, а текст
//! в попапе: попап остаётся открытым и называет случай (`data-notice`), сеанса с плеером нет,
//! видео остаётся у браузера (его настоящий счётчик кадров растёт).

use std::time::Duration;

use super::super::capture::refusals::{
    REFUSAL_WINDOW, browser_keeps_playing_failures, browser_real_frame_metrics,
};
use super::super::context::{ScenarioError, Stand};
use super::super::evidence::page_failures;
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    DEFAULT_QUERY, EarlyUnsupported, Opened, POPUP_SETTLE_TIMEOUT, ReadyRun, open_ready,
    with_extra_failures,
};
use super::robot::wait_for_settled_popup;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::observe::{HostLogEvidence, read_host_evidence};

/// Страница самого браузера не читается экстеншеном: попап обязан сказать это текстом.
const NOT_SCRIPTABLE_NOTICE: &str = "tab_not_scriptable";
/// Сколько даём странице браузера открыться, прежде чем входить.
const BROWSER_PAGE_SETTLE: Duration = Duration::from_secs(2);

/// Что ждём от попапа на странице отказа.
#[derive(Debug, Clone, Copy)]
pub(super) struct RefusalRow {
    /// Имя правила для таблицы.
    name: &'static str,
    /// Идентификатор случая в `data-notice`.
    notice: &'static str,
    /// Как судить страницу, отчитавшуюся `unsupported` до входа.
    early_unsupported: EarlyUnsupported,
}

impl RefusalRow {
    /// Только баннер и превью: передавать нечего.
    pub(super) const NO_VIDEO: Self = Self {
        name: "refusal no_video",
        notice: "no_video",
        early_unsupported: EarlyUnsupported::OnlyWhereTheTableAllows,
    };
    /// Видео под DRM.
    pub(super) const PROTECTED: Self = Self {
        name: "refusal protected",
        notice: "protected",
        early_unsupported: EarlyUnsupported::OnlyWhereTheTableAllows,
    };
    /// Родной HLS: с нашим экстеншеном страница честно `unsupported` ещё до входа (в обоих браузерах).
    pub(super) const NATIVE_HLS: Self = Self {
        name: "refusal not_supported_yet",
        notice: "not_supported_yet",
        early_unsupported: EarlyUnsupported::Always,
    };
    /// `blob:`-файл в `src`: форма, которую экстеншен пока честно не передаёт (`blob_file`).
    pub(super) const BLOB_FILE: Self = Self {
        name: "refusal not_supported_yet",
        notice: "not_supported_yet",
        early_unsupported: EarlyUnsupported::OnlyWhereTheTableAllows,
    };

    pub(super) fn name(self) -> &'static str {
        self.name
    }
}

/// Отказ на странице стенда: вход, текст попапа, сеанса нет, браузер играет сам.
pub(super) fn run_refusal(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    entry: Entry,
    page_id: &'static str,
    refusal: RefusalRow,
) -> Result<Verdict, ScenarioError> {
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        page_id,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        refusal.early_unsupported,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    // Окно «браузер играет сам» открываем ДО входа: человек входит медленнее, чем ролик DRM
    // (он короткий и не зациклен) успевает доиграть, и окно, начатое после отказа, осталось бы пустым.
    ready.run.tell_page("handed-off");
    ready.enter(entry)?;
    let mut failures = Vec::new();
    let shown = expect_notice(&mut ready, refusal.notice, &mut failures);
    std::thread::sleep(REFUSAL_WINDOW);
    // Попап остался открытым и после паузы: отказ его не закрывает.
    match ready.run.browser.popup_snapshot() {
        Ok(Some(snapshot)) if snapshot.notice.as_deref() == Some(refusal.notice) => {}
        Ok(other) => failures.push(format!(
            "the popup did not stay open with '{}': {other:?}",
            refusal.notice
        )),
        Err(error) => failures.push(format!("popup probe after the pause: {error}")),
    }
    let report = ready.run.finish_page();
    let page_log = super::super::evidence::format_browser_log(&ready.run.take_page_log());
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    failures.extend(session_failures(&host));
    failures.extend(page_failures(&report, page_id));
    let (window_s, real_frames) = browser_real_frame_metrics(&report);
    failures.extend(browser_keeps_playing_failures(window_s, real_frames));
    let summary = format!(
        "popup notice={shown:?}; browser_real_frames={real_frames:?} in {window_s:?} s; session_granted={}",
        host.session_granted
    );
    Ok(if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        Verdict::fail(format!("{}\n{summary}\n{page_log}", failures.join("; ")))
    })
}

/// Страница самого браузера: вкладку читать нельзя, плеер не разбужен.
pub(super) fn run_browser_page(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    entry: Entry,
) -> Result<Verdict, ScenarioError> {
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        "mse-fmp4",
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    // Новая вкладка со страницей самого браузера становится активной поверх страницы стенда.
    ready.run.browser.open_tab(browser_internal_page(browser))?;
    std::thread::sleep(BROWSER_PAGE_SETTLE);
    ready.enter(entry)?;
    let mut failures = Vec::new();
    let shown = expect_notice(&mut ready, NOT_SCRIPTABLE_NOTICE, &mut failures);
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    failures.extend(session_failures(&host));
    Ok(with_extra_failures(
        Verdict::pass(format!(
            "popup notice={shown:?}; session_granted={}",
            host.session_granted
        )),
        &failures,
    ))
}

/// Страница, которую экстеншен читать не может: внутренняя страница браузера.
fn browser_internal_page(browser: BrowserKind) -> &'static str {
    match browser {
        BrowserKind::Chromium => "chrome://version",
        BrowserKind::Firefox => "about:preferences",
    }
}

/// Ждёт итоговый текст попапа и сверяет с ожидаемым случаем; провал дописывает в `failures`.
fn expect_notice(
    ready: &mut ReadyRun<'_, '_>,
    expected: &str,
    failures: &mut Vec<String>,
) -> Option<String> {
    match wait_for_settled_popup(ready.run.browser.as_mut(), POPUP_SETTLE_TIMEOUT) {
        Ok(snapshot) => {
            if snapshot.notice.as_deref() != Some(expected) {
                failures.push(format!(
                    "the popup shows '{:?}', expected '{expected}'",
                    snapshot.notice
                ));
            }
            snapshot.notice
        }
        Err(message) => {
            failures.push(format!("expected the popup to say '{expected}': {message}"));
            None
        }
    }
}

/// Сеанса с плеером быть не должно.
fn session_failures(host: &HostLogEvidence) -> Vec<String> {
    if host.session_granted {
        vec!["the player granted a session although nothing may be handed off".to_owned()]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_granted_session_is_a_failure_for_a_refusal_row() {
        let granted = HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        };
        assert_eq!(session_failures(&granted).len(), 1);
        let quiet = HostLogEvidence {
            session_granted: false,
            session_grants: 0,
            ..granted
        };
        assert!(session_failures(&quiet).is_empty());
    }

    #[test]
    fn browser_internal_pages_differ_per_browser() {
        assert_ne!(
            browser_internal_page(BrowserKind::Chromium),
            browser_internal_page(BrowserKind::Firefox)
        );
    }

    #[test]
    fn the_refusal_table_names_the_expected_notice_ids() {
        assert_eq!(RefusalRow::NO_VIDEO.notice, "no_video");
        assert_eq!(RefusalRow::PROTECTED.notice, "protected");
        assert_eq!(RefusalRow::NATIVE_HLS.notice, "not_supported_yet");
    }
}
