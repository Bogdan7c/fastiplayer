//! Строка «введённый текст»: вкладка с текстом в поле, экстеншен появляется позже, попап предлагает
//! перезагрузку, а человек её НЕ нажимает. Доказывает, что без кнопки страницу не трогают:
//! момент рождения документа тот же, текст в поле на месте, сеанса нет.

use std::time::Duration;

use super::super::super::capture::plain_page;
use super::super::super::context::{ScenarioError, Stand};
use super::super::super::evidence::{format_browser_log, page_failures};
use super::super::super::handoff_run::{ExtensionMode, real_extension_dir};
use super::super::super::outcome::Verdict;
use super::super::super::run_tweaks::{RunTweaks, RuntimeInstall};
use super::super::common::{
    EarlyUnsupported, Opened, POPUP_CLOSE_TIMEOUT, ReadyRun, open_ready_with, poll_until,
    with_extra_failures,
};
use super::super::robot::wait_for_popup_closed;
use super::steps::{
    LATE_QUERY, check_nothing_happened_yet, check_page_played_before_install,
    open_popup_expecting_reload_button, read_beats,
};
use crate::browsers::{BrowserKind, ExtensionSource};
use crate::desktop_input::Key;
use crate::environment::BenchEnvironment;
use crate::observe::read_host_evidence;

/// Имя команды страницы «сообщи состояние» (`hook:<имя>`).
pub(super) const REPORT_STATE_HOOK: &str = "report-state";
/// Имя цели поля ввода в зонде страницы.
pub(super) const INPUT_TARGET: &str = "note";
/// Метка строки состояния в журнале страницы.
pub(super) const STATE_LINE_MARK: &str = "late-state ";
/// Слово, которое печатает человек (строчные латинские буквы: набор без Shift).
const TYPED_WORD: &str = "hello";
/// Сколько ждём, пока попап закроется после Escape.
const POPUP_ESCAPE_TIMEOUT: Duration = POPUP_CLOSE_TIMEOUT;
/// Сколько страница стоит после закрытия попапа: за это время любая самовольная перезагрузка успеет.
const OBSERVE_AFTER_CLOSE: Duration = Duration::from_secs(3);
/// Сколько ждём ответ страницы на команду состояния.
const STATE_TIMEOUT: Duration = Duration::from_secs(15);
/// Сколько страница играет до установки.
const PLAY_BEFORE_INSTALL: Duration = Duration::from_secs(3);

/// Состояние страницы из её строки журнала.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageState {
    pub(super) origin: String,
    pub(super) value: String,
}

/// Разбирает `late-state origin=<момент> value=<текст>` (текст — всё после `value=`).
pub(super) fn parse_state_line(line: &str) -> Result<PageState, String> {
    let (_, rest) = line
        .split_once(STATE_LINE_MARK)
        .ok_or_else(|| format!("not a state line: {line}"))?;
    let rest = rest
        .strip_prefix("origin=")
        .ok_or_else(|| format!("state line without origin: {line}"))?;
    let (origin, value) = rest
        .split_once(" value=")
        .ok_or_else(|| format!("state line without value: {line}"))?;
    Ok(PageState {
        origin: origin.to_owned(),
        value: value.to_owned(),
    })
}

/// Просит страницу сообщить состояние и ждёт НОВУЮ строку (старые остаются в журнале).
fn ask_state(ready: &ReadyRun<'_, '_>) -> Result<PageState, String> {
    let known = ready.run.page_log_lines(STATE_LINE_MARK).len();
    ready.run.tell_page(&format!("hook:{REPORT_STATE_HOOK}"));
    let line = poll_until(STATE_TIMEOUT, || {
        ready
            .run
            .page_log_lines(STATE_LINE_MARK)
            .get(known)
            .cloned()
    })
    .ok_or_else(|| format!("the page did not report its state within {STATE_TIMEOUT:?}"))?;
    parse_state_line(&line)
}

/// Человек кликает в поле и печатает слово настоящими клавишами; ждёт, пока текст дойдёт до страницы.
fn type_into_the_field(ready: &ReadyRun<'_, '_>) -> Result<PageState, ScenarioError> {
    let place = ready.locate_page()?;
    let field = place
        .point_in(INPUT_TARGET, 0.5, 0.5)
        .map_err(ScenarioError::EntryNotActivated)?;
    ready.robot.click_screen(field)?;
    let keys = Key::typing(TYPED_WORD).ok_or_else(|| {
        ScenarioError::EntryNotActivated(format!("`{TYPED_WORD}` is not typeable by the robot"))
    })?;
    for key in keys {
        ready.robot.press(key)?;
    }
    let state = ask_state(ready).map_err(ScenarioError::EntryNotActivated)?;
    if state.value != TYPED_WORD {
        return Err(ScenarioError::EntryNotActivated(format!(
            "the typed keys did not reach the field: it holds `{}`",
            state.value
        )));
    }
    Ok(state)
}

/// Прогон строки.
pub(super) fn run(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
) -> Result<Verdict, ScenarioError> {
    let build = real_extension_dir(stand, browser);
    if !build.join("manifest.json").is_file() {
        return Err(ScenarioError::RealExtensionNotBuilt { path: build });
    }
    let tweaks = RunTweaks {
        runtime_install: RuntimeInstall::ByTheRow,
        ..RunTweaks::default()
    };
    let mut ready = match open_ready_with(
        stand,
        environment,
        browser,
        page_id,
        ExtensionMode::InstalledLater,
        &tweaks,
        LATE_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    let mut failures = Vec::new();
    std::thread::sleep(PLAY_BEFORE_INSTALL);
    check_page_played_before_install(&ready, "film", &mut failures);
    let before = type_into_the_field(&ready)?;

    ready
        .run
        .browser
        .install_extension_at_runtime(&ExtensionSource::UnpackedDir(build))?;
    if open_popup_expecting_reload_button(&mut ready, &mut failures)? {
        // Человек видит предложение и закрывает попап, не нажимая: настоящий Escape.
        ready.robot.press(Key::Escape)?;
        if let Err(problem) =
            wait_for_popup_closed(ready.run.browser.as_mut(), POPUP_ESCAPE_TIMEOUT)
        {
            failures.push(problem);
        }
        std::thread::sleep(OBSERVE_AFTER_CLOSE);
    }

    // Состояние ПОСЛЕ: страница та же, текст на месте, сеанса нет, пульс одного рождения.
    match ask_state(&ready) {
        Ok(after) if after == before => {}
        Ok(after) => failures.push(format!(
            "the page changed without the button: before {before:?}, after {after:?}"
        )),
        Err(problem) => failures.push(problem),
    }
    check_nothing_happened_yet(
        &ready,
        "after the popup was closed without pressing",
        &mut failures,
    )?;
    let beats = read_beats(&ready)?;
    let summary = format!(
        "popup offered the reload and was closed with Escape; after {OBSERVE_AFTER_CLOSE:?} the field still holds `{}`, one document birth in {} heartbeats",
        before.value,
        beats.count()
    );

    let page_report = ready.run.finish_page();
    let page_log = format_browser_log(&ready.run.take_page_log());
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    failures.extend(page_failures(&page_report, plain_page(page_id).id));
    if host.session_granted {
        failures.push(
            "the player granted a session although the person never pressed the button".to_owned(),
        );
    }
    Ok(if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        with_extra_failures(Verdict::pass(format!("{summary}\n{page_log}")), &failures)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_line_gives_the_birth_moment_and_the_whole_text() {
        let state =
            parse_state_line("late-typed-text: late-state origin=1730.25 value=hello world")
                .expect("parses");
        assert_eq!(
            state,
            PageState {
                origin: "1730.25".to_owned(),
                value: "hello world".to_owned()
            }
        );
        assert_eq!(
            parse_state_line("x: late-state origin=1.0 value=")
                .expect("empty text")
                .value,
            ""
        );
    }

    #[test]
    fn broken_state_lines_are_errors() {
        assert!(parse_state_line("x: something else").is_err());
        assert!(parse_state_line("x: late-state value=a").is_err());
        assert!(parse_state_line("x: late-state origin=1").is_err());
    }
}
