//! Изолированное окружение стенда: временный HOME и XDG, частная шина D-Bus с каталогом
//! служб установщика, невидимый экран, настоящий установщик моста.
//!
//! Владение и инварианты:
//! - всё, что создаёт стенд, лежит в подкаталоге прогона `work_root/run-<pid>-<время>`
//!   (единственное исключение — короткий каталог сокетов, см. `runtime_dir`); наружу
//!   ничего не пишется, настоящие `~/.mozilla` и `~/.config/chromium` не трогаются;
//! - `Drop` убивает шину ВМЕСТЕ с активированным ею плеером и kwin, затем удаляет
//!   каталог прогона (если не попросили сохранить через [`BenchEnvironment::keep_work_dir`]).

mod bus;
mod display;
pub mod error;
mod install;
pub mod options;
mod player_config;
pub(crate) mod process;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::browsers::BrowserKind;

pub use error::EnvironmentError;
pub use options::{DisplayTarget, EnvironmentOptions, PlayerConfigProfile};
pub use process::install_interrupt_cleanup;

use bus::{BusSpec, PrivateBus};
use display::VirtualDisplay;

/// Предел длины пути сокета Unix — 108 байт; берём запас на имя файла.
const MAX_SOCKET_DIRECTORY_BYTES: usize = 70;
/// Имя переменной, которой плеер отличает тестовый запуск: при значении
/// [`PLAYER_AUDIO_SIMULATED`] он берёт тихие часы вместо звуковой карты.
const PLAYER_AUDIO_VARIABLE_NAME: &str = "FASTIPLAYER_TEST_BROWSER_AUDIO";
/// Значение «звук имитируется»: в стенде нет звуковой карты, а часы всё равно нужны.
const PLAYER_AUDIO_SIMULATED: &str = "simulated";
/// Имя переменной, отключающей цветовые коды в журнале (общее соглашение `NO_COLOR`).
const NO_COLOR_VARIABLE_NAME: &str = "NO_COLOR";
/// Любое непустое значение `NO_COLOR` выключает цвет; журнал разбирается как текст.
const NO_COLOR_ENABLED: &str = "1";
/// Имя переменной с фильтром журнала плеера.
const RUST_LOG_VARIABLE_NAME: &str = "RUST_LOG";

/// Окружение прогона. Создаётся [`BenchEnvironment::create`], живёт до `Drop`.
pub struct BenchEnvironment {
    options: EnvironmentOptions,
    /// Подкаталог прогона внутри `work_root`.
    run_dir: PathBuf,
    /// Каталог сокетов (Wayland kwin и шина); короткий путь.
    runtime_dir: PathBuf,
    /// Каталог сокетов создан вне `run_dir` и удаляется отдельно.
    runtime_dir_is_separate: bool,
    home: PathBuf,
    xdg_config_home: PathBuf,
    xdg_data_home: PathBuf,
    xdg_state_home: PathBuf,
    xdg_cache_home: PathBuf,
    player_log: PathBuf,
    keep: bool,
    // Порядок важен: Option, потому что `Drop` останавливает процессы раньше удаления каталогов.
    bus: Option<PrivateBus>,
    display: Option<VirtualDisplay>,
}

impl BenchEnvironment {
    /// Создаёт каталоги, пишет `config.toml` профиля, поднимает экран и шину.
    pub fn create(options: EnvironmentOptions) -> Result<Self, EnvironmentError> {
        // Real рисует на экране процесса: без WAYLAND_DISPLAY рисовать некуда.
        if options.display == DisplayTarget::Real && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return Err(EnvironmentError::NoRealDisplay);
        }
        let run_dir = unique_run_dir(&options.work_root);
        let (runtime_dir, runtime_dir_is_separate) = runtime_dir(&run_dir)?;
        let mut environment = Self {
            home: run_dir.join("home"),
            xdg_config_home: run_dir.join("xdg/config"),
            xdg_data_home: run_dir.join("xdg/data"),
            xdg_state_home: run_dir.join("xdg/state"),
            xdg_cache_home: run_dir.join("xdg/cache"),
            player_log: run_dir.join("player.log"),
            options,
            run_dir,
            runtime_dir,
            runtime_dir_is_separate,
            keep: false,
            bus: None,
            display: None,
        };
        // Если дальше что-то упадёт, `Drop` уже убирает за собой частично созданное.
        environment.create_directories()?;
        player_config::write_config(
            &environment.xdg_config_home,
            environment.options.player_config,
        )?;
        if environment.options.display == DisplayTarget::Virtual {
            environment.display = Some(VirtualDisplay::start(
                &environment.runtime_dir,
                &environment.run_dir,
            )?);
        }
        environment.bus = Some(environment.start_bus()?);
        Ok(environment)
    }

    /// Подключает браузер настоящим установщиком (как кнопка «Connect» в плеере).
    pub fn connect_browser(&self, target: BrowserKind) -> Result<(), EnvironmentError> {
        let bus = self.bus_ref()?;
        install::connect(&install::InstallRequest {
            home: &self.home,
            xdg_config_home: &self.xdg_config_home,
            xdg_data_home: &self.xdg_data_home,
            run_dir: &self.run_dir,
            player_bin_dir: &self.options.player_bin_dir,
            bus_address: bus.address(),
            target_name: target.catalog_name(),
        })
    }

    /// Переменные для запуска браузера: тот же HOME/XDG и та же шина, что у плеера, чтобы
    /// посредник, запущенный браузером, нашёл службу плеера. Остальное окружение наследуется.
    pub fn browser_command_env(&self) -> Vec<(OsString, OsString)> {
        let mut variables = self.isolated_home_env();
        if let Ok(bus) = self.bus_ref() {
            variables.push(("DBUS_SESSION_BUS_ADDRESS".into(), bus.address().into()));
        }
        variables.extend(self.display_env());
        variables
    }

    /// Переменные, которые надо УБРАТЬ из окружения браузера (X11 при невидимом экране).
    pub fn browser_command_env_removed(&self) -> Vec<OsString> {
        match self.options.display {
            DisplayTarget::Virtual => vec!["DISPLAY".into()],
            DisplayTarget::Real => Vec::new(),
        }
    }

    /// Выбранный экран.
    pub fn display_target(&self) -> DisplayTarget {
        self.options.display
    }

    /// Журнал плеера (stderr шины, а с ним и активированного ею плеера).
    pub fn player_log(&self) -> &Path {
        &self.player_log
    }

    /// Временный HOME прогона.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Каталог прогона: журналы, профили браузеров.
    pub fn work_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Временный `XDG_CONFIG_HOME` (в нём профиль Chromium).
    pub fn xdg_config_home(&self) -> &Path {
        &self.xdg_config_home
    }

    /// Не удалять каталог прогона при `Drop` (для разбора после сбоя).
    pub fn keep_work_dir(&mut self) {
        self.keep = true;
    }

    fn bus_ref(&self) -> Result<&PrivateBus, EnvironmentError> {
        self.bus.as_ref().ok_or(EnvironmentError::BusDidNotStart {
            log: self.player_log.clone(),
        })
    }

    fn create_directories(&self) -> Result<(), EnvironmentError> {
        for directory in [
            &self.home,
            &self.xdg_config_home,
            &self.xdg_data_home,
            &self.xdg_state_home,
            &self.xdg_cache_home,
            &self.services_dir(),
        ] {
            std::fs::create_dir_all(directory).map_err(|source| EnvironmentError::Io {
                action: "create directory",
                path: directory.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// Каталог `.service`: именно сюда пишет установщик.
    fn services_dir(&self) -> PathBuf {
        self.xdg_data_home.join("dbus-1/services")
    }

    fn isolated_home_env(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("HOME".into(), self.home.clone().into()),
            (
                "XDG_CONFIG_HOME".into(),
                self.xdg_config_home.clone().into(),
            ),
            ("XDG_DATA_HOME".into(), self.xdg_data_home.clone().into()),
            ("XDG_STATE_HOME".into(), self.xdg_state_home.clone().into()),
            ("XDG_CACHE_HOME".into(), self.xdg_cache_home.clone().into()),
        ]
    }

    /// Куда рисовать: Virtual — наш kwin; Real — окружение процесса без изменений.
    fn display_env(&self) -> Vec<(OsString, OsString)> {
        match self.options.display {
            DisplayTarget::Virtual => VirtualDisplay::drawing_env(&self.runtime_dir),
            DisplayTarget::Real => Vec::new(),
        }
    }

    fn start_bus(&self) -> Result<PrivateBus, EnvironmentError> {
        let socket_path = self.runtime_dir.join("bus");
        let mut env = self.isolated_home_env();
        env.push((
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={}", socket_path.display()).into(),
        ));
        // Плеер наследует окружение шины: журнал без цветовых кодов, фильтр, тихие часы.
        env.push((NO_COLOR_VARIABLE_NAME.into(), NO_COLOR_ENABLED.into()));
        env.push((
            RUST_LOG_VARIABLE_NAME.into(),
            self.options.player_rust_log.clone().into(),
        ));
        env.push((
            PLAYER_AUDIO_VARIABLE_NAME.into(),
            PLAYER_AUDIO_SIMULATED.into(),
        ));
        env.extend(self.display_env());
        let env_removed = self.browser_command_env_removed();
        let config_path = self.run_dir.join("bus.conf");
        PrivateBus::start(&BusSpec {
            config_path: &config_path,
            socket_path: &socket_path,
            services_dir: &self.services_dir(),
            log_path: &self.player_log,
            env: &env,
            env_removed: &env_removed,
        })
    }
}

impl Drop for BenchEnvironment {
    fn drop(&mut self) {
        // Сначала процессы (шина тянет за собой плеер), потом каталоги.
        self.bus = None;
        self.display = None;
        if self.runtime_dir_is_separate {
            let _ = std::fs::remove_dir_all(&self.runtime_dir);
        }
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.run_dir);
        }
    }
}

/// Подкаталог прогона с уникальным именем (pid + наносекунды).
fn unique_run_dir(work_root: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    work_root.join(format!("run-{}-{nanos}", std::process::id()))
}

/// Каталог для сокетов с приватными правами 0700 и КОРОТКИМ путём.
///
/// Пути Unix-сокетов ограничены ~108 байтами, а `work_root` обычно лежит глубоко в
/// `target/`. Если путь внутри прогона короткий — берём его; иначе заводим каталог в
/// системном временном (единственное место вне `work_root`, удаляется при `Drop`).
fn runtime_dir(run_dir: &Path) -> Result<(PathBuf, bool), EnvironmentError> {
    let inside = run_dir.join("rt");
    let (path, separate) = if inside.as_os_str().len() <= MAX_SOCKET_DIRECTORY_BYTES {
        (inside, false)
    } else {
        let name = run_dir.file_name().map_or_else(
            || "run".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        (std::env::temp_dir().join(format!("fpb-{name}")), true)
    };
    // Родитель создаём как угодно (`run_dir` уникален, `temp_dir` системный), а САМ каталог
    // сокетов — строго `create_dir`: `create_dir_all` молча принял бы чужой каталог с тем же
    // именем в /tmp, а `create_dir` при совпадении падает, и сокеты не уходят в чужое место.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| EnvironmentError::Io {
            action: "create socket directory parent",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::create_dir(&path).map_err(|source| EnvironmentError::Io {
        action: "create socket directory",
        path: path.clone(),
        source,
    })?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700)).map_err(
        |source| EnvironmentError::Io {
            action: "restrict socket directory",
            path: path.clone(),
            source,
        },
    )?;
    Ok((path, separate))
}
