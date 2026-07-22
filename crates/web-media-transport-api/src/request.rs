//! Typed open/refresh requests и generation fences.

use std::fmt;
use std::num::NonZeroU64;

use source_core::{CancellationToken, HttpRequestTarget};

use crate::{
    MediaComponentIdentity, RedirectPolicy, SecretRequestContext, SourceGeneration,
    TransportProviderId,
};

/// Media timeline nature, независимая от byte seekability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPresentation {
    /// Конечный media resource.
    Vod,
    /// Обновляемый live resource.
    Live,
}

/// Верхняя граница одного HTTP Range-запроса для конкретного media source.
///
/// Тип описывает транспортное намерение и не хранит provider-specific config:
/// HTTP owner сам решает, как совместить этот предел со своей prefetch policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpRangeRequestLimit {
    /// Проверенный ненулевой предел в bytes.
    maximum_bytes: NonZeroU64,
}

impl HttpRangeRequestLimit {
    /// Создаёт предел, запрещая бессмысленный нулевой Range-запрос.
    pub const fn new(maximum_bytes: u64) -> Result<Self, HttpRangeRequestLimitError> {
        let Some(maximum_bytes) = NonZeroU64::new(maximum_bytes) else {
            return Err(HttpRangeRequestLimitError::Zero);
        };
        Ok(Self { maximum_bytes })
    }

    /// Возвращает максимальный размер одного Range-запроса в bytes.
    #[must_use]
    pub const fn maximum_bytes(self) -> u64 {
        self.maximum_bytes.get()
    }
}

/// Ошибка построения source-specific HTTP Range policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HttpRangeRequestLimitError {
    /// Нулевой предел не может породить корректный byte range.
    #[error("HTTP Range request limit должен быть больше нуля")]
    Zero,
}

/// Exact identity успешно открытого runtime component-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedComponentIdentity {
    /// Concrete provider owner.
    provider: TransportProviderId,
    /// Exact + semantic candidate identity.
    component: MediaComponentIdentity,
    /// Runtime source generation.
    source_generation: SourceGeneration,
}

impl OpenedComponentIdentity {
    /// Создаёт identity для explicit runtime fence/reconstruction handoff.
    #[must_use]
    pub fn new(
        provider: TransportProviderId,
        component: MediaComponentIdentity,
        source_generation: SourceGeneration,
    ) -> Self {
        Self {
            provider,
            component,
            source_generation,
        }
    }

    /// Возвращает provider owner-а.
    #[must_use]
    pub const fn provider(&self) -> &TransportProviderId {
        &self.provider
    }

    /// Возвращает component identity.
    #[must_use]
    pub const fn component(&self) -> &MediaComponentIdentity {
        &self.component
    }

    /// Возвращает runtime source generation.
    #[must_use]
    pub const fn source_generation(&self) -> SourceGeneration {
        self.source_generation
    }
}

/// Owned request первого открытия component-а.
#[derive(Clone)]
pub struct TransportOpenRequest {
    /// Exact selected provider.
    provider: TransportProviderId,
    /// Exact/semantic/role identity.
    component: MediaComponentIdentity,
    /// Exact secret target + validated policy attributes.
    target: HttpRequestTarget,
    /// Expected timeline nature.
    presentation: MediaPresentation,
    /// Runtime generation, назначенная composition owner-ом.
    source_generation: SourceGeneration,
    /// Ephemeral scoped request material.
    secrets: SecretRequestContext,
    /// Bounded redirect policy.
    redirects: RedirectPolicy,
    /// Optional source-specific верхняя граница одного HTTP Range-запроса.
    http_range_request_limit: Option<HttpRangeRequestLimit>,
    /// Shared cooperative cancellation.
    cancellation: CancellationToken,
}

impl TransportOpenRequest {
    /// Создаёт request и доказывает, что non-empty secret context покрывает initial target.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: TransportProviderId,
        component: MediaComponentIdentity,
        target: HttpRequestTarget,
        presentation: MediaPresentation,
        source_generation: SourceGeneration,
        secrets: SecretRequestContext,
        redirects: RedirectPolicy,
        cancellation: CancellationToken,
    ) -> Result<Self, TransportOpenRequestError> {
        if !secrets.is_empty() && !secrets.scope().allows(&target) {
            return Err(TransportOpenRequestError::InitialTargetOutsideSecretScope);
        }
        Ok(Self {
            provider,
            component,
            target,
            presentation,
            source_generation,
            secrets,
            redirects,
            http_range_request_limit: None,
            cancellation,
        })
    }

    /// Добавляет проверенную source-specific HTTP Range policy.
    #[must_use]
    pub fn with_http_range_request_limit(
        mut self,
        http_range_request_limit: HttpRangeRequestLimit,
    ) -> Self {
        self.http_range_request_limit = Some(http_range_request_limit);
        self
    }

    /// Возвращает exact provider selection.
    #[must_use]
    pub const fn provider(&self) -> &TransportProviderId {
        &self.provider
    }

    /// Возвращает exact component identity.
    #[must_use]
    pub const fn component(&self) -> &MediaComponentIdentity {
        &self.component
    }

    /// Возвращает checked target.
    #[must_use]
    pub const fn target(&self) -> &HttpRequestTarget {
        &self.target
    }

    /// Возвращает expected presentation nature.
    #[must_use]
    pub const fn presentation(&self) -> MediaPresentation {
        self.presentation
    }

    /// Возвращает assigned runtime generation.
    #[must_use]
    pub const fn source_generation(&self) -> SourceGeneration {
        self.source_generation
    }

    /// Возвращает ephemeral secret context для per-request scope checks.
    #[must_use]
    pub const fn secrets(&self) -> &SecretRequestContext {
        &self.secrets
    }

    /// Возвращает redirect policy.
    #[must_use]
    pub const fn redirects(&self) -> RedirectPolicy {
        self.redirects
    }

    /// Возвращает optional предел одного HTTP Range-запроса для source-а.
    #[must_use]
    pub const fn http_range_request_limit(&self) -> Option<HttpRangeRequestLimit> {
        self.http_range_request_limit
    }

    /// Возвращает shared cancellation token.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Строит identity результата только из caller-owned exact fields.
    #[must_use]
    pub(crate) fn opened_identity(&self) -> OpenedComponentIdentity {
        OpenedComponentIdentity::new(
            self.provider.clone(),
            self.component.clone(),
            self.source_generation,
        )
    }
}

impl fmt::Debug for TransportOpenRequest {
    /// Target/context используют собственный secret-safe Debug.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportOpenRequest")
            .field("provider", &self.provider)
            .field("component", &self.component)
            .field("target", &self.target)
            .field("presentation", &self.presentation)
            .field("source_generation", &self.source_generation)
            .field("secrets", &self.secrets)
            .field("redirects", &self.redirects)
            .field("http_range_request_limit", &self.http_range_request_limit)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish()
    }
}

/// Ошибка построения open request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportOpenRequestError {
    /// Non-empty context не покрывает initial request target.
    #[error("initial target находится вне secret request scope")]
    InitialTargetOutsideSecretScope,
}

/// Owned refresh request: старый runtime fence + новое exact request material.
pub struct TransportRefreshRequest {
    /// Identity generation, которую caller намерен заменить.
    previous: OpenedComponentIdentity,
    /// Полный replacement open request с новой generation/material.
    replacement: TransportOpenRequest,
}

impl TransportRefreshRequest {
    /// Проверяет provider, semantic identity, role и monotonic generation.
    pub fn new(
        previous: OpenedComponentIdentity,
        replacement: TransportOpenRequest,
    ) -> Result<Self, TransportRefreshRequestError> {
        if previous.provider() != replacement.provider() {
            return Err(TransportRefreshRequestError::ProviderChanged);
        }
        if previous.component().semantic() != replacement.component().semantic() {
            return Err(TransportRefreshRequestError::SemanticIdentityChanged);
        }
        if previous.component().role() != replacement.component().role() {
            return Err(TransportRefreshRequestError::ComponentRoleChanged);
        }
        if replacement.source_generation() <= previous.source_generation() {
            return Err(TransportRefreshRequestError::GenerationNotNewer);
        }
        Ok(Self {
            previous,
            replacement,
        })
    }

    /// Возвращает exact previous runtime identity.
    #[must_use]
    pub const fn previous(&self) -> &OpenedComponentIdentity {
        &self.previous
    }

    /// Возвращает replacement request.
    #[must_use]
    pub const fn replacement(&self) -> &TransportOpenRequest {
        &self.replacement
    }

    /// Возвращает cancellation token replacement request-а.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        self.replacement.cancellation()
    }
}

impl fmt::Debug for TransportRefreshRequest {
    /// Оба вложенных contracts имеют secret-safe Debug.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportRefreshRequest")
            .field("previous", &self.previous)
            .field("replacement", &self.replacement)
            .finish()
    }
}

/// Ошибка refresh identity contract-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportRefreshRequestError {
    /// Refresh не может молча сменить concrete provider-а.
    #[error("refresh replacement меняет transport provider")]
    ProviderChanged,
    /// Refresh обязан сохранить semantic identity.
    #[error("refresh replacement меняет semantic component identity")]
    SemanticIdentityChanged,
    /// Refresh обязан сохранить component role.
    #[error("refresh replacement меняет component role")]
    ComponentRoleChanged,
    /// Replacement generation обязана быть строго новее.
    #[error("refresh source generation не новее предыдущей")]
    GenerationNotNewer,
}
