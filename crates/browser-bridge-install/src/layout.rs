//! Где что лежит на диске. Чистые функции от окружения и цели, ничего не читают.
//!
//! - посредник у пакетных целей — ОДНА копия на всех:
//!   `$XDG_DATA_HOME/fastiplayer/browser-host/fastiplayer-browser-host`;
//! - посредник у Flatpak-цели — в её каталоге данных:
//!   `~/.var/app/<id>/data/fastiplayer/browser-host/...` (песочница видит свой
//!   `~/.var/app/<id>` по тому же абсолютному пути, поэтому браузер найдёт файл);
//! - файл подключения — `<каталог браузера>/<NATIVE_HOST_NAME>.json`;
//! - служба D-Bus — `$XDG_DATA_HOME/dbus-1/services/<имя службы>.service`.

use std::path::PathBuf;

use browser_bridge_protocol::extension_identity::NATIVE_HOST_NAME;
use desktop_integration::BROWSER_SESSION_BUS_NAME;

use crate::catalog::BrowserTarget;
use crate::environment::InstallEnvironment;
use crate::source::HOST_BINARY_NAME;

/// Каталог посредника относительно каталога данных.
const HOST_DIRECTORY: &str = "fastiplayer/browser-host";

/// Куда копируется посредник для этой цели.
pub fn host_install_path(environment: &InstallEnvironment, target: &BrowserTarget) -> PathBuf {
    let data_root = match target.flatpak_app_id() {
        Some(app_id) => environment.flatpak_app_dir(app_id).join("data"),
        None => environment.xdg_data_home.clone(),
    };
    data_root.join(HOST_DIRECTORY).join(HOST_BINARY_NAME)
}

/// Пути файлов подключения цели (для Firefox их два: главный и XDG).
pub fn manifest_file_paths(
    environment: &InstallEnvironment,
    target: &BrowserTarget,
) -> Vec<PathBuf> {
    target
        .manifest_directories(environment)
        .into_iter()
        .map(|directory| directory.join(format!("{NATIVE_HOST_NAME}.json")))
        .collect()
}

/// Путь `.service`-файла D-Bus activation.
pub fn service_file_path(environment: &InstallEnvironment) -> PathBuf {
    environment
        .xdg_data_home
        .join("dbus-1/services")
        .join(format!("{BROWSER_SESSION_BUS_NAME}.service"))
}
