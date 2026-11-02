//! Подключение браузера НАСТОЯЩИМ установщиком моста — тем же, что за кнопкой «Connect».
//!
//! Установщик определяет браузер по исполняемому файлу в `PATH`, поэтому стенд даёт ему
//! фейковый `PATH` с заглушкой нужного имени; пишет он во временный HOME, а частная шина
//! получает просьбу перечитать службы.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use browser_bridge_install::{
    BrowserBridgeInstaller, BrowserTargetId, FlatpakError, FlatpakTool, InstallEnvironment,
    InstallSource, SessionBusReloadError, SessionBusReloader,
};
use desktop_integration::BrowserEntryBus;

use super::error::EnvironmentError;

/// Сколько ждать ответ шины на «перечитай службы».
const RELOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// Имя плеера и посредника в каталоге сборки.
const PLAYER_BINARY: &str = "fastiplayer";
const HOST_BINARY: &str = "fastiplayer-browser-host";

/// `flatpak` без приложений: пакетные цели от него не зависят.
struct NoFlatpakApps;

impl FlatpakTool for NoFlatpakApps {
    fn installed_apps(&self) -> Result<Vec<String>, FlatpakError> {
        Ok(Vec::new())
    }

    fn permissions(&self, _app_id: &str) -> Result<String, FlatpakError> {
        Err(FlatpakError::NotAvailable)
    }

    fn grant_bridge_bus_access(&self, _app_id: &str) -> Result<(), FlatpakError> {
        Err(FlatpakError::NotAvailable)
    }
}

/// Просит перечитать службы именно ЧАСТНУЮ шину стенда, а не рабочий стол владельца.
struct PrivateBusReloader {
    address: String,
}

impl SessionBusReloader for PrivateBusReloader {
    fn reload_service_files(&self) -> Result<(), SessionBusReloadError> {
        desktop_integration::reload_session_bus_services(
            &BrowserEntryBus::Address(self.address.clone()),
            RELOAD_TIMEOUT,
        )
    }
}

/// Всё, что нужно установщику от окружения стенда.
pub(crate) struct InstallRequest<'a> {
    /// Временный HOME.
    pub(crate) home: &'a Path,
    /// Временный `XDG_CONFIG_HOME`.
    pub(crate) xdg_config_home: &'a Path,
    /// Временный `XDG_DATA_HOME`.
    pub(crate) xdg_data_home: &'a Path,
    /// Рабочий каталог прогона (для заглушек и копии плеера).
    pub(crate) run_dir: &'a Path,
    /// Каталог сборки с плеером и посредником.
    pub(crate) player_bin_dir: &'a Path,
    /// Адрес частной шины.
    pub(crate) bus_address: &'a str,
    /// Имя цели каталога установщика: `chromium` или `firefox`.
    pub(crate) target_name: &'a str,
}

/// Подключает браузер и проверяет, что установщик ничего не оговорил предупреждениями.
pub(crate) fn connect(request: &InstallRequest<'_>) -> Result<(), EnvironmentError> {
    let player = player_with_host_next_to_it(request.player_bin_dir)?;
    let stub_dir = request.run_dir.join("fake-path");
    write_browser_stub(&stub_dir, request.target_name)?;
    let search_path = std::env::join_paths([&stub_dir])
        .map_err(|error| EnvironmentError::Install(format!("fake PATH: {error}")))?;
    let environment = InstallEnvironment::from_variables(
        Some(request.home.as_os_str().to_owned()),
        Some(request.xdg_config_home.as_os_str().to_owned()),
        Some(request.xdg_data_home.as_os_str().to_owned()),
        Some(search_path),
    )
    .map_err(|error| EnvironmentError::Install(error.to_string()))?;
    let installer = BrowserBridgeInstaller::new(
        environment,
        InstallSource::from_player_executable(player),
        Box::new(NoFlatpakApps),
        Box::new(PrivateBusReloader {
            address: request.bus_address.to_owned(),
        }),
    );
    let outcome = installer
        .connect(&BrowserTargetId::new(request.target_name))
        .map_err(|error| EnvironmentError::Install(error.to_string()))?;
    if outcome.warnings.is_empty() {
        Ok(())
    } else {
        Err(EnvironmentError::Install(format!(
            "installer finished with warnings: {:?}",
            outcome.warnings
        )))
    }
}

/// Заглушка «браузера» в фейковом `PATH`: по ней установщик находит цель.
fn write_browser_stub(stub_dir: &Path, name: &str) -> Result<(), EnvironmentError> {
    std::fs::create_dir_all(stub_dir).map_err(|source| EnvironmentError::Io {
        action: "create fake PATH directory",
        path: stub_dir.to_path_buf(),
        source,
    })?;
    let stub = stub_dir.join(name);
    std::fs::write(&stub, b"#!/bin/sh\nexit 0\n")
        .and_then(|()| std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)))
        .map_err(|source| EnvironmentError::Io {
            action: "write browser stub",
            path: stub,
            source,
        })
}

/// Путь к плееру, рядом с которым лежит посредник.
///
/// Установщик ищет посредника РЯДОМ с плеером и копирует его оттуда; сборка
/// `cargo build -p app-egui -p browser-bridge-host` кладёт обоих в `target/<профиль>/`,
/// вместе с `lib/` вендорного FFmpeg (RUNPATH `$ORIGIN/lib`). Здесь только проверяем,
/// что сборка полная, и говорим понятно, чего не хватает.
fn player_with_host_next_to_it(player_bin_dir: &Path) -> Result<PathBuf, EnvironmentError> {
    let player = player_bin_dir.join(PLAYER_BINARY);
    let host = player_bin_dir.join(HOST_BINARY);
    for required in [&player, &host] {
        if !required.is_file() {
            return Err(EnvironmentError::PlayerBuild(format!(
                "{} not found (cargo build -p app-egui -p browser-bridge-host)",
                required.display()
            )));
        }
    }
    Ok(player)
}
