//! Человеческие тексты фатальных ошибок запуска.
//!
//! Тексты по-английски, без имён Rust-типов, `{:?}` и внутреннего жаргона. Каждый
//! текст отвечает на два вопроса: что случилось и что пользователь может сделать.
//! Технические подробности сюда не попадают — они идут в лог.

use crate::app_instance::ProcessArgsError;

use super::{ConfigDirectoryLocation, ConfigDirectoryProblem, FatalErrorPhase, FatalStartupReason};

/// Заголовок окна ошибки и уведомления; одинаков для всех причин.
pub(super) const FATAL_STARTUP_TITLE: &str = "Fastiplayer could not start";

/// Заголовок для ошибки, случившейся в уже работавшем плеере (после сна/сворачивания).
pub(super) const FATAL_RUNTIME_TITLE: &str = "Fastiplayer stopped";

/// Где пользователь увидит технические подробности: лог пишется только в stderr.
const DETAILS_HINT: &str = "Run fastiplayer from a terminal to see details.";

/// Готовый к показу текст: заголовок и сообщение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FatalStartupNotice {
    pub(crate) title: &'static str,
    pub(crate) message: String,
}

/// Строит текст для пользователя по причине и фазе ошибки.
pub(super) fn notice_for(
    reason: &FatalStartupReason,
    phase: FatalErrorPhase,
) -> FatalStartupNotice {
    match phase {
        FatalErrorPhase::Startup => FatalStartupNotice {
            title: FATAL_STARTUP_TITLE,
            message: message_for(reason),
        },
        FatalErrorPhase::RuntimeRecreation => FatalStartupNotice {
            title: FATAL_RUNTIME_TITLE,
            message: runtime_recreation_message_for(reason),
        },
    }
}

/// Текст для ошибки при восстановлении плеера после сна/сворачивания.
///
/// Достижимы только причины, которые может дать пересоздание renderer и runtime
/// (`GraphicsUnavailable`, `InternalFailure`). Остальные причины — стартовые
/// (аргументы, lease, папка настроек, окно); для них остаётся стартовый текст,
/// но варианты перечислены явно, чтобы новая причина не прошла незамеченной.
fn runtime_recreation_message_for(reason: &FatalStartupReason) -> String {
    match reason {
        FatalStartupReason::GraphicsUnavailable => {
            "Could not restore video output after sleep or window minimization: the graphics card or its driver is not responding.\n\nThe player is closing. Start it again; if this keeps happening, update your graphics driver."
                .to_owned()
        }
        FatalStartupReason::InternalFailure => {
            format!(
                "Could not restore the player after sleep or window minimization.\n\nThe player is closing. Start it again. {DETAILS_HINT}"
            )
        }
        FatalStartupReason::InvalidArguments(_)
        | FatalStartupReason::ConfigLocationUnknown
        | FatalStartupReason::AlreadyRunning
        | FatalStartupReason::RunningInstanceNotResponding
        | FatalStartupReason::RunningInstanceShuttingDown
        | FatalStartupReason::RunningInstanceDidNotTakeFiles
        | FatalStartupReason::ConfigDirectoryUnusable { .. }
        | FatalStartupReason::ConfigFileUnusable { .. }
        | FatalStartupReason::UnsupportedPlatform
        | FatalStartupReason::GraphicalSessionUnavailable
        | FatalStartupReason::WindowCreationFailed => message_for(reason),
    }
}

fn message_for(reason: &FatalStartupReason) -> String {
    match reason {
        FatalStartupReason::InvalidArguments(arguments_error) => {
            invalid_arguments_message(*arguments_error)
        }
        FatalStartupReason::ConfigLocationUnknown => {
            "Could not determine where to store settings: the system did not provide the user home folder."
                .to_owned()
        }
        FatalStartupReason::AlreadyRunning => "Fastiplayer is already running.\n\nSwitch to the open player window."
            .to_owned(),
        FatalStartupReason::RunningInstanceNotResponding => {
            "Fastiplayer is already running but is not responding.\n\nWait a moment and try again. If the player window does not respond, close it and open the file again."
                .to_owned()
        }
        FatalStartupReason::RunningInstanceShuttingDown => "Fastiplayer is closing.\n\nWait a few seconds and open the file again."
            .to_owned(),
        FatalStartupReason::RunningInstanceDidNotTakeFiles => {
            format!(
                "Fastiplayer is already running, but the files could not be forwarded.\n\nOpen them in the running player window. {DETAILS_HINT}"
            )
        }
        FatalStartupReason::ConfigDirectoryUnusable { problem, location } => {
            config_directory_message(problem, location)
        }
        FatalStartupReason::ConfigFileUnusable { location } => format!(
            "Could not read or create the settings file in folder {}.\n\nCheck folder permissions and free disk space. {DETAILS_HINT}",
            location.readable_text()
        ),
        FatalStartupReason::UnsupportedPlatform => {
            "This operating system is not supported yet: Fastiplayer runs on Linux."
                .to_owned()
        }
        FatalStartupReason::GraphicalSessionUnavailable => {
            "Could not connect to the graphical session (Wayland or X11).\n\nStart Fastiplayer within a graphical desktop session."
                .to_owned()
        }
        FatalStartupReason::WindowCreationFailed => {
            format!("Could not create the player window.\n\n{DETAILS_HINT}")
        }
        FatalStartupReason::GraphicsUnavailable => {
            "Could not start video output: the player requires a graphics card with Vulkan support.\n\nInstall or update the Vulkan driver for your graphics card and start the player again."
                .to_owned()
        }
        FatalStartupReason::InternalFailure => {
            format!("An internal player error occurred during startup.\n\n{DETAILS_HINT}")
        }
    }
}

fn invalid_arguments_message(arguments_error: ProcessArgsError) -> String {
    match arguments_error {
        ProcessArgsError::UnknownOption => "The player was started with an unknown option.\n\nIf the file name starts with '-', put '--' before it, for example: fastiplayer -- -video.mkv"
            .to_owned(),
    }
}

fn config_directory_message(
    problem: &ConfigDirectoryProblem,
    location: &ConfigDirectoryLocation,
) -> String {
    let folder = location.readable_text();
    match problem {
        ConfigDirectoryProblem::OwnedByAnotherUser => format!(
            "The settings folder {folder} belongs to another user, probably because the player was previously run with sudo.\n\nRestore your ownership of the folder with this terminal command:\n{}",
            ownership_fix_command(location)
        ),
        ConfigDirectoryProblem::NotADirectory => format!(
            "The settings folder path {folder} points to a file.\n\nRename or delete it, and the player will create the folder again."
        ),
        ConfigDirectoryProblem::LockFileNotRegularFile { lock_file_name } => format!(
            "The service file {lock_file_name} in settings folder {folder} is corrupted.\n\nDelete it, and the player will create a new one."
        ),
        ConfigDirectoryProblem::LockFileReplacedDuringStartup => format!(
            "A service file in settings folder {folder} changed during startup.\n\nStart the player again."
        ),
        ConfigDirectoryProblem::AccessDenied => format!(
            "Access to settings folder {folder} is denied.\n\nIf the player was previously run with sudo, restore your ownership of the folder with this terminal command:\n{}",
            ownership_fix_command(location)
        ),
        ConfigDirectoryProblem::ReadOnlyStorage => {
            format!("The settings folder {folder} is on a read-only disk.")
        }
        ConfigDirectoryProblem::StorageFull => {
            format!("The disk containing settings folder {folder} is full.")
        }
        ConfigDirectoryProblem::SystemError => format!(
            "Could not prepare settings folder {folder} due to a system error.\n\n{DETAILS_HINT}"
        ),
    }
}

/// Команда, возвращающая папку настроек текущему пользователю.
///
/// `"$USER":` — владелец и его основная группа; `-R` нужен, потому что после
/// запуска через sudo чужими оказываются и файлы внутри папки. Синтаксис
/// одинаково работает в bash, zsh и fish.
fn ownership_fix_command(location: &ConfigDirectoryLocation) -> String {
    format!("sudo chown -R \"$USER\": {}", location.shell_argument())
}

#[cfg(test)]
mod tests;
