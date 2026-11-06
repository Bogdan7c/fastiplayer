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
mod popup_probe;
mod support;
mod toolbar_pin;

use std::path::{Path, PathBuf};

pub use error::BrowserError;
pub use hand_off::{HandOffError, HandOffOptions, HandOffOutcome, LocalAddressConsent};
pub use popup_probe::{FocusedElement, PopupSnapshot};

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Имена хостов, которые браузер обязан разрешать в `127.0.0.1`: так «публичный» сайт
    /// (`public.bench.example`, рекламный домен) живёт на сервере стенда. Пусто — ничего.
    pub loopback_host_aliases: Vec<String>,
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
    /// Делает вкладку активной (видимой) в окне браузера: попап работает с АКТИВНОЙ вкладкой.
    fn activate_tab(&mut self, tab: &TabHandle) -> Result<(), BrowserError>;
    /// Закрывает вкладку.
    fn close_tab(&mut self, tab: TabHandle) -> Result<(), BrowserError>;
    /// Открывает страницу нашего экстеншена и возвращает её видимый текст.
    fn open_extension_page(&mut self, page: ExtensionPage) -> Result<String, BrowserError>;
    /// Робот «отдать вкладку» (решение 175): открывает служебную страницу экстеншена, находит
    /// вкладку, чей адрес начинается с `page_url_prefix`, шлёт фону `hand_off_tab` и разбирает
    /// исход. `stand_tab` — вкладка стенда: после закрытия служебной страницы она снова
    /// становится активной (неактивную вкладку браузер притормаживает).
    fn hand_off_tab_with_options(
        &mut self,
        stand_tab: &TabHandle,
        page_url_prefix: &str,
        options: HandOffOptions,
    ) -> Result<HandOffOutcome, BrowserError>;
    /// То же без согласий человека: ровно то, что шлёт кнопка попапа.
    fn hand_off_tab(
        &mut self,
        stand_tab: &TabHandle,
        page_url_prefix: &str,
    ) -> Result<HandOffOutcome, BrowserError> {
        self.hand_off_tab_with_options(stand_tab, page_url_prefix, HandOffOptions::default())
    }
    /// Выполняет в служебной странице экстеншена (страница настроек) асинхронную функцию
    /// `async (api) => string` и возвращает её строку. `api` — `chrome`/`browser` страницы
    /// экстеншена. Пока страница грузится и API нет, попытки повторяются до таймаута.
    /// Нужна сценарию кодеков: записать настройку в `storage.local` и прочитать регистрацию
    /// динамического скрипта — то, что делает сам экстеншен, а не страница сайта.
    fn run_in_extension_page(&mut self, async_function: &str) -> Result<String, BrowserError>;
    /// Заголовок вкладки: независимая от снимка улика того, что в страницу дошли клик и клавиши
    /// (тестовая страница пишет их в `document.title`).
    fn tab_title(&mut self, tab: &TabHandle) -> Result<String, BrowserError>;
    /// Адрес открытого попапа нашего экстеншена, если он сейчас показан на панели браузера.
    /// Независимая от снимка улика «клик по значку открыл попап»: Chromium отвечает целью
    /// DevTools `chrome-extension://…/popup.html`, Firefox читает chrome-контекст Marionette.
    fn open_popup_url(&mut self) -> Result<Option<String>, BrowserError>;
    /// Снимок открытого попапа (случай `data-notice`, строки, фокус); `None`, если попапа нет.
    fn popup_snapshot(&mut self) -> Result<Option<PopupSnapshot>, BrowserError>;
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
