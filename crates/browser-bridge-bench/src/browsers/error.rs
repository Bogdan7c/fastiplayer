//! Ошибки запуска и управления браузерами.

use std::time::Duration;

/// Что пошло не так при работе с браузером под тестом.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    /// Браузер не запустился.
    #[error("cannot start {program}: {source}")]
    Spawn {
        /// Путь или имя программы.
        program: String,
        /// Ошибка ОС.
        #[source]
        source: std::io::Error,
    },
    /// Ошибка файла или сети на нашей стороне.
    #[error("{action}: {source}")]
    Io {
        /// Что делали.
        action: String,
        /// Ошибка ОС.
        #[source]
        source: std::io::Error,
    },
    /// Не дождались события.
    #[error("timed out after {waited:?} waiting for {what}")]
    Timeout {
        /// Чего ждали.
        what: String,
        /// Сколько ждали.
        waited: Duration,
    },
    /// Браузер ответил не так, как описывает его протокол отладки.
    #[error("{protocol} protocol error: {message}")]
    Protocol {
        /// `DevTools` или `Marionette`.
        protocol: &'static str,
        /// Подробности.
        message: String,
    },
    /// Браузер отказал команде.
    #[error("{protocol} command {command} failed: {message}")]
    CommandFailed {
        /// `DevTools` или `Marionette`.
        protocol: &'static str,
        /// Имя команды.
        command: String,
        /// Текст отказа.
        message: String,
    },
    /// Такой вид расширения этот браузер не принимает.
    #[error("{0}")]
    UnsupportedExtension(String),
    /// Робот «отдать вкладку» не получил исход от фона экстеншена.
    #[error("hand off: {0}")]
    HandOff(#[from] super::hand_off::HandOffError),
    /// Страница расширения так и не показала текст.
    #[error("extension page {url} stayed empty for {waited:?}")]
    ExtensionPageEmpty {
        /// Адрес страницы.
        url: String,
        /// Сколько ждали.
        waited: Duration,
    },
}

impl BrowserError {
    /// Короткий конструктор ошибки протокола.
    pub(crate) fn protocol(protocol: &'static str, message: impl Into<String>) -> Self {
        Self::Protocol {
            protocol,
            message: message.into(),
        }
    }

    /// Короткий конструктор ошибки ввода-вывода.
    pub(crate) fn io(action: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            action: action.into(),
            source,
        }
    }
}
