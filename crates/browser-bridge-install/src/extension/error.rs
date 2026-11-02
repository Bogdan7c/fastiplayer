//! Ошибки установщика экстеншена: у каждой причины свой вариант, чтобы интерфейс мог
//! отличить «нечего ставить» от «диск не пишется» и от «браузер не запустился».

use std::io;
use std::path::PathBuf;

use atomic_file_store::AtomicFileWriteFailure;
use thiserror::Error;

use crate::flatpak::FlatpakError;

use super::launch::LaunchError;
use super::presence::UnreadableKind;
use super::state::ExtensionUnsupported;

/// Шаг работы с файлом-подсказкой, на котором случилась ошибка ОС.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintStep {
    /// Чтение уже лежащего файла.
    ReadHint,
    /// Создание каталога `External Extensions/`.
    CreateDirectory,
    /// Запись файла.
    WriteHint,
    /// Удаление файла.
    RemoveHint,
}

/// Почему установка или удаление подсказки не состоялись.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExtensionInstallError {
    /// Установщик экстеншена реализован только для Linux. Windows получит свой
    /// путь через реестр (см. документацию модуля).
    #[error("Extension installation is not supported on this platform yet")]
    UnsupportedPlatform,
    /// Такой цели нет в каталоге.
    #[error("Unknown browser")]
    UnknownTarget,
    /// Браузер не найден в системе.
    #[error("The browser is not installed")]
    BrowserNotInstalled,
    /// Экстеншен ещё не опубликован в магазине этого браузера.
    #[error("The extension is not published in this browser's store yet")]
    NotPublished,
    /// Для этого браузера экстеншен поставить нельзя.
    #[error("The extension cannot be installed in this browser: {0:?}")]
    Unsupported(ExtensionUnsupported),
    /// У этого браузера нет файла-подсказки, убирать нечего.
    #[error("This browser has no extension hint file")]
    NoHintForThisBrowser,
    /// Профиль браузера не читается, состояние неизвестно: действовать вслепую не станем.
    #[error("A browser profile file is unreadable ({kind:?}) at {path}")]
    ProfileUnreadable {
        /// Файл или каталог.
        path: PathBuf,
        /// Причина.
        kind: UnreadableKind,
    },
    /// Под именем нашей подсказки лежит чужое: не перезаписываем.
    #[error("A foreign file occupies the hint path {path}")]
    ForeignHintInTheWay {
        /// Путь чужого файла.
        path: PathBuf,
    },
    /// Утилита `flatpak` не ответила, поэтому нельзя понять, стоит ли Flatpak-браузер.
    /// Отличается от `BrowserNotInstalled`: проблема в утилите, а не в браузере.
    #[error("Cannot list Flatpak apps: {0}")]
    FlatpakUnavailable(FlatpakError),
    /// Ошибка ОС при работе с файлом-подсказкой.
    #[error("{kind:?} while {step:?} at {path}")]
    HintIo {
        /// Шаг.
        step: HintStep,
        /// Путь (может содержать имя пользователя: в журнал не пишется).
        path: PathBuf,
        /// Класс ошибки ОС.
        kind: io::ErrorKind,
    },
    /// Атомарная запись не состоялась; прежний файл остался на месте.
    #[error("Atomic write failed ({failure:?}) at {path}")]
    HintAtomicWrite {
        /// Целевой файл.
        path: PathBuf,
        /// Этап и причина.
        failure: AtomicFileWriteFailure,
    },
    /// Браузер с адресом магазина не запустился.
    #[error("{0}")]
    Launch(LaunchError),
}
