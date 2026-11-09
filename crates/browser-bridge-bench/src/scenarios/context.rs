//! Общее для всех сценариев: настройки прогона, запущенный сайт стенда, создание окружения
//! и запуск браузера.

use std::path::{Path, PathBuf};

use super::outcome::Verdict;
use super::run_tweaks::RunTweaks;
use crate::browsers::{
    BrowserError, BrowserKind, BrowserUnderTest, ExtensionSource, LaunchOptions, launch,
    launch_with_executable,
};
use crate::environment::{
    BenchEnvironment, DesktopSetup, DisplayTarget, EnvironmentError, EnvironmentOptions,
    PlayerConfigProfile, PlayerTestVariables,
};
use crate::media::{MediaError, ensure_media};
use crate::observe::ObserveError;
use crate::site::{SiteError, StandSite, StandSiteConfig};

/// Фильтр журнала плеера внутри стенда: `info` содержит сводки воспроизведения.
const PLAYER_RUST_LOG: &str = "info";

/// Ошибка сценария: любая ошибка модулей стенда, приведённая к одному типу.
#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("media: {0}")]
    Media(#[from] MediaError),
    #[error("site: {0}")]
    Site(#[from] SiteError),
    #[error("environment: {0}")]
    Environment(#[from] EnvironmentError),
    #[error("browser: {0}")]
    Browser(#[from] BrowserError),
    #[error("log: {0}")]
    Observe(#[from] ObserveError),
    #[error("desktop input: {0}")]
    Input(#[from] crate::desktop_input::InputError),
    #[error("screen: {0}")]
    Screen(#[from] crate::screen_capture::ScreenError),
    #[error("compositor windows: {0}")]
    Windows(#[from] crate::compositor_windows::WindowError),
    #[error("player control: {0}")]
    PlayerControl(#[from] crate::player_control::PlayerControlError),
    /// Вход (меню, кнопка поверх видео) не удалось совершить настоящей рукой: страница не
    /// сообщила цель, меню не открылось, пункта нет там, где он обязан быть.
    #[error("entry: {0}")]
    EntryNotActivated(String),
    /// Не собран настоящий экстеншен: стенд не собирает его сам.
    #[error(
        "the real extension build is missing at {path}: run `npm run build` in extensions/browser"
    )]
    RealExtensionNotBuilt { path: PathBuf },
}

/// Явные пути к исполняемым файлам браузеров (`None` — искать в `PATH`).
#[derive(Debug, Clone, Default)]
pub struct BrowserExecutables {
    pub chromium: Option<PathBuf>,
    pub firefox: Option<PathBuf>,
}

impl BrowserExecutables {
    fn for_kind(&self, kind: BrowserKind) -> Option<&Path> {
        match kind {
            BrowserKind::Chromium => self.chromium.as_deref(),
            BrowserKind::Firefox => self.firefox.as_deref(),
        }
    }
}

/// Всё, что задаёт пользователь командной строки и что не зависит от сценария.
#[derive(Debug, Clone)]
pub struct RunSettings {
    /// Корень репозитория (там `extensions/browser/dist`).
    pub repo_root: PathBuf,
    /// Куда складывать временные каталоги прогонов.
    pub work_root: PathBuf,
    /// Кэш сгенерированных роликов.
    pub media_cache: PathBuf,
    /// Каталог со сборкой плеера и посредника.
    pub player_bin_dir: PathBuf,
    /// Экран.
    pub display: DisplayTarget,
    /// Настройки плеера.
    pub player_config: PlayerConfigProfile,
    /// Дополнительные экстеншены в КАЖДОМ запуске браузера (механизм блокировщика рекламы).
    pub extra_extensions: Vec<PathBuf>,
    /// Не удалять каталоги прогонов и после успеха.
    pub keep_work: bool,
    /// Явные пути к браузерам.
    pub browser_executables: BrowserExecutables,
}

/// Работающий сайт стенда + настройки. Один на весь запуск CLI, чтобы ролики и порты
/// готовились один раз.
pub struct Stand {
    pub settings: RunSettings,
    pub site: StandSite,
}

impl Stand {
    /// Готовит ролики (из кэша или ffmpeg-ом) и поднимает сайт на двух origin.
    pub fn start(settings: RunSettings) -> Result<Self, ScenarioError> {
        let media = ensure_media(&settings.media_cache)?;
        let site = StandSite::start(StandSiteConfig {
            site_root: Path::new(env!("CARGO_MANIFEST_DIR")).join("site"),
            media,
        })?;
        Ok(Self { settings, site })
    }

    /// Создаёт изолированное окружение прогона (экран, шина, временный HOME) с заданным
    /// профилем настроек плеера и устройством экрана. Сценарий, которому важно, как плеер настроен
    /// (метка «дешёвых» кодеков при GLES2), не зависит от `--player-config`; мышиным сценариям
    /// нужен большой экран и право на настоящий ввод ([`DesktopSetup::pointer_stand`]).
    pub(crate) fn create_environment_with_desktop(
        &self,
        player_config: PlayerConfigProfile,
        desktop: DesktopSetup,
        player_test_variables: PlayerTestVariables,
    ) -> Result<BenchEnvironment, ScenarioError> {
        let environment = BenchEnvironment::create(EnvironmentOptions {
            display: self.settings.display,
            player_bin_dir: self.settings.player_bin_dir.clone(),
            player_config,
            player_rust_log: PLAYER_RUST_LOG.to_owned(),
            work_root: self.settings.work_root.clone(),
            desktop,
            player_test_variables,
        })?;
        if self.settings.keep_work {
            environment.keep_work_dir();
        }
        Ok(environment)
    }

    /// Запускает браузер: сначала экстеншены сценария, затем дополнительные из командной строки.
    pub(crate) fn launch_browser(
        &self,
        kind: BrowserKind,
        environment: &BenchEnvironment,
        scenario_extensions: Vec<ExtensionSource>,
    ) -> Result<Box<dyn BrowserUnderTest>, ScenarioError> {
        self.launch_browser_tweaked(
            kind,
            environment,
            scenario_extensions,
            &RunTweaks::default(),
        )
    }

    /// То же с отклонениями прогона: порядок установки, свои дополнительные экстеншены,
    /// имена хостов, живущие на сервере стенда.
    pub(super) fn launch_browser_tweaked(
        &self,
        kind: BrowserKind,
        environment: &BenchEnvironment,
        scenario_extensions: Vec<ExtensionSource>,
        tweaks: &RunTweaks,
    ) -> Result<Box<dyn BrowserUnderTest>, ScenarioError> {
        let extensions =
            tweaks.arrange_extensions(scenario_extensions, &self.settings.extra_extensions);
        let options = LaunchOptions {
            extensions,
            autoplay_without_gesture: true,
            loopback_host_aliases: tweaks.loopback_host_aliases.clone(),
            runtime_extension_install: tweaks.runtime_install.browser_must_allow_it(),
        };
        let browser = match self.settings.browser_executables.for_kind(kind) {
            Some(executable) => launch_with_executable(kind, environment, options, executable)?,
            None => launch(kind, environment, options)?,
        };
        Ok(browser)
    }

    /// Запускает `body` в свежем окружении и сводит любую ошибку к вердикту `Fail`.
    ///
    /// Если вердикт `Fail`, каталог прогона сохраняется для разбора и его путь дописывается
    /// в подробности — иначе после выхода из функции журналы исчезли бы.
    pub(crate) fn with_environment<F>(&self, body: F) -> Verdict
    where
        F: FnOnce(&BenchEnvironment) -> Result<Verdict, ScenarioError>,
    {
        self.with_environment_for_player_config(self.settings.player_config, body)
    }

    /// [`Self::with_environment`] с заданным профилем настроек плеера.
    pub(crate) fn with_environment_for_player_config<F>(
        &self,
        player_config: PlayerConfigProfile,
        body: F,
    ) -> Verdict
    where
        F: FnOnce(&BenchEnvironment) -> Result<Verdict, ScenarioError>,
    {
        self.with_environment_on_desktop(player_config, DesktopSetup::default(), body)
    }

    /// [`Self::with_environment`] с настройками плеера и устройством экрана по выбору сценария.
    pub(crate) fn with_environment_on_desktop<F>(
        &self,
        player_config: PlayerConfigProfile,
        desktop: DesktopSetup,
        body: F,
    ) -> Verdict
    where
        F: FnOnce(&BenchEnvironment) -> Result<Verdict, ScenarioError>,
    {
        self.with_environment_full(player_config, desktop, PlayerTestVariables::default(), body)
    }

    /// [`Self::with_environment`] с тестовыми переменными процесса плеера (выборы вариантов
    /// страницы сценария `series`, сессия 14b).
    pub(crate) fn with_environment_for_player_test<F>(
        &self,
        player_test_variables: PlayerTestVariables,
        body: F,
    ) -> Verdict
    where
        F: FnOnce(&BenchEnvironment) -> Result<Verdict, ScenarioError>,
    {
        self.with_environment_full(
            self.settings.player_config,
            DesktopSetup::default(),
            player_test_variables,
            body,
        )
    }

    /// Общая часть всех `with_environment*`: свежее окружение, тело, сохранение каталога при провале.
    fn with_environment_full<F>(
        &self,
        player_config: PlayerConfigProfile,
        desktop: DesktopSetup,
        player_test_variables: PlayerTestVariables,
        body: F,
    ) -> Verdict
    where
        F: FnOnce(&BenchEnvironment) -> Result<Verdict, ScenarioError>,
    {
        let environment = match self.create_environment_with_desktop(
            player_config,
            desktop,
            player_test_variables,
        ) {
            Ok(environment) => environment,
            Err(error) => return Verdict::fail(error.to_string()),
        };
        let mut verdict =
            body(&environment).unwrap_or_else(|error| Verdict::fail(error.to_string()));
        if verdict.outcome == super::outcome::Outcome::Fail {
            environment.keep_work_dir();
            verdict.detail.push_str(&format!(
                "\nrun directory kept for inspection: {}",
                environment.work_dir().display()
            ));
        }
        verdict
    }
}
