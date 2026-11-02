//! Файл подключения браузера (native messaging manifest): сборка и разбор.
//!
//! Все значения берутся из `browser_bridge_protocol::extension_identity`: своих
//! констант здесь нет, чтобы установщик, посредник и экстеншен не разошлись.
//! Chromium разрешает экстеншены через `allowed_origins` (`chrome-extension://<ID>/`),
//! Firefox — через `allowed_extensions` (ID как есть).

use std::path::Path;

use browser_bridge_protocol::extension_identity::{
    ALLOWED_EXTENSIONS, BrowserFamily, NATIVE_HOST_NAME,
};
use serde_json::{Value, json};
use thiserror::Error;

/// Человекочитаемая метка в файле подключения.
pub const MANIFEST_DESCRIPTION: &str = "Fastiplayer Bridge native host";

/// Почему файл подключения собрать нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ManifestError {
    /// Путь посредника не UTF-8: JSON его записать не может.
    #[error("The path of the browser helper is not valid UTF-8")]
    HostPathNotUtf8,
}

/// Собирает JSON файла подключения (с отступами и переводом строки в конце).
pub fn build_manifest(family: BrowserFamily, host_path: &Path) -> Result<String, ManifestError> {
    let host_path = host_path.to_str().ok_or(ManifestError::HostPathNotUtf8)?;
    let allowed_ids = || {
        ALLOWED_EXTENSIONS
            .iter()
            .filter(move |allowed| allowed.browser == family)
    };
    let manifest = match family {
        BrowserFamily::Chromium => json!({
            "name": NATIVE_HOST_NAME,
            "description": MANIFEST_DESCRIPTION,
            "path": host_path,
            "type": "stdio",
            "allowed_origins": allowed_ids()
                .map(|allowed| format!("chrome-extension://{}/", allowed.extension_id))
                .collect::<Vec<_>>(),
        }),
        BrowserFamily::Firefox => json!({
            "name": NATIVE_HOST_NAME,
            "description": MANIFEST_DESCRIPTION,
            "path": host_path,
            "type": "stdio",
            "allowed_extensions": allowed_ids()
                .map(|allowed| allowed.extension_id)
                .collect::<Vec<_>>(),
        }),
    };
    // Сериализация `Value` не может завершиться ошибкой: ключи — строки.
    let mut text =
        serde_json::to_string_pretty(&manifest).unwrap_or_else(|_unreachable| manifest.to_string());
    text.push('\n');
    Ok(text)
}

/// Чей файл лежит под нашим именем и отличается ли он от ожидаемого.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestOwnership {
    /// Побайтово то, что записал бы этот плеер.
    Identical,
    /// Наш (имя и путь посредника наши), но содержимое другое: другая версия.
    OursButDiffers,
    /// Корректный JSON, но не наш: другое имя или путь посредника.
    Foreign,
    /// Не JSON (обрезан, мусор).
    Damaged,
}

/// Классифицирует текст файла, найденного под нашим именем.
///
/// «Наш» = `name` равно `NATIVE_HOST_NAME` и `path` указывает на ожидаемое место
/// копии посредника. Файл с таким же именем, но чужим `path`, мы не трогаем.
pub fn classify_manifest(
    existing_text: &str,
    expected_text: &str,
    expected_host_path: &Path,
) -> ManifestOwnership {
    if existing_text == expected_text {
        return ManifestOwnership::Identical;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(existing_text) else {
        return ManifestOwnership::Damaged;
    };
    let is_ours = parsed.get("name").and_then(Value::as_str) == Some(NATIVE_HOST_NAME)
        && parsed.get("path").and_then(Value::as_str) == expected_host_path.to_str();
    if is_ours {
        ManifestOwnership::OursButDiffers
    } else {
        ManifestOwnership::Foreign
    }
}
