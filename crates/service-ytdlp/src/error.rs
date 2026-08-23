use thiserror::Error;

use crate::locator::YtDlpLocatorParseError;

/// Typed ошибка generic `yt-dlp` service boundary.
///
/// Варианты намеренно сохраняют различия, важные для app composition:
/// пользовательская отмена, timeout, отказ extractor-а и ошибка media transport
/// не должны превращаться в один непрозрачный `anyhow::Error`.
#[derive(Debug, Error)]
pub enum YtDlpServiceError {
    /// Locator не является допустимым absolute network URL утверждённой схемы.
    #[error(transparent)]
    InvalidLocator(#[from] YtDlpLocatorParseError),

    /// Adapter отключён committed пользовательской конфигурацией.
    #[error("yt-dlp adapter отключён в настройках")]
    AdapterDisabled,

    /// Владелец фоновой задачи отменил extraction/open.
    #[error("операция yt-dlp отменена")]
    Cancellation,

    /// Внешний процесс не завершился за configured timeout.
    #[error("истекло время ожидания yt-dlp")]
    Timeout,

    /// Stdout single-item process-а пересёк configured byte budget.
    #[error("stdout yt-dlp превысил лимит {limit_bytes} bytes")]
    StdoutLimitExceeded {
        /// Точный configured предел без раскрытия output payload.
        limit_bytes: u64,
    },

    /// Stderr single-item process-а пересёк configured byte budget.
    #[error("stderr yt-dlp превысил лимит {limit_bytes} bytes")]
    StderrLimitExceeded {
        /// Точный configured предел без раскрытия diagnostic payload.
        limit_bytes: u64,
    },

    /// Валидный JSON пересёк configured structural node budget до построения DOM.
    #[error("JSON yt-dlp превысил structural лимит {limit_nodes} nodes")]
    JsonNodeLimitExceeded {
        /// Точный configured предел числа JSON values.
        limit_nodes: u64,
    },

    /// OS/process plumbing не позволил выполнить или корректно дождаться `yt-dlp`.
    #[error("не удалось выполнить системный yt-dlp")]
    ProcessFailure {
        /// Безопасная причина без locator/stderr payload.
        #[source]
        source: anyhow::Error,
    },

    /// Extractor завершился с non-zero status и отверг URL/media.
    #[error("yt-dlp extractor отклонил URL (stderr скрыт, {stderr_bytes} bytes)")]
    ExtractorRejection {
        /// Размер скрытого stderr нужен для диагностики без раскрытия payload.
        stderr_bytes: usize,
    },

    /// `yt-dlp` завершился успешно, но вернул невалидный JSON/UTF-8 contract.
    #[error("yt-dlp вернул некорректный metadata response")]
    InvalidExtractorResponse {
        /// Parser/encoding error не содержит исходный locator.
        #[source]
        source: anyhow::Error,
    },

    /// Single-item metadata resolver получил collection вместо одного media item.
    #[error("URL описывает коллекцию, а не один media item")]
    CollectionUrl,
}

impl YtDlpServiceError {
    /// Строит process failure, не позволяя callsite-ам смешать её с extractor rejection.
    pub(crate) fn process(source: impl Into<anyhow::Error>) -> Self {
        Self::ProcessFailure {
            source: source.into(),
        }
    }

    /// Строит invalid-response failure для UTF-8/JSON contract-а.
    pub(crate) fn invalid_response(source: impl Into<anyhow::Error>) -> Self {
        Self::InvalidExtractorResponse {
            source: source.into(),
        }
    }
}
