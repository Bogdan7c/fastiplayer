//! Чтение профилей Firefox-семейства: `profiles.ini` и `extensions.json`.
//!
//! `profiles.ini` перечисляет профили секциями `[Profile0]`, `[Profile1]`, ... с
//! ключами `Path=` и `IsRelative=` (1 — путь от каталога с `profiles.ini`, 0 или
//! ключа нет — абсолютный). В `extensions.json` профиля лежит массив `addons`, где наш
//! экстеншен узнаётся по gecko-ID.

use std::path::{Path, PathBuf};

use browser_bridge_protocol::extension_identity::FIREFOX_EXTENSION_ID;
use serde_json::Value;

use super::{Presence, ReadEvidence, Unreadable, read_bounded, read_json};

/// Просматривает профили всех корней и возвращает лучшее состояние нашего экстеншена.
///
/// Корень, у которого не читается `profiles.ini`, и профиль с битым `extensions.json`
/// не отравляют остальные: ошибка возвращается, только если не прочиталось ничего
/// (`ReadEvidence`).
pub fn read_firefox_presence(profile_roots: &[PathBuf]) -> Result<Presence, Unreadable> {
    let mut presence = Presence::Absent;
    let mut evidence = ReadEvidence::default();
    for root in profile_roots {
        let profiles = match profile_directories(root) {
            Ok(profiles) => profiles,
            Err(failure) => {
                evidence.record_failure(failure);
                continue;
            }
        };
        for profile in profiles {
            if let Some(extensions) = evidence.record(read_json(&profile.join("extensions.json"))) {
                presence = presence.strongest(presence_in(&extensions));
            }
        }
    }
    evidence.finish()?;
    Ok(presence)
}

/// Профили из `profiles.ini` корня. Нет файла — у корня нет профилей.
fn profile_directories(root: &Path) -> Result<Vec<PathBuf>, Unreadable> {
    let Some(bytes) = read_bounded(&root.join("profiles.ini"))? else {
        return Ok(Vec::new());
    };
    Ok(parse_profiles_ini(&String::from_utf8_lossy(&bytes))
        .into_iter()
        .map(|profile| {
            if profile.is_relative {
                root.join(profile.path)
            } else {
                PathBuf::from(profile.path)
            }
        })
        .collect())
}

/// Одна запись `[Profile<число>]`.
#[derive(Debug, PartialEq, Eq)]
struct IniProfile {
    path: String,
    is_relative: bool,
}

/// `Profile0`, `Profile12`, ...: только такие секции — профили. Остальные
/// (`[General]`, `[Install...]`, `[ProfileGroups]`-подобные) профилями не считаются.
fn is_profile_section(section: &str) -> bool {
    section
        .strip_prefix("Profile")
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// Крошечный разбор INI: секции `[Profile<число>]`, строки `ключ=значение`
/// (ключ и значение обрезаются от пробелов). Остальные секции и комментарии
/// пропускаются.
///
/// `IsRelative` не указан — путь абсолютный (так Firefox трактует отсутствие ключа).
fn parse_profiles_ini(text: &str) -> Vec<IniProfile> {
    let mut profiles = Vec::new();
    let mut current: Option<(Option<String>, bool)> = None;
    let mut finish = |current: Option<(Option<String>, bool)>| {
        if let Some((Some(path), is_relative)) = current {
            profiles.push(IniProfile { path, is_relative });
        }
    };
    for line in text.lines().map(str::trim) {
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            finish(current.take());
            if is_profile_section(section.trim()) {
                current = Some((None, false));
            }
        } else if let (Some((path, is_relative)), Some((key, value))) =
            (current.as_mut(), line.split_once('='))
        {
            let value = value.trim();
            match key.trim() {
                "Path" if !value.is_empty() => *path = Some(value.to_owned()),
                "IsRelative" => *is_relative = value == "1",
                _ => {}
            }
        }
    }
    finish(current.take());
    profiles
}

fn presence_in(extensions: &Value) -> Presence {
    let Some(addons) = extensions.get("addons").and_then(Value::as_array) else {
        return Presence::Absent;
    };
    addons
        .iter()
        .filter(|addon| addon.get("id").and_then(Value::as_str) == Some(FIREFOX_EXTENSION_ID))
        .map(|addon| {
            let flag = |name: &str| addon.get(name).and_then(Value::as_bool).unwrap_or(false);
            let is_active = addon.get("active").and_then(Value::as_bool).unwrap_or(true);
            if is_active && !flag("userDisabled") && !flag("appDisabled") && !flag("softDisabled") {
                Presence::Enabled
            } else {
                Presence::Disabled
            }
        })
        .fold(Presence::Absent, Presence::strongest)
}
