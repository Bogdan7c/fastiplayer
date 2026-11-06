//! Закрепление значка экстеншена на панели Chromium настройками профиля стенда.
//!
//! По умолчанию Chromium прячет значок экстеншена в меню «пазла», и робот не может по
//! нему кликнуть. Закреплённые экстеншены хранит настройка профиля `extensions.pinned_extensions`
//! (список ID); стенд кладёт её в `Default/Preferences` ДО первого запуска браузера. На
//! обычного пользователя это не влияет: правится только временный профиль стенда.
//! У Firefox закрепление даёт сам манифест (`action.default_area: "navbar"`).

use std::path::Path;

use serde_json::json;

use super::BrowserError;

/// Каталог профиля, который Chromium создаёт для первого пользователя.
const DEFAULT_PROFILE_DIR: &str = "Default";
/// Файл настроек профиля.
const PREFERENCES_FILE: &str = "Preferences";

/// Текст `Preferences` с закреплённым экстеншеном (чистая функция: её проверяет тест).
pub(super) fn preferences_json(extension_id: &str) -> String {
    json!({ "extensions": { "pinned_extensions": [extension_id] } }).to_string()
}

/// Пишет `Default/Preferences` в профиль, если его ещё нет. Уже существующий профиль не
/// перезаписывается: чужие настройки не затираем.
pub(super) fn pin_extension(profile: &Path, extension_id: &str) -> Result<(), BrowserError> {
    let directory = profile.join(DEFAULT_PROFILE_DIR);
    std::fs::create_dir_all(&directory)
        .map_err(|source| BrowserError::io(format!("create {}", directory.display()), source))?;
    let preferences = directory.join(PREFERENCES_FILE);
    if preferences.exists() {
        return Ok(());
    }
    std::fs::write(&preferences, preferences_json(extension_id))
        .map_err(|source| BrowserError::io(format!("write {}", preferences.display()), source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_pinned_list_with_the_extension_id() {
        let profile = tempfile::tempdir().expect("temp profile");
        pin_extension(profile.path(), "abcdefg").expect("pinned");
        let written = std::fs::read_to_string(profile.path().join("Default/Preferences"))
            .expect("preferences exist");
        let parsed: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
        assert_eq!(parsed["extensions"]["pinned_extensions"][0], "abcdefg");
    }

    #[test]
    fn existing_preferences_are_left_alone() {
        let profile = tempfile::tempdir().expect("temp profile");
        let directory = profile.path().join("Default");
        std::fs::create_dir_all(&directory).expect("dir");
        std::fs::write(directory.join("Preferences"), "{\"keep\":true}").expect("seed");
        pin_extension(profile.path(), "abcdefg").expect("no-op");
        let written = std::fs::read_to_string(directory.join("Preferences")).expect("read");
        assert_eq!(written, "{\"keep\":true}");
    }
}
