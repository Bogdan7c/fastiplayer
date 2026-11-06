//! Адреса невидимого экрана для тех, кто им управляет: ввод, снимки, скрипты окон.

use std::path::{Path, PathBuf};

use super::display::{VirtualDisplay, WAYLAND_SOCKET_NAME};
use super::options::ScreenSize;

/// Куда подключаться к невидимому kwin. Выдаётся только окружением с правом на ввод
/// ([`super::BenchEnvironment::desktop_endpoint`]), поэтому само наличие значения — доказательство,
/// что ввод разрешён и экран не настоящий.
#[derive(Debug, Clone)]
pub struct DesktopEndpoint {
    runtime_dir: PathBuf,
    kwin_home: PathBuf,
    kwin_bus_address: String,
    screen: ScreenSize,
}

impl DesktopEndpoint {
    pub(super) fn new(runtime_dir: &Path, run_dir: &Path, screen: ScreenSize) -> Self {
        Self {
            runtime_dir: runtime_dir.to_path_buf(),
            kwin_home: VirtualDisplay::home_dir(run_dir),
            kwin_bus_address: VirtualDisplay::bus_address(runtime_dir),
            screen,
        }
    }

    /// Сокет Wayland невидимого экрана.
    pub fn wayland_socket(&self) -> PathBuf {
        self.runtime_dir.join(WAYLAND_SOCKET_NAME)
    }

    /// Имя сокета для `WAYLAND_DISPLAY`.
    pub fn wayland_display_name(&self) -> &'static str {
        WAYLAND_SOCKET_NAME
    }

    /// Каталог сокетов (`XDG_RUNTIME_DIR` клиентов экрана).
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// HOME kwin: туда клиенты экрана (снимок) пишут свои настройки, а не в настоящий HOME.
    pub fn kwin_home(&self) -> &Path {
        &self.kwin_home
    }

    /// Адрес ЧАСТНОЙ шины kwin. Это не шина плеера: у плеера своя ([`super::BenchEnvironment::bus_address`]).
    pub fn kwin_bus_address(&self) -> &str {
        &self.kwin_bus_address
    }

    /// Размер экрана в пикселях.
    pub fn screen(&self) -> ScreenSize {
        self.screen
    }
}
