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
    /// Render profile `Auto` (файла настроек нет), но у обычного плеера «остался» след
    /// незавершённого старта Vulkan (файл метки рядом с config). Прогноз рендера окна
    /// браузерного режима обязан дать GLES2. Только для сценариев (13a).
    AutoWithVulkanStartupMarker,
    /// Обычные настройки, но самое малое окно предзагрузки `[network]`, какое пропускает
    /// проверка config (1 МиБ окно и кусок, первый кусок 64 КиБ). Для `<video src>` (сессия 09b):
    /// плеер не может взять файл стенда целиком, и перемотка за скачанное обязана дойти до
    /// страницы новым `fetch_range` у цели. Только для сценариев; в командной строке не выбирается.
    SmallFileWindow,
    /// Обычные настройки, но короткий бюджет повторов сети: файл, который фон не может скачать
    /// (заслон локальных адресов, сессия 09c), кончается отказом за секунды, а не за минуту
    /// по умолчанию. Только для сценариев; в командной строке не выбирается.
    ShortNetworkRetry,
}

/// Размер невидимого экрана в пикселях (масштаб экрана всегда 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSize {
    /// Ширина, пикселей.
    pub width: u32,
    /// Высота, пикселей.
    pub height: u32,
}

impl ScreenSize {
    /// Прежний размер всех сценариев: как у владельца на ноутбуке, влезает любое тестовое видео.
    pub const STANDARD: Self = Self {
        width: 1280,
        height: 720,
    };
    /// Экран мышиных сценариев. На 1280x720 родное контекстное меню Chromium не влезает и
    /// прокручивается, а попап экстеншена и окно плеера рядом с браузером тесны. 1920x1200
    /// (16:10) вмещает и меню, и попап, и два окна; кадр снимка при этом остаётся небольшим.
    pub const POINTER_STAND: Self = Self {
        width: 1920,
        height: 1200,
    };
}

/// Пускать ли в невидимый экран «настоящую мышь и клавиатуру» (протокол `fake_input`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopInputPolicy {
    /// Нельзя: kwin работает с обычными проверками прав. Так идут все старые сценарии.
    Forbidden,
    /// Можно: нашему kwin ставится `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` (без неё kwin не
    /// отдаёт `fake_input` и снимок экрана чужим клиентам). Допустимо только на `Virtual`:
    /// на `Real` создание окружения падает ошибкой, а не молча пропускает ввод.
    Allowed,
}

/// Устройство невидимого экрана: размер и право на ввод. По умолчанию — как раньше.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopSetup {
    /// Размер экрана.
    pub screen: ScreenSize,
    /// Право на настоящий ввод и снимки.
    pub input: DesktopInputPolicy,
}

impl Default for DesktopSetup {
    fn default() -> Self {
        Self {
            screen: ScreenSize::STANDARD,
            input: DesktopInputPolicy::Forbidden,
        }
    }
}

impl DesktopSetup {
    /// Экран для мышиных сценариев: большой, с правом на ввод и снимки.
    pub fn pointer_stand() -> Self {
        Self {
            screen: ScreenSize::POINTER_STAND,
            input: DesktopInputPolicy::Allowed,
        }
    }
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
    /// Размер невидимого экрана и право на настоящий ввод (на `Real` не действует размер).
    pub desktop: DesktopSetup,
}
