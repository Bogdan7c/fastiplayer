//! Подготовка стендового перехватчика (`extension/`) к загрузке в браузер.
//!
//! Каталог `extension/` содержит шаблоны с плейсхолдерами. Здесь они подставляются из
//! единого источника правды об «наших» экстеншенах (`browser-bridge-protocol`): ключ
//! манифеста Chrome (ID экстеншена, которому посредник разрешает вызов), gecko id, имя
//! посредника — и из origin главного сайта стенда (адрес журнала).

use std::path::{Path, PathBuf};

use browser_bridge_protocol::extension_identity::{FIREFOX_EXTENSION_ID, NATIVE_HOST_NAME};
use serde_json::Value;

/// Шаблоны и результат подстановки.
const MANIFEST_TEMPLATE: &str = "manifest.template.json";
const MANIFEST_OUTPUT: &str = "manifest.json";
const CONFIG_TEMPLATE: &str = "config.template.js";
const CONFIG_OUTPUT: &str = "config.js";
/// Копия `lib/activation.js` для ISOLATED-мира. Chromium (проверено на 156) не выполняет один и
/// тот же файл во второй записи `content_scripts`, если он уже запущен в MAIN-мире того же
/// фрейма: мост получал пустой `__fpBench` и падал на `resolveActivation is not a function`.
/// Единственный источник кода остаётся один (`lib/activation.js`), копию делает подготовка.
const ACTIVATION_SOURCE: &str = "lib/activation.js";
const ACTIVATION_ISOLATED_COPY: &str = "lib/activation.isolated.js";
/// Тесты Node не нужны браузеру и не копируются.
const SKIPPED_DIRECTORY: &str = "tests";

/// Файл идентичности «наших» экстеншенов; ключ манифеста лежит только в нём (константы нет),
/// поэтому вшиваем файл при сборке — расхождения с протоколом не будет.
const EXTENSION_IDENTITY_JSON: &str =
    include_str!("../../../browser-bridge-protocol/fixtures/extension-identity.json");

#[derive(Debug, thiserror::Error)]
pub enum StandExtensionError {
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("extension-identity.json has no chromium_development.manifest_key")]
    IdentityWithoutKey,
    #[error("extension-identity.json is not valid JSON: {0}")]
    IdentityParse(#[source] serde_json::Error),
    /// После подстановки в файле остался плейсхолдер `@…@` — шаблон и код разошлись.
    #[error("placeholder {placeholder} was not substituted in {file}")]
    PlaceholderLeft { placeholder: String, file: String },
}

/// Подготовленный каталог экстеншена; удаляется вместе с объектом.
pub struct StandExtension {
    // Держим владельца временного каталога, чтобы он жил не дольше прогона.
    directory: tempfile::TempDir,
}

impl StandExtension {
    /// Копирует `extension/` в новый подкаталог `work_root` и подставляет значения.
    pub fn prepare(work_root: &Path, report_origin: &str) -> Result<Self, StandExtensionError> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("extension");
        Self::prepare_from(&source, work_root, report_origin)
    }

    /// То же с явным каталогом-источником (для тестов).
    pub fn prepare_from(
        source: &Path,
        work_root: &Path,
        report_origin: &str,
    ) -> Result<Self, StandExtensionError> {
        std::fs::create_dir_all(work_root).map_err(|error| io("create", work_root, error))?;
        let directory = tempfile::Builder::new()
            .prefix("stand-extension-")
            .tempdir_in(work_root)
            .map_err(|error| io("create a directory in", work_root, error))?;
        copy_tree(source, directory.path())?;
        let activation_copy = directory.path().join(ACTIVATION_ISOLATED_COPY);
        std::fs::copy(source.join(ACTIVATION_SOURCE), &activation_copy)
            .map_err(|error| io("copy activation.js to", &activation_copy, error))?;
        write_substituted(
            source,
            directory.path(),
            MANIFEST_TEMPLATE,
            MANIFEST_OUTPUT,
            &[
                ("@MANIFEST_KEY@", manifest_key()?),
                ("@GECKO_ID@", FIREFOX_EXTENSION_ID.to_owned()),
            ],
        )?;
        write_substituted(
            source,
            directory.path(),
            CONFIG_TEMPLATE,
            CONFIG_OUTPUT,
            &[
                ("@HOST_NAME@", NATIVE_HOST_NAME.to_owned()),
                ("@REPORT_ORIGIN@", report_origin.to_owned()),
            ],
        )?;
        Ok(Self { directory })
    }

    /// Каталог, который загружается как распакованный экстеншен.
    pub fn dir(&self) -> &Path {
        self.directory.path()
    }
}

/// Ключ манифеста Chrome из общего файла идентичности.
fn manifest_key() -> Result<String, StandExtensionError> {
    let identity: Value = serde_json::from_str(EXTENSION_IDENTITY_JSON)
        .map_err(StandExtensionError::IdentityParse)?;
    identity
        .pointer("/chromium_development/manifest_key")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(StandExtensionError::IdentityWithoutKey)
}

fn io(action: &'static str, path: &Path, source: std::io::Error) -> StandExtensionError {
    StandExtensionError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Рекурсивная копия без `tests/` верхнего уровня и без самих шаблонов.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), StandExtensionError> {
    let entries = std::fs::read_dir(source).map_err(|error| io("read", source, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io("read an entry of", source, error))?;
        let name = entry.file_name();
        let target = destination.join(&name);
        let path = entry.path();
        if path.is_dir() {
            if name == SKIPPED_DIRECTORY {
                continue;
            }
            std::fs::create_dir_all(&target).map_err(|error| io("create", &target, error))?;
            copy_tree(&path, &target)?;
        } else if name != MANIFEST_TEMPLATE && name != CONFIG_TEMPLATE {
            std::fs::copy(&path, &target).map_err(|error| io("copy", &path, error))?;
        }
    }
    Ok(())
}

/// Читает шаблон, подставляет значения и пишет результат; остаток `@NAME@` — ошибка.
fn write_substituted(
    source: &Path,
    destination: &Path,
    template: &str,
    output: &str,
    substitutions: &[(&str, String)],
) -> Result<(), StandExtensionError> {
    let template_path = source.join(template);
    let mut text = std::fs::read_to_string(&template_path)
        .map_err(|error| io("read", &template_path, error))?;
    for (placeholder, value) in substitutions {
        text = text.replace(placeholder, value);
    }
    if let Some(left) = find_placeholder(&text) {
        return Err(StandExtensionError::PlaceholderLeft {
            placeholder: left,
            file: template.to_owned(),
        });
    }
    let output_path = destination.join(output);
    std::fs::write(&output_path, text).map_err(|error| io("write", &output_path, error))
}

/// Первый оставшийся плейсхолдер вида `@ИМЯ_ЗАГЛАВНЫМИ@`.
fn find_placeholder(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(start) = rest.find('@') {
        let tail = &rest[start + 1..];
        let name_len = tail
            .find(|symbol: char| !(symbol.is_ascii_uppercase() || symbol == '_'))
            .unwrap_or(tail.len());
        if name_len > 0 && tail[name_len..].starts_with('@') {
            return Some(format!("@{}@", &tail[..name_len]));
        }
        rest = tail;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(origin: &str) -> (tempfile::TempDir, StandExtension) {
        let work = tempfile::tempdir().expect("work dir");
        let extension = StandExtension::prepare(work.path(), origin).expect("prepare");
        (work, extension)
    }

    #[test]
    fn manifest_gets_the_development_key_and_gecko_id() {
        let (_work, extension) = prepared("http://127.0.0.1:4000");
        let text = std::fs::read_to_string(extension.dir().join("manifest.json")).expect("read");
        let manifest: Value = serde_json::from_str(&text).expect("manifest is valid JSON");
        assert_eq!(
            manifest["key"].as_str(),
            Some(manifest_key().unwrap().as_str())
        );
        assert!(manifest["key"].as_str().unwrap().starts_with("MIIB"));
        assert_eq!(
            manifest["browser_specific_settings"]["gecko"]["id"],
            FIREFOX_EXTENSION_ID
        );
    }

    #[test]
    fn config_gets_host_name_and_report_origin() {
        let (_work, extension) = prepared("http://127.0.0.1:4000");
        let config = std::fs::read_to_string(extension.dir().join("config.js")).expect("read");
        assert!(config.contains(&format!("hostName: \"{NATIVE_HOST_NAME}\"")));
        assert!(config.contains("reportOrigin: \"http://127.0.0.1:4000\""));
    }

    #[test]
    fn copy_skips_tests_and_templates_but_keeps_scripts() {
        let (_work, extension) = prepared("http://127.0.0.1:1");
        let dir = extension.dir();
        assert!(!dir.join("tests").exists());
        assert!(!dir.join(MANIFEST_TEMPLATE).exists());
        assert!(!dir.join(CONFIG_TEMPLATE).exists());
        // Копия для ISOLATED-мира совпадает с исходником байт в байт и есть в манифесте.
        assert_eq!(
            std::fs::read(dir.join(ACTIVATION_ISOLATED_COPY)).expect("copy"),
            std::fs::read(dir.join(ACTIVATION_SOURCE)).expect("source")
        );
        let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
        assert!(manifest.contains(ACTIVATION_ISOLATED_COPY));
        for kept in [
            "background.js",
            "page-hook.js",
            "content-bridge.js",
            "lib/frame.js",
        ] {
            assert!(dir.join(kept).is_file(), "{kept} must be copied");
        }
    }

    #[test]
    fn placeholder_finder_ignores_emails_and_reports_leftovers() {
        assert_eq!(find_placeholder("user@example.com and a@b"), None);
        assert_eq!(
            find_placeholder("x \"@HOST_NAME@\" y").as_deref(),
            Some("@HOST_NAME@")
        );
    }

    #[test]
    fn a_template_with_an_unknown_placeholder_fails_preparation() {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(
            source.path().join(MANIFEST_TEMPLATE),
            "{\"k\":\"@MANIFEST_KEY@\",\"x\":\"@NEW_ONE@\"}",
        )
        .expect("write");
        std::fs::write(
            source.path().join(CONFIG_TEMPLATE),
            "@HOST_NAME@@REPORT_ORIGIN@",
        )
        .expect("write");
        std::fs::create_dir_all(source.path().join("lib")).expect("lib dir");
        std::fs::write(source.path().join(ACTIVATION_SOURCE), "// stub").expect("write");
        let work = tempfile::tempdir().expect("work");
        let error = StandExtension::prepare_from(source.path(), work.path(), "http://o")
            .err()
            .expect("must fail");
        assert!(matches!(
            error,
            StandExtensionError::PlaceholderLeft { ref placeholder, .. } if placeholder == "@NEW_ONE@"
        ));
    }
}
