//! Concrete provider boundary и typed operational outcomes.

use source_core::{HttpRequestTarget, HttpScheme};

use crate::TransportProviderId;
use crate::{
    MediaPresentation, RedirectHopCount, TransportInput, TransportOpenRequest,
    TransportRefreshRequest,
};

/// Способность provider-а обновлять expiring request material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshSupport {
    /// Provider не реализует refresh.
    Unsupported,
    /// Provider реализует exact identity/generation refresh.
    Supported,
}

/// Immutable registration descriptor concrete provider-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    /// Canonical provider identity.
    provider_id: TransportProviderId,
    /// Exact admitted HTTP schemes.
    schemes: Box<[HttpScheme]>,
    /// Refresh capability.
    refresh: RefreshSupport,
}

impl ProviderDescriptor {
    /// Проверяет non-empty duplicate-free capability snapshot.
    pub fn new(
        provider_id: TransportProviderId,
        schemes: Vec<HttpScheme>,
        refresh: RefreshSupport,
    ) -> Result<Self, ProviderDescriptorError> {
        if schemes.is_empty() {
            return Err(ProviderDescriptorError::MissingSchemes);
        }
        let mut unique_schemes = schemes.clone();
        unique_schemes.sort_unstable();
        unique_schemes.dedup();
        if unique_schemes.len() != schemes.len() {
            return Err(ProviderDescriptorError::DuplicateScheme);
        }
        Ok(Self {
            provider_id,
            schemes: schemes.into_boxed_slice(),
            refresh,
        })
    }

    /// Возвращает canonical provider ID.
    #[must_use]
    pub const fn provider_id(&self) -> &TransportProviderId {
        &self.provider_id
    }

    /// Проверяет exact scheme capability.
    #[must_use]
    pub fn supports_scheme(&self, scheme: HttpScheme) -> bool {
        self.schemes.contains(&scheme)
    }

    /// Возвращает immutable scheme snapshot.
    #[must_use]
    pub fn schemes(&self) -> &[HttpScheme] {
        &self.schemes
    }

    /// Возвращает refresh capability.
    #[must_use]
    pub const fn refresh_support(&self) -> RefreshSupport {
        self.refresh
    }
}

/// Ошибка provider descriptor-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderDescriptorError {
    /// Provider без scheme capability никогда не может быть выбран.
    #[error("transport provider не объявил HTTP schemes")]
    MissingSchemes,
    /// Scheme rows должны быть unique.
    #[error("transport provider повторно объявил HTTP scheme")]
    DuplicateScheme,
}

/// Typed unsupported outcome до/во время open-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UnsupportedTransportReason {
    /// Provider не поддерживает target scheme.
    #[error("target scheme не поддерживается provider-ом")]
    Scheme,
    /// Provider не поддерживает VOD/live presentation.
    #[error("media presentation не поддерживается provider-ом")]
    Presentation,
    /// Provider не может дать требуемую seekability shape.
    #[error("требуемая seekability не поддерживается provider-ом")]
    Seekability,
    /// Request material требует неподдерживаемую declarative operation.
    #[error("request material не поддерживается provider-ом")]
    RequestMaterial,
}

/// Typed authentication outcome без server/body/credential payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthenticationFailure {
    /// Требуемые credentials отсутствуют.
    #[error("transport authentication credentials отсутствуют")]
    CredentialsMissing,
    /// Server отверг переданные credentials.
    #[error("transport authentication credentials отклонены")]
    CredentialsRejected,
    /// Secret scope не разрешает forwarding к нужному target-у.
    #[error("transport authentication material находится вне разрешённого scope")]
    SecretScopeRejected,
    /// Ephemeral credentials истекли.
    #[error("transport authentication credentials истекли")]
    CredentialsExpired,
}

/// Typed network/response outcome без URL и response payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportFailure {
    /// Network временно/постоянно недоступна.
    #[error("transport network недоступна")]
    NetworkUnavailable,
    /// Operation превысила bounded deadline.
    #[error("transport operation превысила deadline")]
    Timeout,
    /// Redirect chain отклонена validated policy.
    #[error("transport redirect отклонён policy")]
    RedirectRejected,
    /// HTTP response нарушает provider contract.
    #[error("transport получил некорректный response")]
    InvalidResponse,
    /// Source оборвался после частичного progress.
    #[error("transport source был прерван")]
    Interrupted,
}

/// Typed refresh-specific failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RefreshFailure {
    /// Provider descriptor не обещает refresh.
    #[error("transport provider не поддерживает refresh")]
    ProviderDoesNotSupportRefresh,
    /// Provider больше не может сопоставить semantic identity.
    #[error("transport refresh не нашёл semantic component")]
    SemanticIdentityUnavailable,
    /// Refresh material уже протух до открытия.
    #[error("transport refresh material уже истёк")]
    ReplacementExpired,
    /// Provider отклонил exact refresh contract.
    #[error("transport provider отклонил refresh contract")]
    ProviderRejected,
}

/// Provider-owned успешный output до registry wrapping-а.
pub struct ProviderOpenOutput {
    /// Final checked target после redirect chain.
    final_target: HttpRequestTarget,
    /// Фактическое число redirect hops.
    redirect_hops: RedirectHopCount,
    /// Provider-confirmed timeline nature.
    presentation: MediaPresentation,
    /// Neutral byte input.
    input: TransportInput,
}

impl ProviderOpenOutput {
    /// Создаёт provider output без возможности подменить caller identity/generation.
    #[must_use]
    pub fn new(
        final_target: HttpRequestTarget,
        redirect_hops: RedirectHopCount,
        presentation: MediaPresentation,
        input: TransportInput,
    ) -> Self {
        Self {
            final_target,
            redirect_hops,
            presentation,
            input,
        }
    }

    /// Разбирает output внутри registry validation boundary.
    pub(crate) fn into_parts(
        self,
    ) -> (
        HttpRequestTarget,
        RedirectHopCount,
        MediaPresentation,
        TransportInput,
    ) {
        (
            self.final_target,
            self.redirect_hops,
            self.presentation,
            self.input,
        )
    }
}

/// Provider-specific open error без raw implementation error chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderOpenError {
    /// Static/runtime unsupported outcome.
    #[error("transport open unsupported: {0}")]
    Unsupported(UnsupportedTransportReason),
    /// Authentication outcome.
    #[error("transport open authentication failed: {0}")]
    Authentication(AuthenticationFailure),
    /// Network/response outcome.
    #[error("transport open failed: {0}")]
    Transport(TransportFailure),
    /// Cooperative cancellation.
    #[error("transport open cancelled")]
    Cancelled,
}

/// Provider-specific refresh error без raw implementation error chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderRefreshError {
    /// Refresh-specific outcome.
    #[error("transport refresh failed: {0}")]
    Refresh(RefreshFailure),
    /// Replacement request unsupported.
    #[error("transport refresh unsupported: {0}")]
    Unsupported(UnsupportedTransportReason),
    /// Authentication outcome.
    #[error("transport refresh authentication failed: {0}")]
    Authentication(AuthenticationFailure),
    /// Network/response outcome.
    #[error("transport refresh transport failed: {0}")]
    Transport(TransportFailure),
    /// Cooperative cancellation.
    #[error("transport refresh cancelled")]
    Cancelled,
}

/// Concrete transport adapter, регистрируемый composition layer-ом.
pub trait TransportProvider: Send + Sync {
    /// Возвращает immutable registration descriptor.
    fn descriptor(&self) -> &ProviderDescriptor;

    /// Открывает один exact component request.
    fn open(&self, request: &TransportOpenRequest)
    -> Result<ProviderOpenOutput, ProviderOpenError>;

    /// Обновляет один exact component request, сохраняя semantic identity.
    fn refresh(
        &self,
        request: &TransportRefreshRequest,
    ) -> Result<ProviderOpenOutput, ProviderRefreshError>;
}
