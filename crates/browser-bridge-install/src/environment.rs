//! Окружение установки: где у пользователя дом, конфиги, данные и исполняемые файлы.
//!
//! Всё, что установщик читает из процесса (HOME, XDG, PATH), собирается здесь один
//! раз; дальше модули получают готовую структуру. Поэтому тесты подставляют фейковое
//! окружение в временном каталоге и никогда не трогают настоящий домашний каталог.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Где у snapd лежат обёртки приложений (`/snap/bin/chromium`).
const DEFAULT_SNAP_BIN_DIRS: [&str; 2] = ["/snap/bin", "/var/lib/snapd/snap/bin"];

/// Почему окружение собрать нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EnvironmentError {
    /// `HOME` не задан или не абсолютный: без домашнего каталога писать некуда.
    #[error("The HOME directory is not set or is not an absolute path")]
    HomeUnavailable,
}

/// Каталоги пользователя, от которых считаются все пути установки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallEnvironment {
    /// Домашний каталог (`~`).
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME` (по умолчанию `~/.config`).
    pub xdg_config_home: PathBuf,
    /// `$XDG_DATA_HOME` (по умолчанию `~/.local/share`).
    pub xdg_data_home: PathBuf,
    /// Каталоги из `PATH`, в которых ищутся исполняемые файлы браузеров.
    pub executable_search_dirs: Vec<PathBuf>,
    /// Каталоги snapd с обёртками приложений; находка здесь = Snap-браузер.
    pub snap_bin_dirs: Vec<PathBuf>,
}

impl InstallEnvironment {
    /// Окружение текущего процесса.
    pub fn from_process_env() -> Result<Self, EnvironmentError> {
        Self::from_variables(
            std::env::var_os("HOME"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("PATH"),
        )
    }

    /// Собирает окружение из значений переменных (чистая функция, её проверяют тесты).
    ///
    /// По спецификации XDG относительные значения `XDG_*` игнорируются.
    pub fn from_variables(
        home: Option<OsString>,
        xdg_config_home: Option<OsString>,
        xdg_data_home: Option<OsString>,
        path: Option<OsString>,
    ) -> Result<Self, EnvironmentError> {
        let home = home
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or(EnvironmentError::HomeUnavailable)?;
        let xdg_config_home = absolute_or(xdg_config_home, || home.join(".config"));
        let xdg_data_home = absolute_or(xdg_data_home, || home.join(".local/share"));
        let executable_search_dirs = path
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        Ok(Self {
            home,
            xdg_config_home,
            xdg_data_home,
            executable_search_dirs,
            snap_bin_dirs: DEFAULT_SNAP_BIN_DIRS.iter().map(PathBuf::from).collect(),
        })
    }

    /// Каталог Flatpak-приложения `~/.var/app/<app id>`: песочница видит его под тем же путём.
    pub fn flatpak_app_dir(&self, app_id: &str) -> PathBuf {
        self.home.join(".var/app").join(app_id)
    }

    /// Лежит ли путь внутри каталога snapd.
    pub fn is_snap_path(&self, path: &Path) -> bool {
        self.snap_bin_dirs
            .iter()
            .any(|snap_dir| path.starts_with(snap_dir))
    }
}

/// Значение, если оно абсолютное, иначе значение по умолчанию.
fn absolute_or(value: Option<OsString>, default: impl FnOnce() -> PathBuf) -> PathBuf {
    value
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(default)
}
