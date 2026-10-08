//! Linux-транспорт пересылки: `org.freedesktop.Application` на сессионной D-Bus.
//!
//! Используется стандартный интерфейс из Desktop Entry Specification (раздел
//! «D-Bus Activation»): `Activate(a{sv})`, `Open(as, a{sv})`,
//! `ActivateAction(s, av, a{sv})`. Так его в будущем сможет вызывать и сам рабочий
//! стол (`DBusActivatable=true` в `.desktop`), без второго процесса.
//!
//! Билет активации окна по спецификации передаётся в `platform_data` под ключом
//! `activation-token` (Wayland) или `desktop-startup-id` (X11).

mod client;
mod service;

pub(super) use client::forward_on_session_bus;
pub(super) use service::{ServiceBackend, start_on_session_bus};

/// Объектный путь по правилу спецификации: ID приложения, точки → `/`.
const APPLICATION_OBJECT_PATH: &str = "/io/github/Bogdan7c/Fastiplayer";

/// Ключ `platform_data` с билетом активации Wayland (`XDG_ACTIVATION_TOKEN`).
const ACTIVATION_TOKEN_PLATFORM_KEY: &str = "activation-token";
/// Ключ `platform_data` с билетом запуска X11 (`DESKTOP_STARTUP_ID`).
const DESKTOP_STARTUP_ID_PLATFORM_KEY: &str = "desktop-startup-id";

/// Полные имена ошибок D-Bus, которые служба возвращает отправителю.
///
/// Должны совпадать с `#[zbus(prefix)]` и именами вариантов
/// `service::ForwardingServiceFailure`; совпадение закреплено тестом.
const REJECTED_ERROR_NAME: &str = "io.github.Bogdan7c.Fastiplayer.Error.Rejected";
const NOT_RESPONDING_ERROR_NAME: &str = "io.github.Bogdan7c.Fastiplayer.Error.NotResponding";
const SHUTTING_DOWN_ERROR_NAME: &str = "io.github.Bogdan7c.Fastiplayer.Error.ShuttingDown";

#[cfg(test)]
mod tests;
