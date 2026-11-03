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
    /// Обычный рендер, но H.264 первым в приоритете видео-кодеков игрока
    /// (`player.preferred_video_codec_order`): метка `hello` ограничивает сайты H.264 (решение 189).
    /// Только для сценариев; в командной строке не выбирается.
    H264FirstCodecOrder,
    /// Обычные настройки, но самое малое окно предзагрузки `[network]`, какое пропускает
    /// проверка config (1 МиБ окно и кусок, первый кусок 64 КиБ). Для `<video src>` (сессия 09b):
    /// плеер не может взять файл стенда целиком, и перемотка за скачанное обязана дойти до
    /// страницы новым `fetch_range` у цели. Только для сценариев; в командной строке не выбирается.
    SmallFileWindow,
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
