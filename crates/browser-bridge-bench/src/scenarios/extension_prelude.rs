//! Сценарий «чужой код раньше нашего скрипта»: копия сборки экстеншена с prelude-скриптом.
//!
//! На настоящих сайтах (YouTube с блокировщиком) чужой код подменяет браузерные классы РАНЬШЕ
//! нашего MAIN-скрипта `page.js`. Чтобы это было детерминированно в обоих браузерах, стенд копирует
//! сборку в каталог прогона и вставляет в `content_scripts` копии манифеста ещё один элемент
//! (`world: MAIN`, `document_start`) ПЕРЕД элементом с `page.js`: скрипты одного мира и одной стадии
//! исполняются в порядке манифеста. Настоящая сборка (и снимок из `FASTIPLAYER_BENCH_EXTENSION_DIST`)
//! не меняется.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::context::ScenarioError;
use super::entries::late_rows::extension_copy::copy_build_as;

/// Имя манифеста внутри сборки.
const MANIFEST_FILE: &str = "manifest.json";
/// Имя MAIN-скрипта экстеншена, перед которым встаёт prelude.
const PAGE_SCRIPT: &str = "page.js";
/// Каталог копии внутри каталога прогона.
const COPY_DIRECTORY: &str = "extension-prelude-copy";

/// Какой чужой код ставится в браузер раньше скрипта экстеншена.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ExtensionPrelude {
    /// Ничего: сборка ставится как есть.
    #[default]
    Absent,
    /// Подкласс `XMLHttpRequest` как у блокировщика рекламы на YouTube.
    XhrSubclass,
}

impl ExtensionPrelude {
    /// Имя файла prelude рядом с манифестом копии и его текст; `None` для [`Self::Absent`].
    fn script(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Absent => None,
            Self::XhrSubclass => Some((
                "bench-xhr-subclass-prelude.js",
                include_str!("../../site-extensions/bench-xhr-subclass-prelude.js"),
            )),
        }
    }

    /// Каталог сборки, который ставится в браузер: исходный (`Absent`) или копия с prelude.
    pub(super) fn apply_to_build(
        self,
        build: &Path,
        run_directory: &Path,
    ) -> Result<PathBuf, ScenarioError> {
        let Some((file_name, source)) = self.script() else {
            return Ok(build.to_path_buf());
        };
        let copy = copy_build_as(build, run_directory, COPY_DIRECTORY)?;
        std::fs::write(copy.join(file_name), source).map_err(|error| {
            ScenarioError::EntryNotActivated(format!("write the prelude {file_name}: {error}"))
        })?;
        let manifest_path = copy.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
            ScenarioError::EntryNotActivated(format!("read {}: {error}", manifest_path.display()))
        })?;
        let mut manifest: Value = serde_json::from_str(&text).map_err(|error| {
            ScenarioError::EntryNotActivated(format!(
                "{} is not JSON: {error}",
                manifest_path.display()
            ))
        })?;
        insert_prelude_before_page_script(&mut manifest, file_name)
            .map_err(ScenarioError::EntryNotActivated)?;
        let pretty = serde_json::to_string_pretty(&manifest).map_err(|error| {
            ScenarioError::EntryNotActivated(format!("cannot write the manifest: {error}"))
        })?;
        std::fs::write(&manifest_path, pretty).map_err(|error| {
            ScenarioError::EntryNotActivated(format!("write {}: {error}", manifest_path.display()))
        })?;
        Ok(copy)
    }
}

/// Вставляет в `content_scripts` копию элемента с `page.js` (те же `matches`, `run_at`, `world`,
/// `all_frames`…), у которой скрипт — prelude, прямо ПЕРЕД оригиналом.
fn insert_prelude_before_page_script(
    manifest: &mut Value,
    prelude_file: &str,
) -> Result<(), String> {
    let scripts = manifest
        .get_mut("content_scripts")
        .and_then(Value::as_array_mut)
        .ok_or("the manifest has no `content_scripts` array")?;
    let position = scripts
        .iter()
        .position(|entry| {
            entry
                .get("js")
                .and_then(Value::as_array)
                .is_some_and(|files| files.iter().any(|file| file.as_str() == Some(PAGE_SCRIPT)))
        })
        .ok_or_else(|| format!("no `content_scripts` entry with {PAGE_SCRIPT}"))?;
    let mut prelude_entry = scripts[position].clone();
    prelude_entry["js"] = Value::Array(vec![Value::String(prelude_file.to_owned())]);
    scripts.insert(position, prelude_entry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{"name":"x","key":"KEY","content_scripts":[
        {"matches":["<all_urls>"],"js":["content.js"],"run_at":"document_start"},
        {"matches":["<all_urls>"],"js":["page.js"],"world":"MAIN","run_at":"document_start","all_frames":true}]}"#;

    fn build_directory() -> tempfile::TempDir {
        let build = tempfile::tempdir().expect("build dir");
        std::fs::write(build.path().join(MANIFEST_FILE), MANIFEST).expect("manifest");
        std::fs::write(build.path().join("page.js"), "// page").expect("page.js");
        build
    }

    #[test]
    fn the_prelude_entry_stands_right_before_page_js_with_its_world_and_stage() {
        let build = build_directory();
        let run = tempfile::tempdir().expect("run dir");
        let copy = ExtensionPrelude::XhrSubclass
            .apply_to_build(build.path(), run.path())
            .expect("copy");

        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(copy.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        let files: Vec<&str> = manifest["content_scripts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["js"][0].as_str().unwrap())
            .collect();
        assert_eq!(
            files,
            ["content.js", "bench-xhr-subclass-prelude.js", "page.js"]
        );
        let prelude = &manifest["content_scripts"][1];
        assert_eq!(prelude["world"], "MAIN");
        assert_eq!(prelude["run_at"], "document_start");
        assert_eq!(prelude["all_frames"], true);
        assert_eq!(manifest["key"], "KEY");
        let script = std::fs::read_to_string(copy.join("bench-xhr-subclass-prelude.js")).unwrap();
        assert!(script.contains("__benchXhrSubclass"));
    }

    #[test]
    fn the_original_build_is_untouched_and_absent_prelude_returns_it_as_is() {
        let build = build_directory();
        let run = tempfile::tempdir().expect("run dir");
        ExtensionPrelude::XhrSubclass
            .apply_to_build(build.path(), run.path())
            .expect("copy");
        assert_eq!(
            std::fs::read_to_string(build.path().join(MANIFEST_FILE)).unwrap(),
            MANIFEST
        );
        assert!(!build.path().join("bench-xhr-subclass-prelude.js").exists());
        let same = ExtensionPrelude::Absent
            .apply_to_build(build.path(), run.path())
            .expect("no-op");
        assert_eq!(same, build.path());
    }

    #[test]
    fn a_build_without_page_js_is_an_error_not_a_silent_no_op() {
        let build = tempfile::tempdir().expect("build dir");
        std::fs::write(
            build.path().join(MANIFEST_FILE),
            r#"{"content_scripts":[{"matches":["<all_urls>"],"js":["content.js"]}]}"#,
        )
        .unwrap();
        let run = tempfile::tempdir().expect("run dir");
        assert!(
            ExtensionPrelude::XhrSubclass
                .apply_to_build(build.path(), run.path())
                .is_err()
        );
    }
}
