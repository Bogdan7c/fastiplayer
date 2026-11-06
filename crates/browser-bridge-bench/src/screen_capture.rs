//! Снимок невидимого экрана и поиск значка экстеншена на панели браузера.
//!
//! Снимок делает `spectacle` внутри частного сеанса (свой kwin, своя шина; нужна переменная
//! `KWIN_WAYLAND_NO_PERMISSION_CHECKS`, её ставит окружение при `DesktopInputPolicy::Allowed`).
//! PNG разбирается `ffmpeg`-ом. Если значок не найден или найдено два, снимок сохраняется в
//! каталоге прогона (а каталог не удаляется) и путь к нему лежит в ошибке.

mod frame;
mod icon_search;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub use frame::{Frame, RgbaImage};
pub use icon_search::{
    DEFAULT_MAX_MEAN_DIFFERENCE, IconMatch, IconTemplate, SearchBand, find_icon,
};

use crate::browsers::BrowserKind;
use crate::desktop_input::Point;
use crate::environment::{BenchEnvironment, DesktopEndpoint, EnvironmentError};

/// Размеры, в которых браузер может нарисовать значок на панели (16–32 px, шаг 2).
const TOOLBAR_ICON_SIZES: [u32; 9] = [16, 18, 20, 22, 24, 26, 28, 30, 32];
/// Исходник значка для масштабирования шаблонов.
const SOURCE_ICON_FILE: &str = "icon-128.png";
/// Полоса поиска: верхние строки экрана, где у обоих браузеров вкладки и панель.
const TOOLBAR_BAND: SearchBand = SearchBand {
    top: 0,
    height: 200,
};
/// Сколько ждать снимок (первый запуск `spectacle` с пустым профилем долгий).
const SPECTACLE_TIMEOUT: Duration = Duration::from_secs(20);

/// Ошибки снимков и поиска.
#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    /// Невидимого экрана с правом на снимки нет.
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// Не удалось запустить внешнюю программу.
    #[error("cannot start {program}: {source}")]
    Spawn {
        /// Имя программы.
        program: &'static str,
        /// Ошибка ОС.
        source: std::io::Error,
    },
    /// `spectacle` не сделал снимок.
    #[error("spectacle did not produce a screenshot: {0}")]
    Spectacle(String),
    /// `ffmpeg` не разобрал PNG.
    #[error("cannot decode {path}: {message}")]
    Decode {
        /// Файл.
        path: PathBuf,
        /// Текст ffmpeg.
        message: String,
    },
    /// Размер данных кадра не сошёлся с ожидаемым.
    #[error("frame has {actual} bytes, expected {expected}")]
    BadFrame {
        /// Ожидаемая длина.
        expected: usize,
        /// Полученная длина.
        actual: usize,
    },
    /// Каталог значков не дал ни одного пригодного шаблона.
    #[error("no usable icon template in {0}")]
    NoTemplates(PathBuf),
    /// Не удалось сохранить снимок или записать файл.
    #[error("{action}: {path}: {source}")]
    Io {
        /// Что делали.
        action: &'static str,
        /// Файл.
        path: PathBuf,
        /// Ошибка ОС.
        source: std::io::Error,
    },
    /// Значка нет в полосе панели.
    #[error(
        "extension icon not found on the toolbar band (no place within the threshold; closest finished reading {closest_difference:.1}); screenshot: {screenshot:?}"
    )]
    IconNotFound {
        /// Наименьшая виденная разница (бесконечность, если искать было негде).
        closest_difference: f32,
        /// Сохранённый снимок для разбора.
        screenshot: Option<PathBuf>,
    },
    /// Значок всё время менял место: панель браузера не устоялась.
    #[error("the extension icon kept moving on the toolbar (last seen at {last:?})")]
    IconNotSettled {
        /// Последнее место значка.
        last: Option<Point>,
    },
    /// В полосе два разных места, похожих на значок.
    #[error("extension icon is ambiguous: {first:?} and {second:?}; screenshot: {screenshot:?}")]
    IconAmbiguous {
        /// Первое место.
        first: Point,
        /// Второе место.
        second: Point,
        /// Сохранённый снимок для разбора.
        screenshot: Option<PathBuf>,
    },
}

/// Снимки экрана стенда и поиск значка экстеншена.
pub struct ScreenCapture<'env> {
    environment: &'env BenchEnvironment,
    endpoint: DesktopEndpoint,
    templates: Vec<IconTemplate>,
    next_shot: AtomicU32,
}

impl<'env> ScreenCapture<'env> {
    /// `icons_dir` — каталог `icons/` собранного экстеншена нужного браузера
    /// (`extensions/browser/dist/<браузер>/icons`).
    pub fn new(environment: &'env BenchEnvironment, icons_dir: &Path) -> Result<Self, ScreenError> {
        let endpoint = environment.desktop_endpoint()?;
        let source = icons_dir.join(SOURCE_ICON_FILE);
        let mut templates = Vec::new();
        for size in TOOLBAR_ICON_SIZES {
            let image = RgbaImage::decode_png_scaled(&source, size)?;
            templates.extend(IconTemplate::from_image(&image));
        }
        if templates.is_empty() {
            return Err(ScreenError::NoTemplates(icons_dir.to_path_buf()));
        }
        Ok(Self {
            environment,
            endpoint,
            templates,
            next_shot: AtomicU32::new(0),
        })
    }

    /// Каталог значков экстеншена браузера внутри репозитория.
    pub fn icons_dir(repository_root: &Path, browser: BrowserKind) -> PathBuf {
        let dist_name = match browser {
            BrowserKind::Chromium => "chrome",
            BrowserKind::Firefox => "firefox",
        };
        repository_root
            .join("extensions/browser/dist")
            .join(dist_name)
            .join("icons")
    }

    /// Снимает экран в файл каталога прогона и возвращает путь к PNG.
    pub fn take_screenshot(&self, label: &str) -> Result<PathBuf, ScreenError> {
        let number = self.next_shot.fetch_add(1, Ordering::Relaxed);
        let path = self
            .environment
            .work_dir()
            .join(format!("screen-{number:02}-{label}.png"));
        run_spectacle(&self.endpoint, &path)?;
        Ok(path)
    }

    /// Снимает экран и разбирает кадр.
    pub fn capture_frame(&self, label: &str) -> Result<Frame, ScreenError> {
        let path = self.take_screenshot(label)?;
        let screen = self.endpoint.screen();
        Frame::decode_png(&path, screen.width, screen.height)
    }

    /// Ищет значок экстеншена на панели. Не нашёл или нашёл два места — ошибка с путём к
    /// сохранённому снимку (каталог прогона при этом не удаляется).
    pub fn find_extension_icon(&self) -> Result<IconMatch, ScreenError> {
        let path = self.take_screenshot("find-icon")?;
        let screen = self.endpoint.screen();
        let frame = Frame::decode_png(&path, screen.width, screen.height)?;
        match find_icon(
            &frame,
            &self.templates,
            TOOLBAR_BAND,
            DEFAULT_MAX_MEAN_DIFFERENCE,
        ) {
            Ok(found) => Ok(found),
            Err(error) => {
                self.environment.keep_work_dir();
                Err(error.with_screenshot(path))
            }
        }
    }
}

/// Сколько раз подряд значок обязан стоять на одном месте, чтобы по нему кликать.
const SETTLED_READINGS: usize = 2;
/// Сколько попыток дать панели устояться (она сдвигается, пока исчезают всплывашки и кнопки адресной строки).
const SETTLE_ATTEMPTS: usize = 12;
/// Пауза между снимками при ожидании устоявшейся панели.
const SETTLE_PAUSE: Duration = Duration::from_millis(500);

impl ScreenCapture<'_> {
    /// Ищет значок, пока он не окажется на ОДНОМ месте в двух снимках подряд. Панель браузера
    /// двигается (всплывашка перевода, кнопки адресной строки), и клик по месту из устаревшего
    /// снимка попал бы в соседний элемент. Ошибка поиска возвращается сразу, без повторов.
    pub fn find_settled_extension_icon(&self) -> Result<IconMatch, ScreenError> {
        let mut previous: Option<IconMatch> = None;
        let mut agreeing = 1;
        for _ in 0..SETTLE_ATTEMPTS {
            let found = self.find_extension_icon()?;
            if previous.is_some_and(|before| before.top_left == found.top_left) {
                agreeing += 1;
            } else {
                agreeing = 1;
            }
            if agreeing >= SETTLED_READINGS {
                return Ok(found);
            }
            previous = Some(found);
            std::thread::sleep(SETTLE_PAUSE);
        }
        Err(ScreenError::IconNotSettled {
            last: previous.map(|found| found.top_left),
        })
    }
}

impl ScreenError {
    /// Дописывает путь снимка в ошибки поиска (остальные не меняются).
    fn with_screenshot(self, path: PathBuf) -> Self {
        match self {
            Self::IconNotFound {
                closest_difference, ..
            } => Self::IconNotFound {
                closest_difference,
                screenshot: Some(path),
            },
            Self::IconAmbiguous { first, second, .. } => Self::IconAmbiguous {
                first,
                second,
                screenshot: Some(path),
            },
            other => other,
        }
    }
}

/// Запускает `spectacle` (весь экран, без уведомления и интерфейса) внутри частного сеанса.
fn run_spectacle(endpoint: &DesktopEndpoint, output: &Path) -> Result<(), ScreenError> {
    let home = endpoint.kwin_home();
    let mut child = Command::new("spectacle")
        .args(["-b", "-n", "-f", "-o"])
        .arg(output)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_RUNTIME_DIR", endpoint.runtime_dir())
        .env("WAYLAND_DISPLAY", endpoint.wayland_display_name())
        .env("DBUS_SESSION_BUS_ADDRESS", endpoint.kwin_bus_address())
        .env("QT_QPA_PLATFORM", "wayland")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| ScreenError::Spawn {
            program: "spectacle",
            source,
        })?;
    let deadline = Instant::now() + SPECTACLE_TIMEOUT;
    loop {
        let status = child.try_wait().map_err(|source| ScreenError::Spawn {
            program: "spectacle",
            source,
        })?;
        if status.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            // Зависший снимок убираем, иначе он остался бы сиротой на невидимом экране.
            let _ = child.kill();
            let _ = child.wait();
            return Err(ScreenError::Spectacle("timed out".to_owned()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let finished = child
        .wait_with_output()
        .map_err(|source| ScreenError::Spawn {
            program: "spectacle",
            source,
        })?;
    // Код выхода spectacle ненадёжен: судим по файлу.
    if output
        .metadata()
        .map(|meta| meta.len() > 0)
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(ScreenError::Spectacle(
            String::from_utf8_lossy(&finished.stderr).trim().to_owned(),
        ))
    }
}
