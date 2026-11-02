//! Источник установки: откуда копировать посредника и какой плеер будить.
//!
//! Посредник `fastiplayer-browser-host` собирается вместе с плеером и лежит РЯДОМ с
//! ним (в том же каталоге). Источник проверяется один раз при создании; если с ним
//! беда, установщик не падает, а сообщает проблему в обзоре ([`SourceProblem`]).

use std::path::{Path, PathBuf};

use desktop_integration::{ActivationServiceFileError, browser_session_activation_service_file};
use thiserror::Error;

/// Имя исполняемого файла посредника (имя `[[bin]]` crate-а `browser-bridge-host`).
pub const HOST_BINARY_NAME: &str = "fastiplayer-browser-host";

/// Почему установить мост с этой копии плеера нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SourceProblem {
    /// Не удалось узнать путь к исполняемому файлу плеера.
    #[error("Cannot determine where the player executable is: {kind:?}")]
    PlayerExecutableUnknown {
        /// Класс ошибки ОС.
        kind: std::io::ErrorKind,
    },
    /// Рядом с плеером нет посредника: сборка неполная.
    #[error("The browser helper `{HOST_BINARY_NAME}` was not found next to the player")]
    HostBinaryMissing {
        /// Где посредник ожидался.
        expected: PathBuf,
        /// Путь плеера: нужен отключению, чтобы узнать свою службу.
        player: PathBuf,
    },
    /// Путь плеера нельзя безопасно записать в `Exec=` файла службы D-Bus.
    #[error("The player path cannot be used for D-Bus activation: {reason}")]
    PlayerPathUnsupported {
        /// Путь плеера как есть.
        player: PathBuf,
        /// Чем он плох.
        reason: ActivationServiceFileError,
    },
}

impl SourceProblem {
    /// Путь плеера, если он известен несмотря на проблему. Отключению и обзору хватает
    /// его одного: убрать свои файлы можно и без посредника рядом с плеером.
    pub fn player_executable(&self) -> Option<&Path> {
        match self {
            Self::HostBinaryMissing { player, .. } | Self::PlayerPathUnsupported { player, .. } => {
                Some(player)
            }
            Self::PlayerExecutableUnknown { .. } => None,
        }
    }
}

/// Проверенный источник: плеер и посредник рядом с ним.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSource {
    player_executable: PathBuf,
    host_executable: PathBuf,
}

impl InstallSource {
    /// Источник по текущему процессу: канонический путь `current_exe`.
    pub fn from_current_exe() -> Result<Self, SourceProblem> {
        let current = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|error| SourceProblem::PlayerExecutableUnknown { kind: error.kind() })?;
        Self::from_player_executable(current)
    }

    /// Источник по явному пути плеера (тесты и приложения с известным путём).
    ///
    /// Порядок проверок: сначала путь плеера (иначе `.service` не записать), потом посредник.
    pub fn from_player_executable(player_executable: PathBuf) -> Result<Self, SourceProblem> {
        browser_session_activation_service_file(&player_executable).map_err(|reason| {
            SourceProblem::PlayerPathUnsupported {
                player: player_executable.clone(),
                reason,
            }
        })?;
        let host_executable = player_executable.with_file_name(HOST_BINARY_NAME);
        if !crate::fs_util::is_executable_file(&host_executable) {
            return Err(SourceProblem::HostBinaryMissing {
                expected: host_executable,
                player: player_executable,
            });
        }
        Ok(Self {
            player_executable,
            host_executable,
        })
    }

    /// Путь исполняемого файла плеера (его будит шина).
    pub fn player_executable(&self) -> &Path {
        &self.player_executable
    }

    /// Путь посредника, откуда он копируется.
    pub fn host_executable(&self) -> &Path {
        &self.host_executable
    }
}
