use thiserror::Error;

use crate::admission::YtDlpCompatibilityRejection;
use crate::locator::YtDlpLocatorParseError;

/// Typed ошибка generic `yt-dlp` service boundary.
///
/// Варианты намеренно сохраняют различия, важные для app composition:
/// пользовательская отмена, timeout, отказ extractor-а и ошибка media transport
/// не должны превращаться в один непрозрачный `anyhow::Error`.
#[derive(Debug, Error)]
pub enum YtDlpServiceError {
    /// Locator не является допустимым absolute HTTP(S) URL.
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

    /// URL описывает коллекцию, которую v1 не раскрывает в playback queue.
    #[error("URL описывает коллекцию, а не один media item")]
    CollectionUrl,

    /// Extractor metadata не содержит совместимую direct WebM VP9+Opus пару.
    #[error("yt-dlp не нашёл совместимые direct WebM VP9+Opus streams: {reason}")]
    NoCompatibleStreams {
        /// Typed admission-причина для UI/tests без parsing текста ошибки.
        reason: YtDlpCompatibilityRejection,
    },

    /// Direct HTTP source не удалось открыть/прочитать.
    #[error("не удалось открыть direct HTTP stream от yt-dlp")]
    TransportFailure {
        /// Secret-safe transport chain от `source-core`.
        #[source]
        source: anyhow::Error,
    },

    /// Совместимые byte streams не прошли demux/probe.
    #[error("не удалось открыть WebM demuxer для yt-dlp streams")]
    DemuxFailure {
        /// Demux chain использует только service-safe labels.
        #[source]
        source: anyhow::Error,
    },
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

    /// Строит transport failure с уже secret-safe source chain.
    pub(crate) fn transport(source: impl Into<anyhow::Error>) -> Self {
        Self::TransportFailure {
            source: source.into(),
        }
    }

    /// Строит demux failure с уже redacted source label.
    pub(crate) fn demux(source: impl Into<anyhow::Error>) -> Self {
        Self::DemuxFailure {
            source: source.into(),
        }
    }
}
