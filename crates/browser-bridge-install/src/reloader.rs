//! Шов «попросить сессионную шину перечитать файлы служб».
//!
//! После записи или удаления `.service` запущенная шина может не знать об изменении.
//! Установщик зависит от trait [`SessionBusReloader`]: в приложении стоит
//! [`DesktopSessionBusReloader`] поверх `desktop-integration`, в тестах — фейк.

use std::time::Duration;

use desktop_integration::{BrowserEntryBus, SessionBusReloadError, reload_session_bus_services};

/// Сколько ждать ответа шины на `ReloadConfig`.
const DEFAULT_RELOAD_TIMEOUT: Duration = Duration::from_secs(3);

/// Просит шину перечитать каталоги служб.
pub trait SessionBusReloader: Send + Sync {
    /// Перечитать `.service`-файлы. Сбой не фатален для установки: он превращается
    /// в предупреждение (шина перечитает файлы при следующем запуске сеанса).
    fn reload_service_files(&self) -> Result<(), SessionBusReloadError>;
}

/// Настоящая шина сеанса пользователя.
#[derive(Debug, Clone, Copy)]
pub struct DesktopSessionBusReloader {
    timeout: Duration,
}

impl DesktopSessionBusReloader {
    /// С таймаутом по умолчанию.
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_RELOAD_TIMEOUT,
        }
    }
}

impl Default for DesktopSessionBusReloader {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionBusReloader for DesktopSessionBusReloader {
    fn reload_service_files(&self) -> Result<(), SessionBusReloadError> {
        reload_session_bus_services(&BrowserEntryBus::Session, self.timeout)
    }
}
