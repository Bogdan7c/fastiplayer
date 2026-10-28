//! Ошибки vendored-сборки с понятным текстом для того, кто запустил `cargo build`.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Почему vendored FFmpeg не собрался.
#[derive(Debug)]
pub enum VendoredBuildError {
    /// Не хватает внешних инструментов; перечислены все сразу.
    MissingTools(Vec<String>),
    /// Сборка под другую платформу не поддерживается.
    CrossCompilation { host: String, target: String },
    /// Архив не скачался.
    Download {
        library_name: &'static str,
        archive_url: &'static str,
        local_archive_env: &'static str,
    },
    /// Архив не совпал с закреплённой контрольной суммой.
    Checksum {
        library_name: &'static str,
        details: String,
    },
    /// В архиве нет ожидаемого каталога верхнего уровня.
    UnexpectedArchiveLayout {
        library_name: &'static str,
        expected_directory: PathBuf,
    },
    /// Внешняя команда завершилась ошибкой.
    StepFailed {
        step_name: String,
        log_path: PathBuf,
        log_tail: String,
    },
    /// Файла патча исходников нет там, где его ждёт `src/source_patches.rs`.
    SourcePatchMissing {
        patch_name: &'static str,
        patch_path: PathBuf,
    },
    /// Патч не лёг на закреплённую версию строго (без fuzz): его надо обновить.
    SourcePatchDoesNotApply {
        patch_name: &'static str,
        patch_path: PathBuf,
        library_name: &'static str,
        library_version: &'static str,
        log_path: PathBuf,
        log_tail: String,
    },
    /// Файл патча не читается (нужен для отпечатка сборки).
    SourcePatchUnreadable { details: String },
    /// Ошибка файловой системы.
    Io { action: String, source: io::Error },
    /// Нужная переменная окружения cargo отсутствует.
    MissingCargoEnv(&'static str),
}

impl fmt::Display for VendoredBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTools(missing_tools) => write!(
                formatter,
                "vendored FFmpeg build needs these tools in PATH: {}. \
                 See README.md (Try it) for the package list",
                missing_tools.join(", ")
            ),
            Self::CrossCompilation { host, target } => write!(
                formatter,
                "vendored FFmpeg supports only native builds (host {host}, target {target}); \
                 build with `--no-default-features --features system-ffmpeg` and a target FFmpeg"
            ),
            Self::Download {
                library_name,
                archive_url,
                local_archive_env,
            } => write!(
                formatter,
                "cannot download {library_name} sources from {archive_url}. \
                 For an offline build download the archive yourself and set \
                 {local_archive_env}=/path/to/archive (its SHA-256 is still checked)"
            ),
            Self::Checksum {
                library_name,
                details,
            } => write!(
                formatter,
                "{library_name} source archive failed verification: {details}"
            ),
            Self::UnexpectedArchiveLayout {
                library_name,
                expected_directory,
            } => write!(
                formatter,
                "{library_name} archive did not unpack into {}",
                expected_directory.display()
            ),
            Self::StepFailed {
                step_name,
                log_path,
                log_tail,
            } => write!(
                formatter,
                "vendored FFmpeg step `{step_name}` failed; full log: {}\n--- log tail ---\n{log_tail}",
                log_path.display()
            ),
            Self::SourcePatchMissing {
                patch_name,
                patch_path,
            } => write!(
                formatter,
                "Fastiplayer source patch `{patch_name}` is missing: expected {}",
                patch_path.display()
            ),
            Self::SourcePatchDoesNotApply {
                patch_name,
                patch_path,
                library_name,
                library_version,
                log_path,
                log_tail,
            } => write!(
                formatter,
                "Fastiplayer patch `{patch_name}` does not apply to {library_name} \
                 {library_version} (strict mode, no fuzz). Update {} for this \
                 {library_name} version and follow the migration checklist in \
                 docs/dependency-patches.toml; full log: {}\n--- log tail ---\n{log_tail}",
                patch_path.display(),
                log_path.display()
            ),
            Self::SourcePatchUnreadable { details } => {
                write!(formatter, "cannot read source patch: {details}")
            }
            Self::Io { action, source } => write!(formatter, "cannot {action}: {source}"),
            Self::MissingCargoEnv(variable) => {
                write!(
                    formatter,
                    "cargo did not provide {variable} to the build script"
                )
            }
        }
    }
}
