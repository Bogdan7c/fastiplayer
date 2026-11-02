//! Типизированные ошибки и предупреждения установки (без паник и без «молчаливых» сбоев).

use std::io;
use std::path::PathBuf;

use atomic_file_store::AtomicFileWriteFailure;
use desktop_integration::SessionBusReloadError;
use thiserror::Error;

use crate::expected::ExpectationError;
use crate::source::SourceProblem;

/// Шаг, на котором установка остановилась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStep {
    /// Проверка, что цель можно подключать.
    CheckTarget,
    /// Создание каталогов назначения.
    PrepareDirectories,
    /// Чтение уже лежащего файла подключения.
    InspectManifest,
    /// Копирование посредника.
    CopyHost,
    /// Запись `.service`.
    WriteServiceFile,
    /// Запись файла подключения.
    WriteManifest,
    /// Удаление файла подключения.
    RemoveManifest,
    /// Удаление копии посредника.
    RemoveHost,
    /// Удаление `.service`.
    RemoveServiceFile,
}

impl std::fmt::Display for InstallStep {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::CheckTarget => "checking the browser",
            Self::PrepareDirectories => "creating folders",
            Self::InspectManifest => "reading the existing connection file",
            Self::CopyHost => "copying the browser helper",
            Self::WriteServiceFile => "writing the D-Bus service file",
            Self::WriteManifest => "writing the connection file",
            Self::RemoveManifest => "removing the connection file",
            Self::RemoveHost => "removing the browser helper",
            Self::RemoveServiceFile => "removing the D-Bus service file",
        };
        formatter.write_str(text)
    }
}

/// Почему цель не поддерживается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// Snap-браузер: песочница snap не даёт ни файла подключения, ни звонка на шину.
    Snap,
    /// Flatpak-Firefox: нет `xdg-native-messaging-proxy` (bug 2078349).
    FlatpakFirefoxFamily,
}

impl std::fmt::Display for UnsupportedReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Snap => "Snap browsers are not supported",
            Self::FlatpakFirefoxFamily => "Flatpak builds of Firefox are not supported yet",
        })
    }
}

/// Причина остановки установки.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstallErrorKind {
    /// Такой цели нет в каталоге.
    #[error("Unknown browser")]
    UnknownTarget,
    /// Браузер не найден в системе.
    #[error("The browser is not installed")]
    TargetNotInstalled,
    /// Цель не поддерживается.
    #[error("{0}")]
    Unsupported(UnsupportedReason),
    /// Эта копия плеера не может быть источником установки.
    #[error("{0}")]
    Source(SourceProblem),
    /// Ожидаемые файлы построить нельзя.
    #[error("{0}")]
    Expectation(ExpectationError),
    /// Ошибка ОС на конкретном пути.
    #[error("{kind:?} at {path}")]
    Io {
        /// Путь (может содержать имя пользователя: в журнал не пишется).
        path: PathBuf,
        /// Класс ошибки ОС.
        kind: io::ErrorKind,
    },
    /// Атомарная запись не состоялась; прежний файл остался на месте.
    #[error("Atomic write failed ({failure:?}) at {path}")]
    AtomicWrite {
        /// Целевой файл.
        path: PathBuf,
        /// Этап и причина.
        failure: AtomicFileWriteFailure,
    },
}

/// Ошибка установки или отключения: где остановились и почему.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Failed while {step}: {kind}")]
pub struct InstallError {
    /// Шаг.
    pub step: InstallStep,
    /// Причина.
    pub kind: InstallErrorKind,
}

impl InstallError {
    /// Ошибка ОС на пути.
    pub(crate) fn io(step: InstallStep, path: impl Into<PathBuf>, error: &io::Error) -> Self {
        Self {
            step,
            kind: InstallErrorKind::Io {
                path: path.into(),
                kind: error.kind(),
            },
        }
    }

    /// Ошибка без пути.
    pub(crate) fn of(step: InstallStep, kind: InstallErrorKind) -> Self {
        Self { step, kind }
    }
}

/// Предупреждение: действие выполнено, но пользователю стоит знать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallWarning {
    /// Под нашим именем лежал чужой корректный файл подключения; перезаписан.
    OverwroteForeignFile {
        /// Путь файла.
        path: PathBuf,
    },
    /// Под нашим именем лежал битый файл; заменён.
    ReplacedDamagedFile {
        /// Путь файла.
        path: PathBuf,
    },
    /// Шина не перечитала службы; заработает после нового входа в сеанс.
    BusReloadFailed(SessionBusReloadError),
    /// Файл записан, но сохранность записи на диск не подтверждена.
    DurabilityUnconfirmed {
        /// Путь файла.
        path: PathBuf,
    },
    /// Служба раньше будила другую копию плеера; теперь будит эту.
    TookOverFromAnotherCopy {
        /// Прежний бинарь из `Exec=`.
        previous_player: PathBuf,
    },
}
