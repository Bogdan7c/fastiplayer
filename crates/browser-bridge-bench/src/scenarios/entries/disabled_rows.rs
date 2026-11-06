//! Строки «выключенный в настройках вход не показывается»: пункт меню видео и пункт меню ссылки.
//! Настройка меняется У РАБОТАЮЩЕГО браузера (без перезапуска): пункт обязан исчезнуть и вернуться.
//!
//! Доказательство «пункта нет» честно только с положительным контролем в той же строке: сначала то
//! же меню с включённым входом показывает пункт (значит, поиск значка видит пункты), потом вход
//! выключен и меню (оно открылось: пиксели нарисовались) пункта не показывает, потом вход включён
//! снова и пункт вернулся.

use super::super::context::{ScenarioError, Stand};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{DEFAULT_QUERY, EarlyUnsupported, Opened, ReadyRun, open_ready};
use super::extension_probe::{EntrySettingsSetup, write_entry_settings};
use super::menu_entry::ClickSpot;
use super::native_menu::{MenuLook, dismiss_menu};
use super::start_rows::prepare_entry;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::observe::read_host_evidence;
use std::time::Duration;

/// Сколько даём экстеншену применить настройку (`storage.onChanged` -> `contextMenus.update`).
const SETTING_APPLY_WAIT: Duration = Duration::from_millis(1500);

/// Какой вход выключаем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DisabledRow {
    /// Пункт на видео (и пункт страницы): настройка `video_menu`.
    VideoItem,
    /// Пункт на ссылке: настройка `link_menu`.
    LinkItem,
}

impl DisabledRow {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::VideoItem => "disabled-video-item-hidden",
            Self::LinkItem => "disabled-link-item-hidden",
        }
    }

    pub(super) const fn page_id(self) -> &'static str {
        match self {
            Self::VideoItem => "mse-fmp4",
            Self::LinkItem => "link-source",
        }
    }

    pub(super) fn entry(self) -> Entry {
        match self {
            Self::VideoItem => Entry::VideoMenu,
            Self::LinkItem => Entry::LinkMenu,
        }
    }

    fn spot(self) -> ClickSpot {
        match self {
            Self::VideoItem => ClickSpot::VIDEO,
            Self::LinkItem => ClickSpot::center_of("link:autoplay"),
        }
    }

    /// Настройки с выключенным этим входом.
    fn disabled(self) -> EntrySettingsSetup {
        match self {
            Self::VideoItem => EntrySettingsSetup {
                video_menu: false,
                ..EntrySettingsSetup::DEFAULT
            },
            Self::LinkItem => EntrySettingsSetup {
                link_menu: false,
                ..EntrySettingsSetup::DEFAULT
            },
        }
    }
}

/// Одно «смотрю меню»: что ожидалось и что увидели.
fn look_and_dismiss(ready: &ReadyRun<'_, '_>, spot: ClickSpot) -> Result<MenuLook, ScenarioError> {
    let look = ready.right_click_and_look(spot)?;
    dismiss_menu(&ready.robot)?;
    Ok(look)
}

pub(super) fn run_disabled(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: DisabledRow,
) -> Result<Verdict, ScenarioError> {
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        row.page_id(),
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    prepare_entry(&mut ready, row.entry())?;
    let spot = row.spot();
    let mut failures = Vec::new();

    let enabled = look_and_dismiss(&ready, spot)?;
    if !matches!(enabled, MenuLook::ItemShown { .. }) {
        failures.push(format!(
            "control: with the entry enabled the menu must show the item, got {enabled:?}"
        ));
    }
    write_entry_settings(ready.run.browser.as_mut(), row.disabled())
        .map_err(ScenarioError::EntryNotActivated)?;
    std::thread::sleep(SETTING_APPLY_WAIT);
    let disabled = look_and_dismiss(&ready, spot)?;
    if !matches!(disabled, MenuLook::ItemAbsent { .. }) {
        failures.push(format!(
            "with the entry disabled the open menu must have no item, got {disabled:?}"
        ));
    }
    write_entry_settings(ready.run.browser.as_mut(), EntrySettingsSetup::DEFAULT)
        .map_err(ScenarioError::EntryNotActivated)?;
    std::thread::sleep(SETTING_APPLY_WAIT);
    let restored = look_and_dismiss(&ready, spot)?;
    if !matches!(restored, MenuLook::ItemShown { .. }) {
        failures.push(format!(
            "after enabling it again (no browser restart) the item must return, got {restored:?}"
        ));
    }

    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    if host.session_granted {
        failures
            .push("the player granted a session although the row only looked at menus".to_owned());
    }
    let summary =
        format!("enabled: {enabled:?}; disabled: {disabled:?}; enabled again: {restored:?}");
    Ok(if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        Verdict::fail(format!("{}\n{summary}", failures.join("; ")))
    })
}
