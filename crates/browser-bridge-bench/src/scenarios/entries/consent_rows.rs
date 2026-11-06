//! Вопрос про локальный адрес (решение 199) на настоящих клавишах.
//!
//! Страница «из интернета» просит файл с локального адреса, настройка стоит в «спрашивать»:
//! попап рисует вопрос. Робот жмёт Tab (фокус обязан прийти на кнопку Allow, это контракт
//! разметки) и Enter. Allow → передача началась, кадры в рендере, файл качает фон. Don't allow
//! (два Tab и Enter) → сеанса нет, попап называет отказ, видео остаётся у браузера.

use std::time::Instant;

use super::super::capture::file_evidence::{file_failures, file_summary};
use super::super::capture::local_address::{
    DIRECT_QUERY, LOCAL_FILE_PAGE, LOCAL_PAGE, PUBLIC_HOST, sorted_file_requests,
    write_setting_function,
};
use super::super::capture::refusals::{
    REFUSAL_WINDOW, browser_keeps_playing_failures, browser_real_frame_metrics,
};
use super::super::capture::{CaptureEvidence, judge_capture};
use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{FORBIDDEN_PLAYER_LINES, format_browser_log, page_failures};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    EarlyUnsupported, Opened, POPUP_SETTLE_TIMEOUT, ReadyRun, open_ready, with_extra_failures,
};
use super::robot::wait_for_settled_popup;
use crate::browsers::{BrowserKind, PopupSnapshot};
use crate::desktop_input::Key;
use crate::environment::BenchEnvironment;
use crate::observe::read_host_evidence;

/// Случай попапа с вопросом.
const NEEDS_CONSENT_NOTICE: &str = "local_address_needs_consent";
/// Случай после «Don't allow».
const DECLINED_NOTICE: &str = "declined";
/// Значение настройки «спрашивать» в `storage.local` экстеншена.
const ASK_SETTING: &str = "ask";
/// Ответ скрипта записи настройки.
const SETTING_SAVED: &str = "saved";
/// Сколько раз можно нажать Tab, прежде чем признать «кнопка недостижима с клавиатуры»: документ
/// попапа, Allow, Deny и запас на ссылку настроек.
const MAX_TABS_TO_BUTTON: usize = 5;
/// Классы кнопок вопроса (контракт разметки попапа).
const ALLOW_CLASS: &str = "consent-allow";
const DENY_CLASS: &str = "consent-deny";

/// Что ответил человек на вопрос.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Decision {
    /// Allow: Tab, Enter.
    Allow,
    /// Don't allow: Tab, Tab, Enter.
    Deny,
}

impl Decision {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Allow => "consent allow",
            Self::Deny => "consent deny",
        }
    }

    /// Класс кнопки, на которой обязан стоять фокус перед Enter.
    fn button_class(self) -> &'static str {
        match self {
            Self::Allow => ALLOW_CLASS,
            Self::Deny => DENY_CLASS,
        }
    }
}

pub(super) fn run(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    entry: Entry,
    decision: Decision,
) -> Result<Verdict, ScenarioError> {
    let run_started_at = Instant::now();
    let tweaks = RunTweaks {
        loopback_host_aliases: vec![PUBLIC_HOST.to_owned()],
        page_host: Some(PUBLIC_HOST.to_owned()),
        ..RunTweaks::default()
    };
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        LOCAL_PAGE,
        &tweaks,
        DIRECT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    // Попап спрашивает только при настройке «спрашивать»; служебная страница настроек сбивает
    // активную вкладку, поэтому вкладку страницы возвращаем вперёд.
    let saved = ready
        .run
        .browser
        .run_in_extension_page(&write_setting_function(ASK_SETTING))?;
    if saved != SETTING_SAVED {
        ready.run.browser.shutdown()?;
        return Ok(Verdict::fail(format!(
            "could not write the setting: {saved}"
        )));
    }
    ready.run.activate_page_tab()?;
    ready.enter(entry)?;

    let mut failures = Vec::new();
    match wait_for_settled_popup(ready.run.browser.as_mut(), POPUP_SETTLE_TIMEOUT) {
        Ok(snapshot) => failures.extend(question_failures(&snapshot)),
        Err(message) => failures.push(format!("expected the consent question: {message}")),
    }
    // Настоящие Tab: фокус обязан дойти до нужной кнопки (Allow первой, Deny следом).
    let walk = ready.robot.press_tab_until(
        ready.run.browser.as_mut(),
        MAX_TABS_TO_BUTTON,
        |snapshot| snapshot.focus_is_button_with_class(decision.button_class()),
    )?;
    if !walk.reached {
        failures.push(format!(
            "after {} Tab the focus never reached '{}': {:?}",
            walk.presses,
            decision.button_class(),
            walk.last_snapshot.map(|snapshot| snapshot.focused)
        ));
    }
    let tab_note = format!("Tab x{} to {}", walk.presses, decision.button_class());
    // Передача начинается с Enter: файл фон может скачать за доли секунды, раньше, чем стенд
    // заметит закрытие попапа, поэтому граница «до/после передачи» ставится ДО нажатия.
    let decided_at = Instant::now();
    ready.robot.press(Key::Enter)?;
    let verdict = match decision {
        Decision::Allow => finish_allow(stand, ready, run_started_at, decided_at)?,
        Decision::Deny => finish_deny(ready)?,
    };
    let verdict = with_extra_failures(verdict, &failures);
    Ok(Verdict {
        detail: format!("{}\n{tab_note}", verdict.detail),
        ..verdict
    })
}

/// Попап обязан спрашивать: вопрос и кнопки в разметке.
fn question_failures(snapshot: &PopupSnapshot) -> Vec<String> {
    let mut failures = Vec::new();
    if snapshot.notice.as_deref() != Some(NEEDS_CONSENT_NOTICE) {
        failures.push(format!(
            "the popup says {:?}, expected '{NEEDS_CONSENT_NOTICE}'",
            snapshot.notice
        ));
    }
    if !snapshot.has_consent_question {
        failures.push("the popup has no Allow / Don't allow buttons".to_owned());
    }
    failures
}

/// Allow: попап закрылся сам, кадры в рендере, файл качает фон, браузер не декодирует.
fn finish_allow(
    stand: &Stand,
    mut ready: ReadyRun<'_, '_>,
    run_started_at: Instant,
    handed_off_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let popup_problem = ready.wait_popup_closed();
    ready.run.tell_page("handed-off");
    let frames = ready.wait_frames()?;
    let page_report = ready.run.finish_page();
    let page_log = ready.run.take_page_log();
    let sorted = sorted_file_requests(stand, run_started_at, handed_off_at);
    let file = Some((
        file_failures(LOCAL_FILE_PAGE, &sorted),
        file_summary(&sorted),
    ));
    let forbidden = ready
        .run
        .player_log
        .snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    let verdict = judge_capture(&CaptureEvidence {
        page_id: LOCAL_PAGE,
        frames,
        hook: None,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
        file,
    });
    Ok(with_extra_failures(
        verdict,
        &popup_problem.into_iter().collect::<Vec<_>>(),
    ))
}

/// Don't allow: сеанса нет, попап называет отказ, файл остался у браузера.
fn finish_deny(mut ready: ReadyRun<'_, '_>) -> Result<Verdict, ScenarioError> {
    let mut failures = Vec::new();
    let shown = match wait_for_settled_popup(ready.run.browser.as_mut(), POPUP_SETTLE_TIMEOUT) {
        Ok(snapshot) => {
            if snapshot.notice.as_deref() != Some(DECLINED_NOTICE) {
                failures.push(format!(
                    "after Don't allow the popup says {:?}, expected '{DECLINED_NOTICE}'",
                    snapshot.notice
                ));
            }
            snapshot.notice
        }
        Err(message) => {
            failures.push(format!("after Don't allow: {message}"));
            None
        }
    };
    ready.run.tell_page("handed-off");
    std::thread::sleep(REFUSAL_WINDOW);
    let report = ready.run.finish_page();
    let page_log = format_browser_log(&ready.run.take_page_log());
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    if host.session_granted {
        failures.push("Don't allow, yet the player granted a session".to_owned());
    }
    failures.extend(page_failures(&report, LOCAL_PAGE));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(notice: Option<&str>, question: bool) -> PopupSnapshot {
        PopupSnapshot {
            notice: notice.map(str::to_owned),
            selectable_rows: Vec::new(),
            static_rows: Vec::new(),
            has_consent_question: question,
            has_reload_button: false,
            focused: None,
        }
    }

    #[test]
    fn each_decision_has_its_own_button_class() {
        assert_eq!(Decision::Allow.button_class(), "consent-allow");
        assert_eq!(Decision::Deny.button_class(), "consent-deny");
    }

    #[test]
    fn the_question_must_be_in_the_popup() {
        assert!(question_failures(&snapshot(Some(NEEDS_CONSENT_NOTICE), true)).is_empty());
        assert_eq!(
            question_failures(&snapshot(Some("failed"), false)).len(),
            2,
            "wrong notice and missing buttons are separate failures"
        );
    }
}
