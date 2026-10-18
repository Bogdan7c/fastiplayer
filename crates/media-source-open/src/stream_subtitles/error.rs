//! Типизированные ошибки транспорта субтитров потокового источника.
//!
//! Ни один вариант не несёт URL, query или заголовки: текст ошибки безопасно попадает в лог.

use std::fmt;

/// Возможность, которую транспорт субтитров сознательно не поддерживает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSubtitleUnsupported {
    /// Сегменты зашифрованы.
    Encryption,
    /// Формат или контейнер сегмента не поддержан этим транспортом.
    SegmentFormat,
}

impl fmt::Display for StreamSubtitleUnsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Encryption => "encrypted segments",
            Self::SegmentFormat => "segment format",
        })
    }
}

/// Почему индекс или сегмент субтитров получить не удалось.
///
/// Варианты различимы намеренно: ленте важно отличать «ресурса нет» от «сеть упала»,
/// «ресурс слишком большой» от «данные битые» и отмену от всего остального.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StreamSubtitleError {
    /// Идентификатор дорожки не относится к этому транспорту.
    #[error("subtitle track is not known to this transport")]
    UnknownTrack,
    /// Ресурса нет (404/410 и прочие постоянные отказы сервера).
    #[error("subtitle resource was not found")]
    NotFound,
    /// Сеть/сервер временно недоступны (после встроенных повторов HTTP).
    #[error("subtitle network request failed")]
    Network,
    /// Запрос заблокирован политикой транспорта (redirect, область секретов).
    #[error("subtitle request was rejected by the transport policy")]
    Rejected,
    /// Ресурс больше разрешённого лимита.
    #[error("subtitle resource exceeds the {limit_bytes} byte limit")]
    TooLarge {
        /// Лимит, который применялся к запросу.
        limit_bytes: usize,
    },
    /// Данные (плейлист, сегмент, время) разобрать нельзя.
    #[error("subtitle data is malformed")]
    Malformed,
    /// Возможность не поддерживается.
    #[error("subtitle stream uses an unsupported feature: {0}")]
    Unsupported(StreamSubtitleUnsupported),
    /// Операцию отменили (лента закрыта или источник закрыт).
    #[error("subtitle request was cancelled")]
    Cancelled,
    /// Транспорт или поколение источника устарели.
    #[error("subtitle transport or generation is stale")]
    Stale,
}

/// Грубая причина сбоя без данных: годится для сообщений ленты и логов.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSubtitleFailureKind {
    /// Неизвестная дорожка.
    UnknownTrack,
    /// Ресурса нет.
    NotFound,
    /// Сеть недоступна.
    Network,
    /// Отказ политики транспорта.
    Rejected,
    /// Ресурс слишком большой.
    TooLarge,
    /// Битые данные.
    Malformed,
    /// Возможность не поддержана.
    Unsupported,
    /// Устаревший транспорт.
    Stale,
}

impl StreamSubtitleError {
    /// Отмена - не сбой: лента при ней молча завершается.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Сбой может пройти сам при повторе позже (сеть), в отличие от постоянных
    /// (нет ресурса, битые данные, неподдерживаемое).
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Network)
    }

    /// Причина без полезной нагрузки; `None` для отмены.
    #[must_use]
    pub const fn failure_kind(&self) -> Option<StreamSubtitleFailureKind> {
        Some(match self {
            Self::UnknownTrack => StreamSubtitleFailureKind::UnknownTrack,
            Self::NotFound => StreamSubtitleFailureKind::NotFound,
            Self::Network => StreamSubtitleFailureKind::Network,
            Self::Rejected => StreamSubtitleFailureKind::Rejected,
            Self::TooLarge { .. } => StreamSubtitleFailureKind::TooLarge,
            Self::Malformed => StreamSubtitleFailureKind::Malformed,
            Self::Unsupported(_) => StreamSubtitleFailureKind::Unsupported,
            Self::Stale => StreamSubtitleFailureKind::Stale,
            Self::Cancelled => return None,
        })
    }
}
