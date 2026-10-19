//! Запуск внешних инструментов сборки (meson, configure, make…).
//!
//! Вывод каждой команды пишется в отдельный лог, а не в терминал cargo:
//! сборка FFmpeg очень многословна. При ошибке хвост лога попадает прямо
//! в сообщение, чтобы причину было видно без поиска по `target/`.

use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::build_error::VendoredBuildError;

/// Сколько последних строк лога показывать в сообщении об ошибке.
const FAILURE_LOG_TAIL_LINES: usize = 40;

/// Внешний инструмент, без которого vendored-сборка невозможна.
#[derive(Debug, Clone, Copy)]
pub struct RequiredTool {
    /// Имя исполняемого файла.
    pub executable: &'static str,
    /// Аргумент, на который инструмент отвечает успехом без побочных эффектов.
    pub version_argument: &'static str,
    /// Зачем инструмент нужен — попадает в сообщение об ошибке.
    pub purpose: &'static str,
}

/// Инструменты, которые нужны всегда (скачивание проверяется отдельно).
pub const REQUIRED_BUILD_TOOLS: [RequiredTool; 6] = [
    RequiredTool {
        executable: "tar",
        version_argument: "--version",
        purpose: "unpacking source archives",
    },
    RequiredTool {
        executable: "meson",
        version_argument: "--version",
        purpose: "building dav1d (AV1 software decoder)",
    },
    RequiredTool {
        executable: "ninja",
        version_argument: "--version",
        purpose: "building dav1d (meson backend)",
    },
    RequiredTool {
        executable: "nasm",
        version_argument: "-v",
        purpose: "x86 SIMD assembly for FFmpeg and dav1d",
    },
    RequiredTool {
        executable: "make",
        version_argument: "--version",
        purpose: "building FFmpeg",
    },
    RequiredTool {
        executable: "pkg-config",
        version_argument: "--version",
        purpose: "FFmpeg configure looks up dav1d",
    },
];

/// Инструмент для скачивания; нужен, только если архив не передан локально.
pub const DOWNLOAD_TOOL: RequiredTool = RequiredTool {
    executable: "curl",
    version_argument: "--version",
    purpose: "downloading pinned source archives",
};

/// Проверяет, что все инструменты есть в PATH, и перечисляет все недостающие разом.
pub fn ensure_tools_available(tools: &[RequiredTool]) -> Result<(), VendoredBuildError> {
    let missing_tools: Vec<String> = tools
        .iter()
        .filter(|tool| !is_tool_available(tool))
        .map(|tool| format!("`{}` ({})", tool.executable, tool.purpose))
        .collect();

    if missing_tools.is_empty() {
        Ok(())
    } else {
        Err(VendoredBuildError::MissingTools(missing_tools))
    }
}

/// Возвращает true, если инструмент запускается и завершается успехом.
fn is_tool_available(tool: &RequiredTool) -> bool {
    Command::new(tool.executable)
        .arg(tool.version_argument)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|exit_status| exit_status.success())
}

/// Запускает подготовленную команду, пишет её вывод в лог и проверяет код выхода.
pub fn run_logged(
    mut command: Command,
    step_name: &str,
    log_directory: &Path,
) -> Result<(), VendoredBuildError> {
    let log_path = log_directory.join(format!("{step_name}.log"));
    let io_error = |source| VendoredBuildError::Io {
        action: format!("prepare log for step `{step_name}`"),
        source,
    };

    fs::create_dir_all(log_directory).map_err(io_error)?;
    let stdout_log = File::create(&log_path).map_err(io_error)?;
    // stdout и stderr идут в один файл, чтобы порядок сообщений сохранился.
    let stderr_log = stdout_log.try_clone().map_err(io_error)?;

    let exit_status = command
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log))
        .status()
        .map_err(|source| VendoredBuildError::Io {
            action: format!("start step `{step_name}` ({command:?})"),
            source,
        })?;

    if exit_status.success() {
        Ok(())
    } else {
        Err(VendoredBuildError::StepFailed {
            step_name: step_name.to_owned(),
            log_tail: read_log_tail(&log_path),
            log_path,
        })
    }
}

/// Читает последние строки лога; если лог не читается, честно говорит об этом.
fn read_log_tail(log_path: &Path) -> String {
    match fs::read_to_string(log_path) {
        Ok(log_text) => {
            let all_lines: Vec<&str> = log_text.lines().collect();
            let first_tail_line = all_lines.len().saturating_sub(FAILURE_LOG_TAIL_LINES);
            all_lines[first_tail_line..].join("\n")
        }
        Err(error) => format!("<log is unreadable: {error}>"),
    }
}
