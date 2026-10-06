//! Человеческие тексты фатальных ошибок запуска.
//!
//! Тексты по-русски, без имён Rust-типов, `{:?}` и внутреннего жаргона. Каждый
//! текст отвечает на два вопроса: что случилось и что пользователь может сделать.
//! Технические подробности сюда не попадают — они идут в лог.

use crate::app_instance::ProcessArgsError;

use super::{ConfigDirectoryLocation, ConfigDirectoryProblem, FatalStartupReason};

/// Заголовок окна ошибки и уведомления; одинаков для всех причин.
pub(super) const FATAL_STARTUP_TITLE: &str = "Fastiplayer не запустился";

/// Где пользователь увидит технические подробности: лог пишется только в stderr.
const DETAILS_HINT: &str = "Подробности видны, если запустить fastiplayer из терминала.";

/// Готовый к показу текст: заголовок и сообщение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FatalStartupNotice {
    pub(crate) title: &'static str,
    pub(crate) message: String,
}

/// Строит текст для пользователя по причине ошибки.
pub(super) fn notice_for(reason: &FatalStartupReason) -> FatalStartupNotice {
    FatalStartupNotice {
        title: FATAL_STARTUP_TITLE,
        message: message_for(reason),
    }
}

fn message_for(reason: &FatalStartupReason) -> String {
    match reason {
        FatalStartupReason::InvalidArguments(arguments_error) => {
            invalid_arguments_message(*arguments_error)
        }
        FatalStartupReason::ConfigLocationUnknown => {
            "Не удалось определить, где хранить настройки: система не сообщила \
             домашнюю папку пользователя."
                .to_owned()
        }
        FatalStartupReason::AlreadyRunning => "Fastiplayer уже запущен.\n\n\
             Перейдите в окно уже открытого плеера."
            .to_owned(),
        FatalStartupReason::ConfigDirectoryUnusable { problem, location } => {
            config_directory_message(problem, location)
        }
        FatalStartupReason::ConfigFileUnusable { location } => format!(
            "Не удалось прочитать или создать файл настроек в папке {}.\n\n\
             Проверьте права на папку и свободное место на диске. {DETAILS_HINT}",
            location.readable_text()
        ),
        FatalStartupReason::UnsupportedPlatform => {
            "Эта операционная система пока не поддерживается: Fastiplayer работает в Linux."
                .to_owned()
        }
        FatalStartupReason::GraphicalSessionUnavailable => {
            "Не удалось подключиться к графическому сеансу (Wayland или X11).\n\n\
             Fastiplayer нужно запускать внутри графического рабочего стола."
                .to_owned()
        }
        FatalStartupReason::WindowCreationFailed => {
            format!("Не удалось создать окно плеера.\n\n{DETAILS_HINT}")
        }
        FatalStartupReason::GraphicsUnavailable => {
            "Не удалось запустить вывод изображения: плееру нужна видеокарта \
             с поддержкой Vulkan.\n\n\
             Установите или обновите драйвер Vulkan для вашей видеокарты \
             и запустите плеер снова."
                .to_owned()
        }
        FatalStartupReason::InternalFailure => {
            format!("Во время запуска произошла внутренняя ошибка плеера.\n\n{DETAILS_HINT}")
        }
    }
}

fn invalid_arguments_message(arguments_error: ProcessArgsError) -> String {
    match arguments_error {
        ProcessArgsError::UnknownOption => "Плеер запущен с неизвестным параметром.\n\n\
             Если имя файла начинается с «-», поставьте перед ним «--», например: \
             fastiplayer -- -видео.mkv"
            .to_owned(),
        ProcessArgsError::ExtraPositional => "При запуске можно открыть только один файл.\n\n\
             Откройте один файл, а остальные добавьте в плейлист из окна плеера."
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
            "Папка настроек {folder} принадлежит другому пользователю — скорее всего, \
             плеер когда-то запускали через sudo.\n\n\
             Верните папку себе командой в терминале:\n{}",
            ownership_fix_command(location)
        ),
        ConfigDirectoryProblem::NotADirectory => format!(
            "Вместо папки настроек {folder} лежит файл.\n\n\
             Переименуйте или удалите его, и плеер создаст папку заново."
        ),
        ConfigDirectoryProblem::LockFileNotRegularFile { lock_file_name } => format!(
            "Служебный файл {lock_file_name} в папке настроек {folder} повреждён.\n\n\
             Удалите его, и плеер создаст новый."
        ),
        ConfigDirectoryProblem::LockFileReplacedDuringStartup => format!(
            "Служебный файл в папке настроек {folder} изменился во время запуска.\n\n\
             Запустите плеер ещё раз."
        ),
        ConfigDirectoryProblem::AccessDenied => format!(
            "Нет доступа к папке настроек {folder}.\n\n\
             Если плеер когда-то запускали через sudo, верните папку себе командой \
             в терминале:\n{}",
            ownership_fix_command(location)
        ),
        ConfigDirectoryProblem::ReadOnlyStorage => {
            format!("Папка настроек {folder} находится на диске, доступном только для чтения.")
        }
        ConfigDirectoryProblem::StorageFull => {
            format!("На диске с папкой настроек {folder} закончилось место.")
        }
        ConfigDirectoryProblem::SystemError => format!(
            "Не удалось подготовить папку настроек {folder} из-за системной ошибки.\n\n\
             {DETAILS_HINT}"
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
