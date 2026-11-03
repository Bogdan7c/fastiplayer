//! Ошибки наблюдения за журналами.

use std::path::PathBuf;
use std::time::Duration;

/// Что пошло не так при чтении журналов плеера и посредника.
#[derive(Debug, thiserror::Error)]
pub enum ObserveError {
    /// Файл журнала не прочитался.
    #[error("cannot read log {path}: {source}")]
    Read {
        /// Путь журнала.
        path: PathBuf,
        /// Ошибка ОС.
        #[source]
        source: std::io::Error,
    },
    /// Ожидаемое не появилось в журнале за отведённое время.
    #[error("timed out after {waited:?} waiting for {what}; last relevant line: {last_seen:?}")]
    Timeout {
        /// Чего ждали.
        what: String,
        /// Сколько ждали.
        waited: Duration,
        /// Последняя подходящая по теме строка (например, последняя сводка), если была.
        last_seen: Option<String>,
    },
    /// Строка журнала с известным маркером, но не в ожидаемом формате (формат плеера изменился).
    #[error("malformed {what} line in the player log: {line:?}")]
    MalformedLine {
        /// Какую строку разбирали.
        what: &'static str,
        /// Строка как есть.
        line: String,
    },
}
