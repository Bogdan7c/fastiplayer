//! Частная копия сборки экстеншена для строки «обновление» (10d).
//!
//! Обновление браузер видит, когда тот же экстеншен (тот же id) ставят второй раз с другой
//! `version` в манифесте. Настоящую сборку `extensions/browser/dist` стенд трогать не вправе, поэтому
//! строка копирует её в каталог прогона, ставит копию, поднимает в копии версию и ставит снова.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::super::super::context::ScenarioError;

/// Имя манифеста внутри сборки.
const MANIFEST_FILE: &str = "manifest.json";
/// Имя каталога копии внутри каталога прогона.
const COPY_DIRECTORY: &str = "extension-update-copy";

/// Копирует каталог сборки в каталог прогона и возвращает путь копии.
pub(super) fn copy_into_run_directory(
    build: &Path,
    run_directory: &Path,
) -> Result<PathBuf, ScenarioError> {
    let copy = run_directory.join(COPY_DIRECTORY);
    copy_directory(build, &copy).map_err(|error| {
        io_problem(
            &format!("copy the extension build to {}", copy.display()),
            error,
        )
    })?;
    Ok(copy)
}

/// Поднимает `version` манифеста копии на единицу и возвращает новую версию.
pub(super) fn bump_version(copy: &Path) -> Result<String, ScenarioError> {
    let manifest_path = copy.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| io_problem(&format!("read {}", manifest_path.display()), error))?;
    let mut manifest: Value = serde_json::from_str(&text).map_err(|error| {
        ScenarioError::EntryNotActivated(format!(
            "{} is not JSON: {error}",
            manifest_path.display()
        ))
    })?;
    let current = manifest
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ScenarioError::EntryNotActivated(format!(
                "{} has no string `version`",
                manifest_path.display()
            ))
        })?
        .to_owned();
    let next = next_version(&current).map_err(ScenarioError::EntryNotActivated)?;
    manifest["version"] = Value::String(next.clone());
    let pretty = serde_json::to_string_pretty(&manifest).map_err(|error| {
        ScenarioError::EntryNotActivated(format!("cannot write the manifest: {error}"))
    })?;
    std::fs::write(&manifest_path, pretty)
        .map_err(|error| io_problem(&format!("write {}", manifest_path.display()), error))?;
    Ok(next)
}

/// Версия на единицу больше: у `0.1.0` это `0.1.1`. Браузеры принимают точечные числа; увеличивается
/// последняя часть (любое число частей, кроме нуля и нечисловых).
fn next_version(current: &str) -> Result<String, String> {
    let mut parts: Vec<u64> = current
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| format!("version `{current}` is not dotted numbers"))
        })
        .collect::<Result<_, _>>()?;
    let last = parts
        .last_mut()
        .ok_or_else(|| format!("version `{current}` is empty"))?;
    *last += 1;
    Ok(parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

/// Рекурсивное копирование; ссылок в сборке нет, обычные файлы и каталоги.
fn copy_directory(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn io_problem(action: &str, error: std::io::Error) -> ScenarioError {
    ScenarioError::EntryNotActivated(format!("{action}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_version_part_grows_and_garbage_is_refused() {
        assert_eq!(next_version("0.1.0"), Ok("0.1.1".to_owned()));
        assert_eq!(next_version("1.2.9"), Ok("1.2.10".to_owned()));
        assert_eq!(next_version("7"), Ok("8".to_owned()));
        assert!(next_version("1.x").is_err());
        assert!(next_version("").is_err());
    }

    #[test]
    fn a_copy_gets_a_newer_version_while_the_original_build_stays_untouched() {
        let build = tempfile::tempdir().expect("build dir");
        std::fs::create_dir(build.path().join("icons")).expect("icons dir");
        std::fs::write(build.path().join("icons/a.png"), b"png").expect("icon");
        let original = r#"{"name":"x","version":"0.1.0","key":"KEY"}"#;
        std::fs::write(build.path().join(MANIFEST_FILE), original).expect("manifest");
        let run = tempfile::tempdir().expect("run dir");

        let copy = copy_into_run_directory(build.path(), run.path()).expect("copied");
        assert_eq!(
            std::fs::read(copy.join("icons/a.png")).expect("icon copy"),
            b"png"
        );

        let bumped = bump_version(&copy).expect("bumped");
        assert_eq!(bumped, "0.1.1");
        let copied: Value =
            serde_json::from_str(&std::fs::read_to_string(copy.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(copied["version"], "0.1.1");
        // Остальные поля (в Chromium по `key` считается id) обязаны остаться как были.
        assert_eq!(copied["key"], "KEY");
        assert_eq!(
            std::fs::read_to_string(build.path().join(MANIFEST_FILE)).unwrap(),
            original,
            "the real build must not change"
        );
    }

    #[test]
    fn a_manifest_without_a_version_is_an_error_not_a_silent_no_op() {
        let copy = tempfile::tempdir().expect("dir");
        std::fs::write(copy.path().join(MANIFEST_FILE), r#"{"name":"x"}"#).unwrap();
        assert!(bump_version(copy.path()).is_err());
    }
}
