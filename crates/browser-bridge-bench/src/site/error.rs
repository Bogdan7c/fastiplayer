//! Ошибки сервера стенда.

use std::time::Duration;

use super::run_id::RunId;

#[derive(Debug, thiserror::Error)]
pub enum SiteError {
    /// Не удалось занять порт слушателя.
    #[error("cannot bind the {role} listener on {address}: {source}")]
    Bind {
        role: &'static str,
        address: String,
        #[source]
        source: std::io::Error,
    },
    /// Прочая ошибка ввода-вывода при запуске сервера.
    #[error("cannot {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// Строка не годится в идентификатор прогона.
    #[error("invalid run id '{0}': expected 1..=64 characters of [a-z0-9-]")]
    InvalidRunId(String),
    /// Страница не прислала отчёт за отведённое время.
    #[error("no report for run '{run}' within {waited:?}")]
    ReportTimeout { run: RunId, waited: Duration },
    /// Страница не написала ожидаемую строку журнала за отведённое время.
    #[error("run '{run}' did not log a line containing '{needle}' within {waited:?}")]
    LogLineTimeout {
        run: RunId,
        needle: String,
        waited: Duration,
    },
}
