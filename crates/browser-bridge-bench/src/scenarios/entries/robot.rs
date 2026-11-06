//! Робот с настоящими руками: мышь, клавиатура, снимок экрана и окна невидимого kwin.
//!
//! Строка матрицы пишется в терминах «что сделал человек» (`entry.activate(&robot)`,
//! `robot.press(Key::Tab)`), а где лежит значок и как вернуть браузер вперёд, знает только
//! этот модуль.

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use crate::browsers::{BrowserKind, BrowserUnderTest, PopupSnapshot};
use crate::compositor_windows::CompositorWindows;
use crate::desktop_input::{DesktopInput, Key};
use crate::environment::BenchEnvironment;
use crate::screen_capture::ScreenCapture;

/// Шаг опроса попапа.
const POPUP_POLL: Duration = Duration::from_millis(200);
/// Попап, который ещё опрашивает фон: показывает `loading`, и это не итог.
const LOADING_NOTICE: &str = "loading";

/// Руки робота на невидимом экране одного прогона.
pub(super) struct Robot<'env> {
    kind: BrowserKind,
    input: DesktopInput,
    screen: ScreenCapture<'env>,
    windows: CompositorWindows<'env>,
}

impl<'env> Robot<'env> {
    /// Подключается к невидимому экрану. Нужно окружение с правом на настоящий ввод.
    pub(super) fn attach(
        stand: &Stand,
        environment: &'env BenchEnvironment,
        kind: BrowserKind,
    ) -> Result<Self, ScenarioError> {
        let endpoint = environment.desktop_endpoint()?;
        Ok(Self {
            kind,
            input: DesktopInput::connect(&endpoint)?,
            screen: ScreenCapture::new(
                environment,
                &ScreenCapture::icons_dir(&stand.settings.repo_root, kind),
            )?,
            windows: CompositorWindows::new(environment)?,
        })
    }

    /// Вернуть браузер на передний план (окно плеера поверх него забирает фокус).
    pub(super) fn bring_browser_forward(&self) -> Result<(), ScenarioError> {
        self.windows.bring_browser_forward(self.kind)?;
        Ok(())
    }

    /// Класс активного окна: кто сейчас принимает клики и клавиши.
    pub(super) fn active_window_class(&self) -> Result<String, ScenarioError> {
        Ok(self.windows.active_window_class()?)
    }

    /// Настоящий клик по значку экстеншена на панели браузера: найти по снимку и кликнуть.
    /// Перед этим браузер выводится вперёд — иначе клик попал бы в окно плеера.
    pub(super) fn click_toolbar_button(&self) -> Result<(), ScenarioError> {
        self.bring_browser_forward()?;
        let icon = self.screen.find_settled_extension_icon()?;
        self.input.mouse().click_at(icon.center())?;
        // Снимок сразу после клика: при провале он показывает, открыл ли клик попап.
        let _ = self.screen.take_screenshot("after-click");
        Ok(())
    }

    /// Нажимает клавишу (настоящее событие клавиатуры композитора).
    pub(super) fn press(&self, key: Key) -> Result<(), ScenarioError> {
        self.input.keyboard().press(key)?;
        Ok(())
    }

    /// Снимок экрана в каталог прогона (для разбора провала).
    pub(super) fn take_screenshot(&self, label: &str) -> Option<std::path::PathBuf> {
        self.screen.take_screenshot(label).ok()
    }
}

/// Ждёт попап с итоговым сообщением (не `loading`). Ошибка опроса не глотается: последняя
/// попадает в текст провала.
pub(super) fn wait_for_settled_popup(
    browser: &mut dyn BrowserUnderTest,
    timeout: Duration,
) -> Result<PopupSnapshot, String> {
    let deadline = Instant::now() + timeout;
    let mut last_problem;
    loop {
        match browser.popup_snapshot() {
            Ok(Some(snapshot)) => match snapshot.notice.as_deref() {
                Some(notice) if notice != LOADING_NOTICE => return Ok(snapshot),
                _ => last_problem = format!("the popup shows no final message yet: {snapshot:?}"),
            },
            Ok(None) => last_problem = "no popup is open".to_owned(),
            Err(error) => last_problem = format!("popup probe: {error}"),
        }
        if Instant::now() >= deadline {
            return Err(format!("{last_problem} (waited {timeout:?})"));
        }
        std::thread::sleep(POPUP_POLL);
    }
}

/// Ждёт, пока попап закроется сам (`window.close()` после `started`).
pub(super) fn wait_for_popup_closed(
    browser: &mut dyn BrowserUnderTest,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match browser.open_popup_url() {
            Ok(None) => return Ok(()),
            Ok(Some(_)) => {}
            Err(error) => return Err(format!("popup check: {error}")),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the popup did not close by itself within {timeout:?}"
            ));
        }
        std::thread::sleep(POPUP_POLL);
    }
}

/// Итог прогулки по Tab к нужному элементу попапа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TabWalk {
    /// Сколько раз нажат Tab.
    pub(super) presses: usize,
    /// Элемент найден (фокус на нём в последнем снимке).
    pub(super) reached: bool,
    /// Последний снимок попапа (`None`, если попапа уже нет).
    pub(super) last_snapshot: Option<PopupSnapshot>,
}

impl Robot<'_> {
    /// Жмёт Tab по одному, пока снимок попапа не удовлетворит `reached`, но не больше
    /// `max_presses` раз. Число нажатий — само по себе улика контракта фокуса: оно попадает в отчёт.
    /// (Firefox первым Tab отдаёт фокус самому документу попапа, Chromium сразу идёт к кнопке.)
    pub(super) fn press_tab_until(
        &self,
        browser: &mut dyn BrowserUnderTest,
        max_presses: usize,
        reached: impl Fn(&PopupSnapshot) -> bool,
    ) -> Result<TabWalk, ScenarioError> {
        let mut last_snapshot = None;
        for presses in 1..=max_presses {
            self.press(Key::Tab)?;
            last_snapshot = browser.popup_snapshot()?;
            if last_snapshot.as_ref().is_some_and(&reached) {
                return Ok(TabWalk {
                    presses,
                    reached: true,
                    last_snapshot,
                });
            }
        }
        Ok(TabWalk {
            presses: max_presses,
            reached: false,
            last_snapshot,
        })
    }
}
