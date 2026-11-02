//! Общие помощники тестов установщика экстеншена: фейковый запуск браузера,
//! профили Chromium/Firefox на фейковом диске.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::support::{FakeFlatpak, Fixture, write_with_parents};
use crate::{
    BrowserLaunchCommand, BrowserLauncher, ExtensionInstaller, ExtensionListings, LaunchError,
    Listing,
};
use browser_bridge_protocol::extension_identity::{
    CHROMIUM_DEVELOPMENT_EXTENSION_ID, FIREFOX_EXTENSION_ID,
};

/// ID «опубликованного» экстеншена в тестах: 32 буквы a–p.
pub(super) const STORE_ID: &str = "abcdefghijklmnopabcdefghijklmnop";
/// slug «опубликованного» экстеншена на AMO.
pub(super) const ADD_ONS_SLUG: &str = "fastiplayer-bridge";
/// Адрес сервера обновлений, который обязан лежать в подсказке.
pub(super) const UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

/// Что запросили у фейкового запускателя.
pub(super) type LaunchCall = (BrowserLaunchCommand, String);

/// Фейковый запуск браузера: записывает вызовы, может отказать.
#[derive(Clone, Default)]
pub(super) struct RecordingLauncher {
    calls: Arc<Mutex<Vec<LaunchCall>>>,
    failure: Arc<Mutex<Option<LaunchError>>>,
}

impl RecordingLauncher {
    pub fn failing_with(error: LaunchError) -> Self {
        let launcher = Self::default();
        *launcher.failure.lock().expect("failure") = Some(error);
        launcher
    }

    pub fn calls(&self) -> Vec<LaunchCall> {
        self.calls.lock().expect("calls").clone()
    }
}

impl BrowserLauncher for RecordingLauncher {
    fn open_url(&self, command: &BrowserLaunchCommand, url: &str) -> Result<(), LaunchError> {
        if let Some(error) = *self.failure.lock().expect("failure") {
            return Err(error);
        }
        self.calls
            .lock()
            .expect("calls")
            .push((command.clone(), url.to_owned()));
        Ok(())
    }
}

/// Обе страницы опубликованы.
pub(super) fn published_listings() -> ExtensionListings {
    ExtensionListings::new(
        Listing::Published(STORE_ID.to_owned()),
        Listing::Published(ADD_ONS_SLUG.to_owned()),
    )
    .expect("valid listings")
}

impl Fixture {
    /// Установщик экстеншена с фейками.
    pub fn extension_installer(
        &self,
        listings: ExtensionListings,
        flatpak: &FakeFlatpak,
        launcher: &RecordingLauncher,
    ) -> ExtensionInstaller {
        ExtensionInstaller::new(
            self.environment.clone(),
            listings,
            Box::new(flatpak.clone()),
            Box::new(launcher.clone()),
        )
    }
}

/// Файл подсказки по независимой таблице: HOME + путь профилей + `External Extensions`.
pub(super) fn expected_hint_path(fixture: &Fixture, user_data_relative: &str) -> PathBuf {
    fixture
        .home
        .join(user_data_relative)
        .join("External Extensions")
        .join(format!("{STORE_ID}.json"))
}

/// Профили Chromium-браузера: `<user_data>/<profile>/<file>` с JSON внутри.
pub(super) fn write_chromium_preferences(
    user_data: &Path,
    profile: &str,
    file_name: &str,
    contents: &Value,
) -> PathBuf {
    let path = user_data.join(profile).join(file_name);
    write_with_parents(&path, &contents.to_string());
    path
}

/// `Preferences` с установленным экстеншеном; `disable_reasons` задаёт включённость.
pub(super) fn installed_preferences(extension_id: &str, disable_reasons: &Value) -> Value {
    json!({"extensions": {"settings": {extension_id: {
        "location": 1, "state": 1, "disable_reasons": disable_reasons
    }}}})
}

/// `Preferences` после удаления человеком: пустая запись и `external_uninstalls`.
pub(super) fn removed_by_user_preferences(extension_id: &str) -> Value {
    json!({"extensions": {
        "settings": {extension_id: {}},
        "external_uninstalls": [extension_id]
    }})
}

/// Профиль Firefox: `extensions.json` с нашим экстеншеном и заданными флагами.
pub(super) fn write_firefox_extensions(profile: &Path, extra_fields: &Value) -> PathBuf {
    let mut addon = json!({"id": FIREFOX_EXTENSION_ID, "active": true, "userDisabled": false});
    if let (Some(addon), Some(extra)) = (addon.as_object_mut(), extra_fields.as_object()) {
        addon.extend(extra.clone());
    }
    let path = profile.join("extensions.json");
    let other = json!({"id": "other@example.org", "active": true});
    write_with_parents(&path, &json!({"addons": [other, addon]}).to_string());
    path
}

/// Идентификатор разработки: распакованный экстеншен тоже наш.
pub(super) const DEVELOPMENT_ID: &str = CHROMIUM_DEVELOPMENT_EXTENSION_ID;

/// Создаёт FIFO через утилиту `mkfifo` (своих `unsafe` и `libc` в крейте нет).
pub(super) fn make_fifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo is available");
    assert!(status.success(), "mkfifo failed");
}
