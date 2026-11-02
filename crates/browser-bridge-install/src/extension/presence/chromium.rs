//! Чтение профилей Chromium-браузера: `Preferences` и `Secure Preferences`.
//!
//! Формат (проверено вживую на Chromium 156 и Chrome 156):
//! - `extensions.settings.<ID>` — непустой объект, если экстеншен стоит; пустой
//!   объект `{}` остаётся после удаления человеком;
//! - `disable_reasons` пуст (или 0) — включён, иначе отключён;
//! - `extensions.external_uninstalls` — ID внешне установленных экстеншенов,
//!   которые человек удалил: по такой подсказке браузер больше ничего не ставит.

use std::path::Path;

use serde_json::Value;

use super::{Presence, ReadEvidence, Unreadable, read_json};

/// Файлы настроек профиля, где браузер хранит записи об экстеншенах.
const PREFERENCE_FILE_NAMES: [&str; 2] = ["Preferences", "Secure Preferences"];

/// Какие ID считаются «нашими» в Chromium-профиле.
#[derive(Debug, Clone, Copy)]
pub struct ChromiumExtensionIds<'a> {
    /// ID из Chrome Web Store, если экстеншен опубликован.
    pub store: Option<&'a str>,
    /// ID сборки разработчика: распакованная копия тоже считается установленной.
    pub development: &'a str,
}

/// Удалил ли человек экстеншен, поставленный по подсказке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserUninstall {
    /// Следов удаления нет.
    NotRecorded,
    /// ID магазина записан в `external_uninstalls`: тихий путь больше не работает.
    Recorded,
}

/// Итог чтения всех профилей одного браузера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromiumPresence {
    /// Лучшее состояние по всем профилям.
    pub presence: Presence,
    /// Есть ли запись об удалении человеком хотя бы в одном профиле.
    pub user_uninstall: UserUninstall,
}

/// Сканирует профили `Default` и `Profile <число>` в `user_data`.
///
/// Каждый файл оценивается независимо; ошибка возвращается, только если все
/// существующие файлы профилей не прочитались (`ReadEvidence`).
pub fn read_chromium_presence(
    user_data: &Path,
    ids: ChromiumExtensionIds<'_>,
) -> Result<ChromiumPresence, Unreadable> {
    let mut result = ChromiumPresence {
        presence: Presence::Absent,
        user_uninstall: UserUninstall::NotRecorded,
    };
    let mut evidence = ReadEvidence::default();
    for profile_directory in profile_directories(user_data, &mut evidence)? {
        for file_name in PREFERENCE_FILE_NAMES {
            let Some(preferences) = evidence.record(read_json(&profile_directory.join(file_name)))
            else {
                continue;
            };
            result.presence = result.presence.strongest(presence_in(&preferences, &ids));
            if is_uninstalled_by_user(&preferences, ids.store) {
                result.user_uninstall = UserUninstall::Recorded;
            }
        }
    }
    evidence.finish()?;
    Ok(result)
}

/// Подкаталоги профилей. Нет `user_data` — профилей нет (браузер не запускался).
/// Не читается сам каталог — это общая ошибка; не читается отдельная запись —
/// сбой копится в `evidence`.
fn profile_directories(
    user_data: &Path,
    evidence: &mut ReadEvidence,
) -> Result<Vec<std::path::PathBuf>, Unreadable> {
    let entries = match std::fs::read_dir(user_data) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Unreadable::io(user_data, &error)),
    };
    let mut directories = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                evidence.record_failure(Unreadable::io(user_data, &error));
                continue;
            }
        };
        let name = entry.file_name();
        if is_profile_name(&name.to_string_lossy()) && entry.path().is_dir() {
            directories.push(entry.path());
        }
    }
    // Порядок каталога случаен; сортировка делает результат и ошибки воспроизводимыми.
    directories.sort();
    Ok(directories)
}

/// `Default` или `Profile <число>`: так Chromium называет каталоги профилей.
fn is_profile_name(name: &str) -> bool {
    name == "Default"
        || name
            .strip_prefix("Profile ")
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

fn presence_in(preferences: &Value, ids: &ChromiumExtensionIds<'_>) -> Presence {
    let settings = preferences.pointer("/extensions/settings");
    ids.store
        .into_iter()
        .chain(std::iter::once(ids.development))
        .filter_map(|id| settings.and_then(|settings| settings.get(id)))
        .map(presence_of_entry)
        .fold(Presence::Absent, Presence::strongest)
}

/// Пустой объект (`{}`) — след удаления, не установка.
fn presence_of_entry(entry: &Value) -> Presence {
    let Some(fields) = entry.as_object().filter(|fields| !fields.is_empty()) else {
        return Presence::Absent;
    };
    let is_disabled = match fields.get("disable_reasons") {
        Some(Value::Array(reasons)) => !reasons.is_empty(),
        Some(Value::Number(bitmask)) => bitmask.as_i64() != Some(0),
        _ => false,
    };
    if is_disabled {
        Presence::Disabled
    } else {
        Presence::Enabled
    }
}

fn is_uninstalled_by_user(preferences: &Value, store_id: Option<&str>) -> bool {
    let Some(store_id) = store_id else {
        return false;
    };
    preferences
        .pointer("/extensions/external_uninstalls")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(store_id)))
}
