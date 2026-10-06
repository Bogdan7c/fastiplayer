//! Системное окно ошибки через внешнюю утилиту рабочего стола (`kdialog`/`zenity`).
//!
//! Утилита рисует окно сама, без нашего GPU-рендера, поэтому работает и тогда,
//! когда Vulkan недоступен. Новых зависимостей не нужно: только `std::process`.

use std::ffi::OsString;
use std::io;
use std::process::{Command, Stdio};

use super::messages::FatalStartupNotice;
use super::presentation::{FatalNoticeChannel, NoticeChannelFailure};

/// Какая утилита показывает окно: у них разный синтаксис аргументов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DialogUtility {
    /// Утилита KDE Plasma.
    Kdialog,
    /// Утилита GNOME и большинства GTK-окружений.
    Zenity,
}

impl DialogUtility {
    /// Имя программы, которая ищется в `PATH`.
    pub(super) fn program_name(self) -> &'static str {
        match self {
            Self::Kdialog => "kdialog",
            Self::Zenity => "zenity",
        }
    }

    /// Аргументы окна ошибки с заголовком и текстом.
    ///
    /// У zenity обязателен `--no-markup`: иначе `<`/`&` в тексте (например, в пути)
    /// разбираются как Pango-разметка и текст пропадает. У kdialog такого флага нет.
    pub(super) fn error_dialog_arguments(self, notice: &FatalStartupNotice) -> Vec<String> {
        match self {
            Self::Kdialog => vec![
                "--title".to_owned(),
                notice.title.to_owned(),
                "--error".to_owned(),
                notice.message.clone(),
            ],
            Self::Zenity => vec![
                "--error".to_owned(),
                "--no-markup".to_owned(),
                "--title".to_owned(),
                notice.title.to_owned(),
                "--text".to_owned(),
                notice.message.clone(),
            ],
        }
    }
}

/// Канал показа «окно через утилиту»; блокирует, пока пользователь не закроет окно.
pub(super) struct DialogUtilityChannel {
    utility: DialogUtility,
    /// Что запускать: обычно имя из `PATH`, в тестах — путь к поддельной утилите.
    program: OsString,
}

impl DialogUtilityChannel {
    /// Настоящая утилита из `PATH`.
    pub(super) fn from_path(utility: DialogUtility) -> Self {
        Self::with_program(utility, utility.program_name())
    }

    /// Утилита с явной программой (для тестов с поддельной утилитой).
    pub(super) fn with_program(utility: DialogUtility, program: impl Into<OsString>) -> Self {
        Self {
            utility,
            program: program.into(),
        }
    }
}

impl FatalNoticeChannel for DialogUtilityChannel {
    fn channel_name(&self) -> &'static str {
        self.utility.program_name()
    }

    fn try_show(&self, notice: &FatalStartupNotice) -> Result<(), NoticeChannelFailure> {
        let status = Command::new(&self.program)
            .args(self.utility.error_dialog_arguments(notice))
            // Вывод утилиты (предупреждения Qt/GTK) пользователю не нужен.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => NoticeChannelFailure::ProgramMissing,
                _ => NoticeChannelFailure::LaunchFailed(error.to_string()),
            })?;

        // Обе утилиты возвращают 0, когда окно показано и закрыто кнопкой «OK».
        // Ненулевой код (нет дисплея, сбой утилиты, закрытие крестиком) не доказывает,
        // что пользователь видел текст, поэтому показ переходит к следующему каналу.
        if status.success() {
            Ok(())
        } else {
            Err(NoticeChannelFailure::ClosedUnsuccessfully {
                exit_code: status.code(),
            })
        }
    }
}

#[cfg(test)]
mod tests;
