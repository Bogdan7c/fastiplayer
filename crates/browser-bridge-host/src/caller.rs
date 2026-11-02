//! Кто запустил посредника: экстеншен определяется по аргументам командной строки.
//!
//! Браузеры передают его по-разному (документация Chrome и MDN, проверено вживую в
//! разведке 00):
//! - Chrome и Chromium (в том числе из Flatpak) — один аргумент
//!   `chrome-extension://<ID>/`; на Windows за ним идёт `--parent-window=<число>`;
//! - Firefox — два аргумента: путь к файлу подключения и ID экстеншена.
//!
//! Лишние открытые дескрипторы (fd 3 у Firefox, fd 235 у Chrome из Flatpak) и
//! текущий каталог посредник не использует.
//!
//! Чужой экстеншен — выход до любого обращения к шине (инвариант 3). Браузер и
//! сам не запустит посредника для экстеншена не из файла подключения, но
//! посредник не полагается на чужую проверку.

use std::ffi::OsString;

use browser_bridge_protocol::extension_identity::{BrowserFamily, is_allowed_extension};
use thiserror::Error;

/// Начало первого аргумента у Chromium.
const CHROMIUM_ORIGIN_PREFIX: &str = "chrome-extension://";

/// Начало дополнительного аргумента Chrome на Windows (ручка окна браузера).
const CHROMIUM_PARENT_WINDOW_PREFIX: &str = "--parent-window=";

/// Сколько аргументов передаёт Firefox: путь к файлу подключения и ID.
const FIREFOX_ARGUMENT_COUNT: usize = 2;

/// Проверенный вызывающий: наш экстеншен в этом семействе браузеров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionCaller {
    browser: BrowserFamily,
}

impl ExtensionCaller {
    /// Семейство браузера, запустившего посредника (для журнала).
    #[must_use]
    pub const fn browser(self) -> BrowserFamily {
        self.browser
    }
}

/// Почему вызывающий не принят. Сам ID в причину не попадает: журнал браузера
/// не место для чужих идентификаторов, а для разбора хватает вида причины.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CallerRejection {
    /// Аргументов нет: посредника запустили не из браузера.
    #[error("no caller argument: the host must be started by a browser")]
    NoCallerArgument,
    /// Аргументы не похожи ни на Chrome, ни на Firefox.
    #[error("caller arguments match neither Chromium nor Firefox")]
    UnrecognizedArguments,
    /// `chrome-extension://…` без завершающей `/` или с путём после ID.
    #[error("malformed Chromium extension origin")]
    MalformedChromiumOrigin,
    /// Экстеншен не из нашего списка.
    #[error("extension is not allowed for {browser:?}")]
    ForeignExtension {
        /// Семейство браузера, которое его передало.
        browser: BrowserFamily,
    },
}

/// Определяет вызывающего по аргументам (без нулевого — пути к бинарю).
///
/// # Errors
/// Аргументы не от браузера, неверной формы или экстеншен не наш.
pub fn identify_caller(arguments: &[OsString]) -> Result<ExtensionCaller, CallerRejection> {
    let first = arguments.first().ok_or(CallerRejection::NoCallerArgument)?;
    // Не-UTF-8 аргумент не может быть ни origin Chrome, ни ID Firefox.
    let first = first
        .to_str()
        .ok_or(CallerRejection::UnrecognizedArguments)?;
    let (browser, extension_id) = match first.strip_prefix(CHROMIUM_ORIGIN_PREFIX) {
        Some(origin_rest) => (
            BrowserFamily::Chromium,
            chromium_extension_id(origin_rest, &arguments[1..])?,
        ),
        None => (BrowserFamily::Firefox, firefox_extension_id(arguments)?),
    };
    if is_allowed_extension(browser, extension_id) {
        Ok(ExtensionCaller { browser })
    } else {
        Err(CallerRejection::ForeignExtension { browser })
    }
}

/// ID из `chrome-extension://<ID>/` (префикс уже снят) и проверка хвоста аргументов.
fn chromium_extension_id<'argument>(
    origin_rest: &'argument str,
    remaining_arguments: &[OsString],
) -> Result<&'argument str, CallerRejection> {
    let extension_id = origin_rest
        .strip_suffix('/')
        .filter(|extension_id| !extension_id.is_empty() && !extension_id.contains('/'))
        .ok_or(CallerRejection::MalformedChromiumOrigin)?;
    // Единственный известный лишний аргумент Chrome — ручка окна на Windows.
    let only_parent_window = remaining_arguments.iter().all(|argument| {
        argument
            .to_str()
            .is_some_and(|argument| argument.starts_with(CHROMIUM_PARENT_WINDOW_PREFIX))
    });
    if only_parent_window {
        Ok(extension_id)
    } else {
        Err(CallerRejection::UnrecognizedArguments)
    }
}

/// ID из аргументов Firefox: ровно `[путь к файлу подключения, ID]`.
fn firefox_extension_id(arguments: &[OsString]) -> Result<&str, CallerRejection> {
    if arguments.len() != FIREFOX_ARGUMENT_COUNT {
        return Err(CallerRejection::UnrecognizedArguments);
    }
    arguments
        .get(1)
        .and_then(|extension_id| extension_id.to_str())
        .filter(|extension_id| !extension_id.is_empty())
        .ok_or(CallerRejection::UnrecognizedArguments)
}

#[cfg(test)]
mod tests;
