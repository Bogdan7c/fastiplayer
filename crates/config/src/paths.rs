use std::path::PathBuf;

use etcetera::{BaseStrategy, choose_base_strategy};

use crate::{ConfigError, ConfigResult};

/// Имя TOML-файла пользовательской конфигурации.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Имя каталога приложения внутри платформенного config-dir.
const APP_CONFIG_DIRECTORY_NAME: &str = "rustiplayer";

/// Стандартные пути config-слоя для текущего пользователя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    /// Директория config-файла, например `~/.config/rustiplayer`.
    pub config_dir: PathBuf,

    /// Полный путь к TOML-файлу, например `~/.config/rustiplayer/config.toml`.
    pub config_file: PathBuf,
}

impl ConfigPaths {
    /// Определяет платформенный config-dir через permissive `etcetera`.
    pub fn discover() -> ConfigResult<Self> {
        let base_strategy =
            choose_base_strategy().map_err(|_| ConfigError::ProjectDirsUnavailable)?;
        let config_dir = base_strategy.config_dir().join(APP_CONFIG_DIRECTORY_NAME);

        Ok(Self::from_config_dir(config_dir))
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
