use std::path::PathBuf;

use etcetera::{BaseStrategy, choose_base_strategy};

use crate::{ConfigError, ConfigResult};

/// Имя TOML-файла пользовательской конфигурации.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Имя отдельного файла состояния очереди рядом с пользовательским config.
const PLAYLIST_STATE_FILE_NAME: &str = "playlist-state.json";

/// Имя маленького sidecar последней подтверждённой позиции.
const PLAYLIST_RESUME_FILE_NAME: &str = "playlist-resume.json";

/// Имя стабильного lock artifact, который нельзя удалять между запусками.
const APP_INSTANCE_LOCK_FILE_NAME: &str = "rustiplayer.instance.lock";

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

    /// Возвращает принадлежащую config-слою директорию приложения.
    #[must_use]
    pub fn config_dir(&self) -> &std::path::Path {
        &self.config_dir
    }

    /// Возвращает путь к TOML-конфигурации без повторения имени файла в app-слое.
    #[must_use]
    pub fn config_file(&self) -> &std::path::Path {
        &self.config_file
    }

    /// Строит путь к отдельному persistent state очереди.
    #[must_use]
    pub fn playlist_state_file(&self) -> PathBuf {
        self.config_dir.join(PLAYLIST_STATE_FILE_NAME)
    }

    /// Строит путь к position sidecar, не смешивая его с большим state очереди.
    #[must_use]
    pub fn playlist_resume_file(&self) -> PathBuf {
        self.config_dir.join(PLAYLIST_RESUME_FILE_NAME)
    }

    /// Строит путь к стабильному process-instance lock artifact.
    #[must_use]
    pub fn app_instance_lock_file(&self) -> PathBuf {
        self.config_dir.join(APP_INSTANCE_LOCK_FILE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::ConfigPaths;

    #[test]
    fn derived_paths_share_the_trusted_config_owner() {
        let config_root = PathBuf::from("platform-config-root").join("rustiplayer");
        let paths = ConfigPaths::from_config_dir(&config_root);

        assert_eq!(paths.config_dir(), config_root);
        assert_eq!(paths.config_file(), config_root.join("config.toml"));
        assert_eq!(
            paths.playlist_state_file(),
            config_root.join("playlist-state.json")
        );
        assert_eq!(
            paths.playlist_resume_file(),
            config_root.join("playlist-resume.json")
        );
        assert_eq!(
            paths.app_instance_lock_file(),
            config_root.join("rustiplayer.instance.lock")
        );
    }
}
