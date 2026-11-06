//! Движения робота, общие для строк позднего входа: попап с «Reload and open», кнопка клавишами,
//! пульс страницы, ожидание перезагрузки. Каждый шаг не прячет провал: либо кладёт причину в
//! `failures`, либо возвращает ошибку стенда, когда рука не смогла сделать движение.

use std::time::Duration;

use super::super::super::context::ScenarioError;
use super::super::common::{POPUP_SETTLE_TIMEOUT, ReadyRun, poll_until};
use super::super::robot::wait_for_settled_popup;
use super::beat::{BEAT_LINE_MARK, PageBeats};
use crate::desktop_input::Key;
use crate::observe::read_host_evidence;

/// Адрес страницы строки: режим передачи плюс пульс страницы (`site/lib/entry-probe.js`).
pub(super) const LATE_QUERY: &str = "&mode=handoff&origin_beat=1";
/// Случай попапа, когда вкладка открыта раньше нашего скрипта.
const RELOAD_NEEDED_NOTICE: &str = "reload_needed";
/// Класс кнопки «Reload and open».
const RELOAD_BUTTON_CLASS: &str = "reload-and-open";
/// Сколько раз можно нажать Tab, прежде чем счесть, что кнопки в порядке фокуса нет.
/// (Firefox первым Tab отдаёт фокус документу попапа, потом идут вопросы и кнопка.)
const MAX_TAB_PRESSES: usize = 12;
/// Сколько ждём, пока страница вернётся в ролик после установки/обновления: пульс должен идти.
const BEAT_TIMEOUT: Duration = Duration::from_secs(20);
/// Сколько ждём новую страницу после нажатия кнопки (перезагрузка, ожидание фона).
pub(super) const RELOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Минимум секунд ролика, который страница обязана сыграть до установки: иначе строка ничего не
/// доказывает («та же секунда» от нуля неотличима от «с начала»).
const MIN_PLAYED_BEFORE_INSTALL_S: f64 = 3.0;

/// Все пульсы страницы на сейчас.
pub(super) fn read_beats(ready: &ReadyRun<'_, '_>) -> Result<PageBeats, ScenarioError> {
    PageBeats::from_log(&ready.run.page_log_lines(BEAT_LINE_MARK))
        .map_err(ScenarioError::EntryNotActivated)
}

/// Выдал ли плеер пропуск на сеанс (посредник написал `session_granted`).
pub(super) fn session_started(ready: &ReadyRun<'_, '_>) -> Result<bool, ScenarioError> {
    Ok(read_host_evidence(&ready.run.stderr_log)?.session_granted)
}

/// Ждёт, пока видео `video` страницы сыграет не меньше `seconds` и будет играть (не пауза).
/// Это предусловие строки: без него «позднее» нечему быть поздним.
pub(super) fn wait_until_played(
    ready: &ReadyRun<'_, '_>,
    video: &str,
    seconds: f64,
    timeout: Duration,
) -> Result<f64, String> {
    poll_until(timeout, || {
        let beats = read_beats(ready).ok()?;
        let found = beats.latest()?.video(video)?;
        (found.seconds >= seconds && !found.paused).then_some(found.seconds)
    })
    .ok_or_else(|| {
        format!("the video `{video}` did not play {seconds} s before the extension appeared (autoplay blocked or the page is dead)")
    })
}

/// Предусловие: страница сыграла достаточно и её пульс идёт.
pub(super) fn check_page_played_before_install(
    ready: &ReadyRun<'_, '_>,
    video: &str,
    failures: &mut Vec<String>,
) {
    if let Err(problem) = wait_until_played(ready, video, MIN_PLAYED_BEFORE_INSTALL_S, BEAT_TIMEOUT)
    {
        failures.push(problem);
    }
}

/// Человек кликает по значку: попап обязан показать «нужна перезагрузка» и кнопку.
/// `Ok(true)` — показал; `Ok(false)` — показал другое (причина в `failures`).
pub(super) fn open_popup_expecting_reload_button(
    ready: &mut ReadyRun<'_, '_>,
    failures: &mut Vec<String>,
) -> Result<bool, ScenarioError> {
    ready.robot.click_toolbar_button()?;
    match wait_for_settled_popup(ready.run.browser.as_mut(), POPUP_SETTLE_TIMEOUT) {
        Ok(snapshot)
            if snapshot.notice.as_deref() == Some(RELOAD_NEEDED_NOTICE)
                && snapshot.has_reload_button =>
        {
            Ok(true)
        }
        Ok(snapshot) => {
            failures.push(format!(
                "the popup must offer `{RELOAD_NEEDED_NOTICE}` with a Reload and open button, it shows: {snapshot:?}"
            ));
            Ok(false)
        }
        Err(problem) => {
            failures.push(problem);
            Ok(false)
        }
    }
}

/// Страницу не перезагружали (одно рождение документа) и сеанса нет: так выглядит попап, пока
/// человек ещё ничего не нажал. `moment` называет, когда проверка, для текста провала.
pub(super) fn check_nothing_happened_yet(
    ready: &ReadyRun<'_, '_>,
    moment: &str,
    failures: &mut Vec<String>,
) -> Result<(), ScenarioError> {
    let beats = read_beats(ready)?;
    let origins = beats.origins().len();
    if origins != 1 {
        failures.push(format!(
            "the page was reloaded by itself {moment}: {origins} document births in its heartbeat"
        ));
    }
    if session_started(ready)? {
        failures.push(format!(
            "a player session exists {moment}, before the person pressed anything"
        ));
    }
    Ok(())
}

/// Tab до кнопки «Reload and open» и Enter. Возвращает, сколько раз нажат Tab (улика порядка
/// фокуса); `None` — кнопка в порядке фокуса не нашлась (причина в `failures`).
pub(super) fn press_reload_button(
    ready: &mut ReadyRun<'_, '_>,
    failures: &mut Vec<String>,
) -> Result<Option<usize>, ScenarioError> {
    let walk =
        ready
            .robot
            .press_tab_until(ready.run.browser.as_mut(), MAX_TAB_PRESSES, |snapshot| {
                snapshot.focus_is_button_with_class(RELOAD_BUTTON_CLASS)
            })?;
    if !walk.reached {
        failures.push(format!(
            "Tab did not reach the Reload and open button in {MAX_TAB_PRESSES} presses; last popup: {:?}",
            walk.last_snapshot
        ));
        return Ok(None);
    }
    ready.robot.press(Key::Enter)?;
    Ok(Some(walk.presses))
}

/// Ждёт, пока вкладка родится заново: в пульсе появился второй момент рождения.
pub(super) fn wait_for_reload(ready: &ReadyRun<'_, '_>) -> Result<(), String> {
    poll_until(RELOAD_TIMEOUT, || {
        let beats = read_beats(ready).ok()?;
        (beats.origins().len() >= 2).then_some(())
    })
    .ok_or_else(|| {
        format!("the page was not reloaded within {RELOAD_TIMEOUT:?} after the button press")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::entries::common::DEFAULT_QUERY;

    #[test]
    fn the_late_query_is_the_ordinary_handoff_query_plus_the_page_heartbeat() {
        assert!(LATE_QUERY.starts_with(DEFAULT_QUERY));
        assert!(LATE_QUERY.ends_with("&origin_beat=1"));
    }
}
