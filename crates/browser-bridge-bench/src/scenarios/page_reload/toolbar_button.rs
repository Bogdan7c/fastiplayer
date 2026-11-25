//! Ход строки `other-site-paused-button` (бэклог п. 75): окно плеера на ПАУЗЕ, вкладка ушла на другой
//! сайт, зритель жмёт значок моста на новой странице.
//!
//! Цепочка: пауза в окне → страница уходит на другой origin (сеанс не кончается: плеер держит связь,
//! «сирота») → новый документ играет сам браузер → настоящий клик по значку на панели браузера →
//! попап обязан отдать главное видео новой страницы, и оно встаёт в то же окно заменой сеанса
//! (`grant=ReplacedPrevious`), попап закрывается как при обычном `started`. Старый экстеншен вместо
//! этого показывает заметку `in_player` и видео не отдаёт — приговор называет именно это.

use std::time::Duration;

use super::judge::{IN_PLAYER_NOTICE, PopupObservation, REPLACED_PREVIOUS_GRANT};
use super::{
    NAVIGATE_HOOK, REPLACEMENT_TIMEOUT, RowOutcome, wait_browser_plays_by_itself,
    wait_frames_grow_after_replacement,
};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::player_control::PlayerControl;
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::entries::robot::{Robot, wait_for_settled_popup};
use crate::scenarios::evidence::{PAGE_LINK_CHANGED_LINE, PAGE_LINK_LEFT};
use crate::scenarios::handoff_run::{ACK_TIMEOUT, HandoffRun};

/// Сколько окно стоит на паузе до ухода страницы: часы плеера успевают сказать «пауза»
/// (то же значение, что в `tab_return`).
const PAUSE_SETTLE: Duration = Duration::from_secs(1);
/// Сколько плеер может обдумывать, что страница ушла: журнал должен показать `change=Left`.
const PAGE_LEFT_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждём итоговое сообщение попапа после клика (не `loading`).
const POPUP_NOTICE_TIMEOUT: Duration = Duration::from_secs(10);
/// Заметка попапа «видео играет в уже открытом окне плеера — переключитесь в него» (`data-notice`).
const SWITCH_TO_PLAYER_NOTICE: &str = "switch_to_player";
/// Сколько ждём, что окно останется управляемым после замены.
const WINDOW_CONTROLLABLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Весь ход строки после передачи. Провал шага пишется в `failed_steps`; ошибка самого стенда
/// (нет мыши, нет экрана) уходит наружу как `ScenarioError`.
pub(super) fn walk(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    failed_steps: &mut Vec<String>,
) -> Result<RowOutcome, ScenarioError> {
    let mut outcome = RowOutcome::default();
    let robot = Robot::attach(stand, environment, browser_kind)?;
    if let Err(reason) = leave_window_paused_on_other_site(run, control) {
        failed_steps.push(reason);
        return Ok(outcome);
    }
    // Клик зрителя по значку на новой странице: попап читаем сразу после него.
    if let Err(error) = robot.click_toolbar_button() {
        failed_steps.push(format!("toolbar-click: {error}"));
        return Ok(outcome);
    }
    let popup = observe_popup(run);
    let offered_nothing =
        matches!(&popup, PopupObservation::Notice(notice) if notice == IN_PLAYER_NOTICE);
    outcome.popup = Some(popup);
    if offered_nothing {
        // Дефект подтверждён самим попапом: замены не будет, 50 с её ожидания ничего не добавят.
        return Ok(outcome);
    }
    follow_replacement(run, control, &mut outcome, failed_steps);
    Ok(outcome)
}

/// Пауза в окне, уход страницы на другой сайт, сеанс остаётся «сиротой», новый документ играет сам.
fn leave_window_paused_on_other_site(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
) -> Result<(), String> {
    control
        .pause()
        .map_err(|error| format!("pause-window: {error}"))?;
    std::thread::sleep(PAUSE_SETTLE);
    run.player_log
        .skip_history()
        .map_err(|error| format!("skip-history: {error}"))?;
    run.tell_page(NAVIGATE_HOOK);
    run.wait_ack(NAVIGATE_HOOK, ACK_TIMEOUT)
        .map_err(|reason| format!("page-acknowledged-leaving: {reason}"))?;
    let link_line = run
        .player_log
        .wait_for_line(PAGE_LINK_CHANGED_LINE, PAGE_LEFT_TIMEOUT)
        .map_err(|error| format!("the player never heard that the page left: {error}"))?;
    if !link_line.contains(PAGE_LINK_LEFT) {
        return Err(format!(
            "the page link changed, but not to «left»: {}",
            link_line.trim()
        ));
    }
    wait_browser_plays_by_itself(run)
        .map_err(|reason| format!("browser-plays-other-site: {reason}"))
}

/// Читает попап после клика: итоговое сообщение либо причина, почему его нет.
fn observe_popup(run: &mut HandoffRun<'_>) -> PopupObservation {
    match wait_for_settled_popup(run.browser.as_mut(), POPUP_NOTICE_TIMEOUT) {
        Ok(snapshot) => PopupObservation::Notice(snapshot.notice.unwrap_or_default()),
        Err(reason) => PopupObservation::NotSeen(reason),
    }
}

/// Замена: `ReplacedPrevious`, попап остался и зовёт в окно, кадры растут, окно управляемо.
fn follow_replacement(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    outcome: &mut RowOutcome,
    failed_steps: &mut Vec<String>,
) {
    if let Err(error) = run
        .player_log
        .wait_for_line(REPLACED_PREVIOUS_GRANT, REPLACEMENT_TIMEOUT)
    {
        failed_steps.push(format!("replacement-session-opened: {error}"));
        return;
    }
    // Окно плеера уже было открыто, и само выйти вперёд оно не может (решение 216): попап НЕ
    // закрывается, а остаётся и зовёт переключиться. Закрылся бы — значит, открылось новое окно.
    match wait_for_settled_popup(run.browser.as_mut(), POPUP_NOTICE_TIMEOUT) {
        Ok(snapshot) if snapshot.notice.as_deref() == Some(SWITCH_TO_PLAYER_NOTICE) => {}
        Ok(snapshot) => failed_steps.push(format!(
            "popup-tells-to-switch: the popup said {:?}, expected `{SWITCH_TO_PLAYER_NOTICE}`",
            snapshot.notice
        )),
        Err(reason) => failed_steps.push(format!("popup-tells-to-switch: {reason}")),
    }
    outcome.frame_growth = Some(wait_frames_grow_after_replacement(run));
    if let Err(error) = control.wait_until_controllable(WINDOW_CONTROLLABLE_TIMEOUT) {
        failed_steps.push(format!("window-still-controllable: {error}"));
    }
}
