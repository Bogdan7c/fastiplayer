//! Provider-neutral сигнал истечения физического web-media endpoint-а.
//!
//! Этот модуль намеренно не знает про yt-dlp, player, UI или стратегию повторного
//! открытия. Transport и adaptive sources только сообщают факт; владелец logical
//! media решает, можно ли и как переизвлечь весь candidate.

use crate::{MediaComponentIdentity, SourceGeneration};

/// Семантическая причина, по которой физический endpoint больше нельзя использовать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointExpiryReason {
    /// Сервер отверг прежний authorization material кодом `401` или `403`.
    AuthorizationExpired,
    /// Сервер сообщил, что прежний ресурс отсутствует или удалён, кодом `404` или `410`.
    ResourceExpired,
}

impl EndpointExpiryReason {
    /// Классифицирует только статусы, допускающие logical-source re-extraction.
    #[must_use]
    pub const fn from_http_status(status: u16) -> Option<Self> {
        match status {
            401 | 403 => Some(Self::AuthorizationExpired),
            404 | 410 => Some(Self::ResourceExpired),
            _ => None,
        }
    }
}

/// Тип физического ресурса без URL, headers, cookies и provider-specific payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointExpiryResourceKind {
    /// Позднее чтение seekable progressive HTTP source-а.
    ProgressiveRange,
    /// Master/media manifest либо presentation manifest.
    Manifest,
    /// Внешний synchronization clock.
    ClockSynchronization,
    /// Media segment или fragment.
    MediaSegment,
    /// Initialization section/resource.
    Initialization,
    /// Encryption key.
    EncryptionKey,
}

/// Один generation-fenced факт истечения физического endpoint-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointExpirySignal {
    /// Exact component identity нужна logical owner-у для diagnostics и A/V coalescing.
    component: MediaComponentIdentity,
    /// Generation не позволяет старому runtime инициировать recovery после replacement-а.
    source_generation: SourceGeneration,
    /// Resource class не раскрывает физический locator.
    resource_kind: EndpointExpiryResourceKind,
    /// Typed expiry taxonomy сохраняет различие authorization/resource lifecycle.
    reason: EndpointExpiryReason,
}

impl EndpointExpirySignal {
    /// Создаёт secret-safe signal из уже проверенных transport identities.
    #[must_use]
    pub fn new(
        component: MediaComponentIdentity,
        source_generation: SourceGeneration,
        resource_kind: EndpointExpiryResourceKind,
        reason: EndpointExpiryReason,
    ) -> Self {
        Self {
            component,
            source_generation,
            resource_kind,
            reason,
        }
    }

    /// Возвращает exact component identity без физического request material.
    #[must_use]
    pub const fn component(&self) -> &MediaComponentIdentity {
        &self.component
    }

    /// Возвращает generation runtime-а, который увидел expiry.
    #[must_use]
    pub const fn source_generation(&self) -> SourceGeneration {
        self.source_generation
    }

    /// Возвращает тип истёкшего ресурса.
    #[must_use]
    pub const fn resource_kind(&self) -> EndpointExpiryResourceKind {
        self.resource_kind
    }

    /// Возвращает семантическую причину expiry.
    #[must_use]
    pub const fn reason(&self) -> EndpointExpiryReason {
        self.reason
    }
}

/// App-owned observer, которому transport сообщает expiry без знания recovery policy.
pub trait EndpointExpiryObserver: Send + Sync {
    /// Публикует один generation-fenced signal; реализация обязана быть неблокирующей.
    fn observe_endpoint_expiry(&self, signal: EndpointExpirySignal);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Только четыре оговорённых статуса должны запускать logical-source recovery.
    #[test]
    fn expiry_reason_classifies_only_signed_endpoint_statuses() {
        assert_eq!(
            EndpointExpiryReason::from_http_status(401),
            Some(EndpointExpiryReason::AuthorizationExpired)
        );
        assert_eq!(
            EndpointExpiryReason::from_http_status(403),
            Some(EndpointExpiryReason::AuthorizationExpired)
        );
        assert_eq!(
            EndpointExpiryReason::from_http_status(404),
            Some(EndpointExpiryReason::ResourceExpired)
        );
        assert_eq!(
            EndpointExpiryReason::from_http_status(410),
            Some(EndpointExpiryReason::ResourceExpired)
        );
        assert_eq!(EndpointExpiryReason::from_http_status(400), None);
        assert_eq!(EndpointExpiryReason::from_http_status(408), None);
        assert_eq!(EndpointExpiryReason::from_http_status(429), None);
        assert_eq!(EndpointExpiryReason::from_http_status(500), None);
    }
}
