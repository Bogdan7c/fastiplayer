//! Параметры окружения стенда.

use std::path::PathBuf;

/// На каком экране рисуют плеер и браузер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayTarget {
    /// Частный невидимый `kwin_wayland --virtual` 1280x720: экран владельца не трогается.
    Virtual,
    /// Экран текущего сеанса (`WAYLAND_DISPLAY` процесса). Шина и профили всё равно частные.
    Real,
}

/// Какой `config.toml` лежит у плеера внутри стенда.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerConfigProfile {
    /// Файла нет: плеер работает на настройках по умолчанию.
    Clean,
    /// Как у владельца: рендер OpenGL ES (GLES2) и аппаратное декодирование VA-API.
    Gles2Vaapi,
}

/// Исходные данные для [`crate::environment::BenchEnvironment::create`].
#[derive(Debug, Clone)]
pub struct EnvironmentOptions {
    /// Экран.
    pub display: DisplayTarget,
    /// Каталог со сборкой: рядом лежат `fastiplayer` и `fastiplayer-browser-host`.
    pub player_bin_dir: PathBuf,
    /// Настройки плеера.
    pub player_config: PlayerConfigProfile,
    /// Фильтр журнала плеера (`RUST_LOG`), обычно `info`.
    pub player_rust_log: String,
    /// Каталог, внутри которого стенд заводит СВОЙ подкаталог прогона.
    pub work_root: PathBuf,
}
