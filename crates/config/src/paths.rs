use std::path::PathBuf;

use directories::ProjectDirs;

use crate::{ConfigError, ConfigResult};

/// Имя TOML-файла пользовательской конфигурации.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Qualifier для `directories::ProjectDirs`; на Linux не влияет на `~/.config/rustiplayer`.
const APP_QUALIFIER: &str = "org";

/// Organization для Windows/macOS путей.
const APP_ORGANIZATION: &str = "Rustiplayer";

/// Application name; на Linux превращается в `rustiplayer`.
const APP_NAME: &str = "Rustiplayer";

/// Стандартные пути config-слоя для текущего пользователя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    /// Директория config-файла, например `~/.config/rustiplayer`.
    pub config_dir: PathBuf,

    /// Полный путь к TOML-файлу, например `~/.config/rustiplayer/config.toml`.
    pub config_file: PathBuf,
}

impl ConfigPaths {
    /// Определяет платформенный config-dir через `directories::ProjectDirs`.
    pub fn discover() -> ConfigResult<Self> {
        let project_dirs = ProjectDirs::from(APP_QUALIFIER, APP_ORGANIZATION, APP_NAME)
            .ok_or(ConfigError::ProjectDirsUnavailable)?;

        Ok(Self::from_config_dir(project_dirs.config_dir()))
    }

    /// Собирает пути от уже известной config-директории.
    #[must_use]
    pub fn from_config_dir(config_dir: impl Into<PathBuf>) -> Self {
        let config_dir = config_dir.into();
        let config_file = config_dir.join(CONFIG_FILE_NAME);

        Self {
            config_dir,
            config_file,
        }
    }
}
