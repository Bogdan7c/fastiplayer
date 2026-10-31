use std::ffi::OsString;
use std::path::{Path, PathBuf};

use etcetera::{BaseStrategy, choose_base_strategy};

use crate::{ConfigError, ConfigResult};

/// Имя TOML-файла пользовательской конфигурации.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Суффикс файла-замка записи config-а: замок лежит рядом с config под именем
/// `<имя config-а>.lock`, например `config.toml.lock`.
///
/// Замок — отдельный файл, а не сам `config.toml`: config заменяется атомарным rename,
/// после которого у пути другой inode, и замок на старом inode уже никого бы не исключал.
const CONFIG_WRITE_LOCK_FILE_SUFFIX: &str = ".lock";

/// Имя отдельного файла состояния очереди рядом с пользовательским config.
const PLAYLIST_STATE_FILE_NAME: &str = "playlist-state.json";

/// Имя маленького sidecar последней подтверждённой позиции.
const PLAYLIST_RESUME_FILE_NAME: &str = "playlist-resume.json";

/// Имя файла памяти выбора дорожек и задержки субтитров по видео.
const TRACK_MEMORY_FILE_NAME: &str = "track-memory.json";

/// Имя метки «запуск рендера не дошёл до первого кадра» (сессия 09 плана gles2-renderer).
const RENDERER_STARTUP_MARKER_FILE_NAME: &str = "renderer-startup.marker";

/// Имя стабильного lock artifact, который нельзя удалять между запусками.
const APP_INSTANCE_LOCK_FILE_NAME: &str = "fastiplayer.instance.lock";

/// Имя каталога приложения внутри платформенного config-dir.
const APP_CONFIG_DIRECTORY_NAME: &str = "fastiplayer";

/// Стандартные пути config-слоя для текущего пользователя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    /// Директория config-файла, например `~/.config/fastiplayer`.
    pub config_dir: PathBuf,

    /// Полный путь к TOML-файлу, например `~/.config/fastiplayer/config.toml`.
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

    /// Строит путь к файлу-замку записи config-а (`config.toml.lock`).
    ///
    /// Замок общий для всех копий плеера, которые пишут этот config (решение 135 моста):
    /// обычного окна и окон браузерного режима.
    #[must_use]
    pub fn config_write_lock_file(&self) -> PathBuf {
        config_write_lock_path_for(&self.config_file)
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

    /// Строит путь к памяти дорожек по видео; отдельный файл, чтобы частые правки выбора не
    /// переписывали очередь и resume-позицию.
    #[must_use]
    pub fn track_memory_file(&self) -> PathBuf {
        self.config_dir.join(TRACK_MEMORY_FILE_NAME)
    }

    /// Строит путь к метке незавершённого старта рендера: файл существует, только пока
    /// запуск не дошёл до первого показанного кадра.
    #[must_use]
    pub fn renderer_startup_marker_file(&self) -> PathBuf {
        self.config_dir.join(RENDERER_STARTUP_MARKER_FILE_NAME)
    }

    /// Строит путь к стабильному process-instance lock artifact.
    #[must_use]
    pub fn app_instance_lock_file(&self) -> PathBuf {
        self.config_dir.join(APP_INSTANCE_LOCK_FILE_NAME)
    }
}

/// Путь файла-замка записи для config-а по произвольному пути (для `*_at` API).
///
/// Замок лежит в том же каталоге, что и config: так он защищает ровно этот файл, и
/// два разных config-а (например, тесты с разными каталогами) не мешают друг другу.
#[must_use]
pub fn config_write_lock_path_for(config_file: &Path) -> PathBuf {
    let mut lock_file_name = config_file
        .file_name()
        .map_or_else(|| OsString::from(CONFIG_FILE_NAME), OsString::from);
    lock_file_name.push(CONFIG_WRITE_LOCK_FILE_SUFFIX);
    config_file.with_file_name(lock_file_name)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ConfigPaths, config_write_lock_path_for};

    #[test]
    fn derived_paths_share_the_trusted_config_owner() {
        let config_root = PathBuf::from("platform-config-root").join("fastiplayer");
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
            paths.track_memory_file(),
            config_root.join("track-memory.json")
        );
        assert_eq!(
            paths.renderer_startup_marker_file(),
            config_root.join("renderer-startup.marker")
        );
        assert_eq!(
            paths.app_instance_lock_file(),
            config_root.join("fastiplayer.instance.lock")
        );
        assert_eq!(
            paths.config_write_lock_file(),
            config_root.join("config.toml.lock")
        );
    }

    /// Замок произвольного config-а лежит рядом с ним и повторяет его имя.
    #[test]
    fn write_lock_path_follows_any_config_file_name() {
        let custom_config = PathBuf::from("some-dir").join("custom.toml");

        assert_eq!(
            config_write_lock_path_for(&custom_config),
            PathBuf::from("some-dir").join("custom.toml.lock")
        );
    }
}
