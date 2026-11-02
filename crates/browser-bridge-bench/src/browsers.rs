//! Запуск Chromium и Firefox под тестом: экстеншены, вкладки, страницы экстеншенов.
//!
//! Chromium ведёт DevTools/CDP (`chromium`), Firefox — Marionette (`firefox`).
//! Оба запускаются в окружении стенда ([`BenchEnvironment`]): временный HOME, частная
//! шина, невидимый экран; stderr браузера (а с ним и stderr посредника) пишется в
//! `<work_dir>/<браузер>.stderr.log`. При `Drop` без `shutdown` группа процессов браузера
//! убивается (`ManagedProcess`).

mod chromium;
mod devtools;
mod error;
mod firefox;
mod hand_off;
mod marionette;
mod support;

use std::path::{Path, PathBuf};

pub use error::BrowserError;
pub use hand_off::{HandOffError, HandOffOutcome};

use crate::environment::BenchEnvironment;

/// Какой браузер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserKind {
    /// Chromium (пакет дистрибутива).
    Chromium,
    /// Firefox (пакет дистрибутива).
    Firefox,
}

impl BrowserKind {
    /// Журнал stderr браузера (и посредника) в каталоге прогона; пишет его запуск браузера,
    /// читают сценарии (свидетельства посредника) — путь известен ещё до запуска.
    pub fn stderr_log_path(self, work_dir: &Path) -> PathBuf {
        work_dir.join(format!("{}.stderr.log", self.catalog_name()))
    }

    /// Имя цели в каталоге установщика и исполняемого файла в `PATH`.
    pub fn catalog_name(self) -> &'static str {
        match self {
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
        }
    }
}

/// Откуда взять дополнительный экстеншен.
#[derive(Debug, Clone)]
pub enum ExtensionSource {
    /// Распакованный каталог (Chromium `--load-extension`, Firefox временная установка).
    UnpackedDir(PathBuf),
    /// `.xpi`/`.zip` (только Firefox; Chromium такие файлы не принимает без `--load-extension`).
    PackedFile(PathBuf),
}

/// Параметры запуска браузера.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// Экстеншены, которые надо поставить.
    pub extensions: Vec<ExtensionSource>,
    /// Разрешить автовоспроизведение без жеста пользователя.
    pub autoplay_without_gesture: bool,
}

/// Вкладка (цель DevTools у Chromium, окно-вкладка у Firefox).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabHandle(String);

impl TabHandle {
    pub(crate) fn new(raw: String) -> Self {
        Self(raw)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Страница нашего экстеншена (его ID известен: Chromium по ключу, Firefox по постоянному UUID).
#[derive(Debug, Clone)]
pub struct ExtensionPage {
    /// Путь внутри экстеншена, например `popup.html`.
    pub file: String,
    /// Если задан — ждать, пока этот текст появится на странице; иначе ждать, пока
    /// текст станет непустым и перестанет меняться.
    pub expect_text: Option<String>,
}

impl ExtensionPage {
    /// Страница без ожидаемого текста.
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            expect_text: None,
        }
    }

    /// Попап экстеншена, пока на нём не появится `expect_text`.
    pub fn popup_with_text(expect_text: impl Into<String>) -> Self {
        Self {
            file: "popup.html".to_owned(),
            expect_text: Some(expect_text.into()),
        }
    }
}

/// Как закончился браузер.
#[derive(Debug, Clone)]
pub struct BrowserExit {
    /// Код выхода главного процесса (`None` — убит сигналом).
    pub exit_code: Option<i32>,
    /// Вышел сам после просьбы закрыться, без SIGKILL.
    pub graceful: bool,
    /// Журнал stderr браузера (и посредника).
    pub stderr_log: PathBuf,
}

/// Браузер, которым управляет стенд.
pub trait BrowserUnderTest {
    /// Какой это браузер.
    fn kind(&self) -> BrowserKind;
    /// Открывает адрес в новой вкладке (не ждёт полной загрузки ресурсов).
    fn open_tab(&mut self, url: &str) -> Result<TabHandle, BrowserError>;
    /// Закрывает вкладку.
    fn close_tab(&mut self, tab: TabHandle) -> Result<(), BrowserError>;
    /// Открывает страницу нашего экстеншена и возвращает её видимый текст.
    fn open_extension_page(&mut self, page: ExtensionPage) -> Result<String, BrowserError>;
    /// Робот «отдать вкладку» (решение 175): открывает служебную страницу экстеншена, находит
    /// вкладку, чей адрес начинается с `page_url_prefix`, шлёт фону `hand_off_tab` и разбирает
    /// исход. `stand_tab` — вкладка стенда: после закрытия служебной страницы она снова
    /// становится активной (неактивную вкладку браузер притормаживает).
    fn hand_off_tab(
        &mut self,
        stand_tab: &TabHandle,
        page_url_prefix: &str,
    ) -> Result<HandOffOutcome, BrowserError>;
    /// Мягко закрывает браузер, ждёт выход, добивает остатки группы.
    fn shutdown(self: Box<Self>) -> Result<BrowserExit, BrowserError>;
}

/// Запускает браузер из `PATH` (`chromium` / `firefox`).
pub fn launch(
    kind: BrowserKind,
    environment: &BenchEnvironment,
    options: LaunchOptions,
) -> Result<Box<dyn BrowserUnderTest>, BrowserError> {
    launch_with_executable(kind, environment, options, Path::new(kind.catalog_name()))
}

/// То же, но с явным путём к исполняемому файлу браузера (другая сборка, Chrome и т. п.).
pub fn launch_with_executable(
    kind: BrowserKind,
    environment: &BenchEnvironment,
    options: LaunchOptions,
    executable: &Path,
) -> Result<Box<dyn BrowserUnderTest>, BrowserError> {
    match kind {
        BrowserKind::Chromium => Ok(Box::new(chromium::launch(
            environment,
            &options,
            executable,
        )?)),
        BrowserKind::Firefox => Ok(Box::new(firefox::launch(
            environment,
            &options,
            executable,
        )?)),
    }
}
