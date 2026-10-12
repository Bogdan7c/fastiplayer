use thiserror::Error;

use crate::locator::YtDlpLocatorParseError;
use crate::rejection_reason::YtDlpRejectionReason;

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
    #[error("yt-dlp adapter disabled in settings")]
    AdapterDisabled,

    /// Владелец фоновой задачи отменил extraction/open.
    #[error("yt-dlp operation canceled")]
    Cancellation,

    /// Внешний процесс не завершился за configured timeout.
    #[error("The waiting time has expired yt-dlp")]
    Timeout,

    /// Stdout single-item process-а пересёк configured byte budget.
    #[error("stdout yt-dlp exceeds {limit_bytes} bytes limit")]
    StdoutLimitExceeded {
        /// Точный configured предел без раскрытия output payload.
        limit_bytes: u64,
    },

    /// Stderr single-item process-а пересёк configured byte budget.
    #[error("stderr yt-dlp exceeds {limit_bytes} bytes limit")]
    StderrLimitExceeded {
        /// Точный configured предел без раскрытия diagnostic payload.
        limit_bytes: u64,
    },

    /// Валидный JSON пересёк configured structural node budget до построения DOM.
    #[error("JSON yt-dlp exceeds structural limit of {limit_nodes} nodes")]
    JsonNodeLimitExceeded {
        /// Точный configured предел числа JSON values.
        limit_nodes: u64,
    },

    /// Программа `yt-dlp` не найдена (запуск вернул `NotFound`): не установлена или
    /// отсутствует в `PATH`. Отдельно от `ProcessFailure`, потому что пользователю нужна
    /// конкретная подсказка — установить yt-dlp.
    #[error("system yt-dlp not found")]
    ExecutableNotFound,

    /// OS/process plumbing не позволил выполнить или корректно дождаться `yt-dlp`.
    #[error("Failed to execute system yt-dlp")]
    ProcessFailure {
        /// Безопасная причина без locator/stderr payload.
        #[source]
        source: anyhow::Error,
    },

    /// Extractor завершился с non-zero status и отверг URL/media.
    #[error("yt-dlp extractor rejected URL (cause: {reason}, stderr hidden, {stderr_bytes} bytes)")]
    ExtractorRejection {
        /// Размер скрытого stderr нужен для диагностики без раскрытия payload.
        stderr_bytes: usize,
        /// Причина, распознанная по строкам `ERROR:` (сам текст не хранится).
        reason: YtDlpRejectionReason,
    },

    /// `yt-dlp` завершился успешно, но вернул невалидный JSON/UTF-8 contract.
    #[error("yt-dlp returned an incorrect metadata response.")]
    InvalidExtractorResponse {
        /// Parser/encoding error не содержит исходный locator.
        #[source]
        source: anyhow::Error,
    },

    /// Single-item metadata resolver получил collection вместо одного media item.
    #[error("The URL describes a collection, not a single media item.")]
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
