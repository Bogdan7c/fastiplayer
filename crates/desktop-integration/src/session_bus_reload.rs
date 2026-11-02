//! Просьба к сессионной шине «перечитай каталоги `.service`-файлов».
//!
//! Зачем: установщик моста браузера (crate `browser-bridge-install`) кладёт
//! `.service`-файл D-Bus activation в `~/.local/share/dbus-1/services/`. Шина,
//! запущенная раньше, о нём может не знать, и первый же звонок посредника получил бы
//! «службы нет». Стандартный метод `org.freedesktop.DBus.ReloadConfig` заставляет
//! шину заново просмотреть каталоги служб.
//!
//! Вызов идёт низкоуровневым `Connection::call_method` на соединении с
//! `method_timeout`: в zbus 5.15 `#[proxy]`-вызовы таймаут игнорируют (так же сделан
//! клиент входа в `browser_session_entry`).

use std::time::Duration;

use thiserror::Error;

use crate::browser_session_entry::BrowserEntryBus;

/// Почему шина не перечитала каталоги служб.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionBusReloadError {
    /// Нет соединения с шиной (нет сессионной шины или неверный адрес).
    #[error("D-Bus session bus not available: {0}")]
    SessionBusUnavailable(String),
    /// Шина не ответила за отведённое время.
    #[error("The D-Bus session bus did not answer the reload request in time")]
    NotResponding,
    /// Шина ответила отказом (например, политика не разрешает `ReloadConfig`).
    #[error("The D-Bus session bus refused to reload its service files: {detail}")]
    Rejected {
        /// Имя и текст ошибки шины, для лога.
        detail: String,
    },
    /// Прочая ошибка транспорта.
    #[error("D-Bus transport error during the reload request: {0}")]
    Transport(String),
    /// На этой платформе шины D-Bus нет.
    #[error("Reloading D-Bus service files is not supported on this platform.")]
    UnsupportedPlatform,
}

/// Просит шину перечитать каталоги `.service`-файлов. Блокирующий вызов, не дольше
/// `timeout` на сам запрос.
///
/// `bus` выбирает обычную сессионную шину или шину по адресу (последнее нужно
/// тестам с частным `dbus-daemon`).
pub fn reload_session_bus_services(
    bus: &BrowserEntryBus,
    timeout: Duration,
) -> Result<(), SessionBusReloadError> {
    #[cfg(target_os = "linux")]
    {
        linux::reload(bus, timeout)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (bus, timeout);
        Err(SessionBusReloadError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::Duration;

    use zbus::blocking::{Connection, connection};

    use super::SessionBusReloadError;
    use crate::browser_session_entry::BrowserEntryBus;

    const DBUS_SERVICE: &str = "org.freedesktop.DBus";
    const DBUS_PATH: &str = "/org/freedesktop/DBus";
    const DBUS_INTERFACE: &str = "org.freedesktop.DBus";
    const RELOAD_CONFIG_METHOD: &str = "ReloadConfig";

    /// Подключается к шине с таймаутом и вызывает `ReloadConfig`.
    pub(super) fn reload(
        bus: &BrowserEntryBus,
        timeout: Duration,
    ) -> Result<(), SessionBusReloadError> {
        reload_with(
            |method_timeout| open_connection(bus, method_timeout),
            timeout,
        )
    }

    /// То же через произвольный способ открыть соединение (шов для тестов:
    /// «шина замёрзла после подключения»).
    pub(super) fn reload_with(
        open_connection: impl FnOnce(Duration) -> zbus::Result<Connection>,
        timeout: Duration,
    ) -> Result<(), SessionBusReloadError> {
        let connection = open_connection(timeout)
            .map_err(|error| SessionBusReloadError::SessionBusUnavailable(error.to_string()))?;
        reload_on(&connection)
    }

    fn open_connection(bus: &BrowserEntryBus, timeout: Duration) -> zbus::Result<Connection> {
        match bus {
            BrowserEntryBus::Session => connection::Builder::session()?
                .method_timeout(timeout)
                .build(),
            BrowserEntryBus::Address(address) => connection::Builder::address(address.as_str())?
                .method_timeout(timeout)
                .build(),
        }
    }

    /// Сам вызов; результат метода пустой.
    fn reload_on(connection: &Connection) -> Result<(), SessionBusReloadError> {
        connection
            .call_method(
                Some(DBUS_SERVICE),
                DBUS_PATH,
                Some(DBUS_INTERFACE),
                RELOAD_CONFIG_METHOD,
                &(),
            )
            .map(|_empty_reply| ())
            .map_err(classify_error)
    }

    /// Таймаут — «не ответила», отказ шины — `Rejected`, остальное — транспорт.
    fn classify_error(error: zbus::Error) -> SessionBusReloadError {
        match &error {
            zbus::Error::InputOutput(io_error)
                if io_error.kind() == std::io::ErrorKind::TimedOut =>
            {
                SessionBusReloadError::NotResponding
            }
            zbus::Error::MethodError(name, detail, _reply) => SessionBusReloadError::Rejected {
                detail: format!("{name}: {}", detail.as_deref().unwrap_or_default()),
            },
            _ => SessionBusReloadError::Transport(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
