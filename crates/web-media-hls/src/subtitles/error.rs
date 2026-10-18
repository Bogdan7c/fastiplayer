//! Типизированные ошибки транспорта субтитров без URL и query в тексте.

use source_core::SourceError;
use web_media_adaptive::AdaptiveTransportError;

/// Возможность, которую лёгкий профиль субтитров сознательно не поддерживает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsSubtitleUnsupportedFeature {
    /// Playlist шифрует сегменты (`EXT-X-KEY` с `METHOD` не `NONE`).
    Encryption,
}

/// Почему playlist или сегмент субтитров не удалось получить.
///
/// Варианты намеренно различимы: потребитель по-разному реагирует на «нет такого ресурса»,
/// «сервер недоступен», «отмена» и «ресурс слишком большой». Текст не содержит URL.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HlsSubtitleFetchError {
    /// Идентификатор не относится к этому транспорту (чужой или устаревший список дорожек).
    #[error("subtitle rendition is not known to this transport")]
    UnknownRendition,
    /// Вызывающий или владелец источника отменил операцию.
    #[error("subtitle request was cancelled")]
    Cancelled,
    /// HTTP 404.
    #[error("subtitle resource was not found (HTTP 404)")]
    NotFound,
    /// HTTP 410.
    #[error("subtitle resource is gone (HTTP 410)")]
    Gone,
    /// Любой другой неуспешный HTTP-статус.
    #[error("subtitle server answered HTTP {status}")]
    HttpStatus {
        /// Числовой статус ответа.
        status: u16,
    },
    /// Сетевая ошибка (соединение, таймаут, обрыв тела) после встроенных повторов.
    #[error("subtitle network request failed")]
    Network,
    /// Запрос заблокирован политикой redirect/secret-scope общего HTTP-контекста.
    #[error("subtitle request was rejected by the transport policy")]
    RequestRejected,
    /// Ресурс превысил разрешённый лимит байт.
    #[error("subtitle resource exceeds the {limit_bytes} byte limit")]
    TooLarge {
        /// Лимит, который применялся к запросу.
        limit_bytes: usize,
    },
    /// Playlist не разобрался или не является media playlist.
    #[error("subtitle playlist is malformed")]
    MalformedPlaylist,
    /// Ссылка внутри master/media playlist не превращается в HTTP(S) адрес.
    #[error("subtitle reference could not be resolved")]
    InvalidReference,
    /// Лёгкий профиль не поддерживает возможность playlist-а.
    #[error("subtitle playlist uses an unsupported feature: {0:?}")]
    Unsupported(HlsSubtitleUnsupportedFeature),
    /// Ссылка на сегмент или контекст принадлежит другому/устаревшему транспорту либо поколению.
    #[error("subtitle transport or generation is stale")]
    Stale,
}

impl From<AdaptiveTransportError> for HlsSubtitleFetchError {
    /// Сжимает ошибку общего HTTP-контекста до безопасных вариантов; URL внутри не копируются.
    fn from(error: AdaptiveTransportError) -> Self {
        match error {
            AdaptiveTransportError::Cancelled
            | AdaptiveTransportError::RestartableReadInterrupted => Self::Cancelled,
            AdaptiveTransportError::StaleGeneration { .. } => Self::Stale,
            AdaptiveTransportError::Target(_) => Self::InvalidReference,
            AdaptiveTransportError::Redirect(_)
            | AdaptiveTransportError::ResourceBoundExceeded { .. }
            | AdaptiveTransportError::SecretScopeRejected
            | AdaptiveTransportError::ExplicitCookieHeader
            | AdaptiveTransportError::InvalidResourcePolicy { .. } => Self::RequestRejected,
            AdaptiveTransportError::WorkerStopped => Self::Network,
            AdaptiveTransportError::Source(source) => Self::from(&source),
        }
    }
}

impl From<&SourceError> for HlsSubtitleFetchError {
    fn from(error: &SourceError) -> Self {
        match error {
            SourceError::Cancelled | SourceError::SourceClosed => Self::Cancelled,
            SourceError::HttpStatus { status, .. } => match status.as_u16() {
                404 => Self::NotFound,
                410 => Self::Gone,
                other => Self::HttpStatus { status: other },
            },
            SourceError::HttpBodyTooLarge { maximum_bytes, .. } => Self::TooLarge {
                limit_bytes: *maximum_bytes,
            },
            _ => Self::Network,
        }
    }
}
