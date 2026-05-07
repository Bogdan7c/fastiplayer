use std::path::PathBuf;

use directories::ProjectDirs;

use crate::{StorageError, StorageResult};

/// Имя SQLite-файла с долговременными данными приложения.
pub const DATABASE_FILE_NAME: &str = "rustiplayer.sqlite";

/// Qualifier совпадает с config crate, чтобы platform dirs оставались едиными.
const APP_QUALIFIER: &str = "org";

/// Organization для Windows/macOS путей.
const APP_ORGANIZATION: &str = "Rustiplayer";

/// Application name; на Linux `directories` приводит его к `rustiplayer`.
const APP_NAME: &str = "Rustiplayer";

/// Стандартные пути storage-слоя для текущего пользователя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePaths {
    /// Директория данных, например `~/.local/share/rustiplayer`.
    pub data_dir: PathBuf,

    /// Полный путь к SQLite-файлу, например `~/.local/share/rustiplayer/rustiplayer.sqlite`.
    pub database_file: PathBuf,
}

impl StoragePaths {
    /// Определяет platform data-dir через `directories::ProjectDirs`.
    pub fn discover() -> StorageResult<Self> {
        let project_dirs = ProjectDirs::from(APP_QUALIFIER, APP_ORGANIZATION, APP_NAME)
            .ok_or(StorageError::ProjectDirsUnavailable)?;

        Ok(Self::from_data_dir(project_dirs.data_local_dir()))
    }

    /// Собирает пути от уже известной data-директории.
    #[must_use]
    pub fn from_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        let database_file = data_dir.join(DATABASE_FILE_NAME);

        Self {
            data_dir,
            database_file,
        }
    }
}
