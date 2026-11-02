//! Шов к утилите `flatpak`: список приложений, права песочницы, выдача доступа к шине.
//!
//! Установщик зависит от trait [`FlatpakTool`], а не от процесса `flatpak`: в тестах
//! стоит фейк, в приложении — [`RealFlatpakTool`]. Разбор текста прав
//! (`flatpak info --show-permissions`) — чистая функция [`parse_bus_access`].

mod command;
mod permissions;

use std::time::Duration;

use thiserror::Error;

pub use command::RealFlatpakTool;
pub use permissions::{BRIDGE_TALK_NAME_MASK, FlatpakBusAccess, parse_bus_access};

/// Сколько ждать одну команду `flatpak` по умолчанию: список и права читаются за
/// доли секунды, `override` пишет один файл.
pub const DEFAULT_FLATPAK_TIMEOUT: Duration = Duration::from_secs(10);

/// Почему команда `flatpak` не дала результата.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FlatpakError {
    /// Утилиты `flatpak` в системе нет.
    #[error("`flatpak` is not installed")]
    NotAvailable,
    /// Команда не уложилась во время и была остановлена.
    #[error("`flatpak` did not finish in time")]
    TimedOut,
    /// Команда завершилась с ошибкой.
    #[error("`flatpak` failed (exit code {exit_code:?}): {stderr}")]
    Failed {
        /// Код выхода, `None` если процесс убит сигналом.
        exit_code: Option<i32>,
        /// Начало stderr для диагностики.
        stderr: String,
    },
    /// Вывод не удалось прочитать или запустить процесс.
    #[error("Could not run `flatpak`: {kind:?}")]
    Io {
        /// Класс ошибки ОС.
        kind: std::io::ErrorKind,
    },
}

/// Операции с Flatpak, нужные установщику.
pub trait FlatpakTool: Send + Sync {
    /// App ID всех установленных Flatpak-приложений.
    ///
    /// Отсутствие утилиты `flatpak` — не ошибка, а пустой список (Flatpak не
    /// используется); остальные сбои возвращаются как есть.
    fn installed_apps(&self) -> Result<Vec<String>, FlatpakError>;

    /// Текст прав приложения (`flatpak info --show-permissions <app id>`).
    fn permissions(&self, app_id: &str) -> Result<String, FlatpakError>;

    /// Даёт приложению доступ к имени браузерного режима на сессионной шине
    /// (`flatpak override --user --talk-name=<BRIDGE_TALK_NAME_MASK> <app id>`).
    /// Вызывается только по явному клику пользователя.
    fn grant_bridge_bus_access(&self, app_id: &str) -> Result<(), FlatpakError>;
}
