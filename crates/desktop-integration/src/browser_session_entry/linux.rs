//! Linux-транспорт входа браузерного режима: собственный интерфейс на сессионной D-Bus.
//!
//! Метод `Entry(s message) -> s reply` на объекте
//! [`super::BROWSER_SESSION_OBJECT_PATH`]. Запуск службы по требованию делает сама
//! шина по `.service`-файлу (D-Bus activation); клиент лишь просит об этом явно
//! вызовом `StartServiceByName`, чтобы отличить «не установлено» от «не поднялось».

mod client;
mod service;

pub(super) use client::{ClientBackend, connect};
pub(super) use service::{ServiceBackend, start_on_session_bus};

/// Полные имена ошибок D-Bus, которые служба возвращает звонящему.
///
/// Должны совпадать с `#[zbus(prefix)]` и именами вариантов
/// `service::EntryServiceFailure`; совпадение закреплено тестом.
const REJECTED_ERROR_NAME: &str = "io.github.Bogdan7c.Fastiplayer.BrowserSession.Error.Rejected";
const NOT_RESPONDING_ERROR_NAME: &str =
    "io.github.Bogdan7c.Fastiplayer.BrowserSession.Error.NotResponding";
const SHUTTING_DOWN_ERROR_NAME: &str =
    "io.github.Bogdan7c.Fastiplayer.BrowserSession.Error.ShuttingDown";

#[cfg(test)]
mod tests;
