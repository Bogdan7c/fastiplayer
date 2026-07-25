//! Process-local exact provider registry.

use crate::{
    AuthenticationFailure, OpenedTransport, ProviderOpenError, ProviderRefreshError,
    RedirectHopCount, RedirectPolicyError, RefreshFailure, RefreshSupport, RefreshedTransport,
    SourceGeneration, TransportFailure, TransportOpenRequest, TransportProvider,
    TransportProviderId, TransportRefreshRequest, UnsupportedTransportReason,
};

/// Deterministic registry concrete transport providers.
#[derive(Default)]
pub struct TransportRegistry {
    /// Registration order используется только для stable iteration/debugging.
    providers: Vec<Box<dyn TransportProvider>>,
}

impl TransportRegistry {
    /// Создаёт пустой registry; отсутствие provider-а является typed outcome.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Регистрирует exact provider identity один раз.
    pub fn register(
        &mut self,
        provider: Box<dyn TransportProvider>,
    ) -> Result<(), TransportRegistryError> {
        let provider_id = provider.descriptor().provider_id();
        if self
            .providers
            .iter()
            .any(|registered| registered.descriptor().provider_id() == provider_id)
        {
            return Err(TransportRegistryError::DuplicateProvider {
                provider_id: provider_id.clone(),
            });
        }
        self.providers.push(provider);
        Ok(())
    }

    /// Открывает request только exact selected provider-ом.
    pub fn open(
        &self,
        request: TransportOpenRequest,
    ) -> Result<OpenedTransport, TransportOpenError> {
        if request.cancellation().is_cancelled() {
            return Err(TransportOpenError::Cancelled);
        }
        let provider = self.provider(request.provider()).ok_or_else(|| {
            TransportOpenError::ProviderUnavailable {
                provider_id: request.provider().clone(),
            }
        })?;
        if !provider.descriptor().supports_target(request.target()) {
            return Err(TransportOpenError::Unsupported(
                UnsupportedTransportReason::Scheme,
            ));
        }
        let output = provider.open(&request).map_err(map_open_error)?;
        if request.cancellation().is_cancelled() {
            return Err(TransportOpenError::Cancelled);
        }
        self.wrap_open_output(&request, output)
    }

    /// Refresh-ит request только если caller-provided current generation совпадает с fence.
    pub fn refresh_if_current(
        &self,
        request: TransportRefreshRequest,
        current_generation: SourceGeneration,
    ) -> Result<RefreshedTransport, TransportRefreshError> {
        let requested_generation = request.previous().source_generation();
        if requested_generation != current_generation {
            return Err(TransportRefreshError::StaleSourceGeneration {
                requested: requested_generation,
                current: current_generation,
            });
        }
        if request.cancellation().is_cancelled() {
            return Err(TransportRefreshError::Cancelled);
        }
        let provider_id = request.previous().provider();
        let provider = self.provider(provider_id).ok_or_else(|| {
            TransportRefreshError::ProviderUnavailable {
                provider_id: provider_id.clone(),
            }
        })?;
        if provider.descriptor().refresh_support() != RefreshSupport::Supported {
            return Err(TransportRefreshError::Refresh(
                RefreshFailure::ProviderDoesNotSupportRefresh,
            ));
        }
        if !provider
            .descriptor()
            .supports_target(request.replacement().target())
        {
            return Err(TransportRefreshError::Unsupported(
                UnsupportedTransportReason::Scheme,
            ));
        }
        let output = provider.refresh(&request).map_err(map_refresh_error)?;
        if request.cancellation().is_cancelled() {
            return Err(TransportRefreshError::Cancelled);
        }
        let replaced_generation = request.previous().source_generation();
        let opened = self
            .wrap_open_output(request.replacement(), output)
            .map_err(TransportRefreshError::from_open_validation)?;
        Ok(RefreshedTransport::new(replaced_generation, opened))
    }

    /// Находит provider только по exact canonical ID.
    fn provider(&self, provider_id: &TransportProviderId) -> Option<&dyn TransportProvider> {
        self.providers
            .iter()
            .find(|provider| provider.descriptor().provider_id() == provider_id)
            .map(Box::as_ref)
    }

    /// Проверяет redirect/output shape и добавляет caller-owned identity.
    fn wrap_open_output(
        &self,
        request: &TransportOpenRequest,
        output: crate::ProviderOpenOutput,
    ) -> Result<OpenedTransport, TransportOpenError> {
        let (final_target, redirect_hops, presentation, input) = output.into_parts();
        if redirect_hops == RedirectHopCount::none() {
            if final_target != *request.target() {
                return Err(TransportOpenError::ProviderContract(
                    ProviderContractViolation::FinalTargetChangedWithoutRedirect,
                ));
            }
        } else {
            let Some(initial_http) = request.target().as_http() else {
                return Err(TransportOpenError::ProviderContract(
                    ProviderContractViolation::FinalTargetChangedWithoutRedirect,
                ));
            };
            let Some(final_http) = final_target.as_http() else {
                return Err(TransportOpenError::ProviderContract(
                    ProviderContractViolation::FinalTargetChangedWithoutRedirect,
                ));
            };
            request
                .redirects()
                .authorize_redirect(
                    initial_http,
                    final_http,
                    RedirectHopCount::new(redirect_hops.value() - 1),
                )
                .map_err(TransportOpenError::Redirect)?;
        }
        Ok(OpenedTransport::new(
            request.opened_identity(),
            presentation,
            final_target,
            input,
        ))
    }
}

/// Ошибка registry composition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportRegistryError {
    /// Provider ID уже занят.
    #[error("transport provider `{provider_id}` уже зарегистрирован")]
    DuplicateProvider {
        /// Exact duplicate provider identity.
        provider_id: TransportProviderId,
    },
}

/// Registry-level open outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportOpenError {
    /// Exact selected provider отсутствует в composition snapshot.
    #[error("transport provider `{provider_id}` недоступен")]
    ProviderUnavailable {
        /// Missing provider identity.
        provider_id: TransportProviderId,
    },
    /// Provider не поддерживает request.
    #[error("transport open unsupported: {0}")]
    Unsupported(UnsupportedTransportReason),
    /// Authentication failure.
    #[error("transport authentication failed: {0}")]
    Authentication(AuthenticationFailure),
    /// Network/response failure.
    #[error("transport open failed: {0}")]
    Transport(TransportFailure),
    /// Redirect policy rejection.
    #[error("transport redirect rejected: {0}")]
    Redirect(RedirectPolicyError),
    /// Provider нарушил neutral output contract.
    #[error("transport provider contract violated: {0}")]
    ProviderContract(ProviderContractViolation),
    /// Cooperative cancellation.
    #[error("transport open cancelled")]
    Cancelled,
}

/// Registry-level refresh outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportRefreshError {
    /// Exact selected provider отсутствует.
    #[error("transport provider `{provider_id}` недоступен для refresh")]
    ProviderUnavailable {
        /// Missing provider identity.
        provider_id: TransportProviderId,
    },
    /// Caller пытается применить result к уже заменённой generation.
    #[error("transport refresh source generation устарела")]
    StaleSourceGeneration {
        /// Generation, которую request намеревался заменить.
        requested: SourceGeneration,
        /// Текущая generation owner-а.
        current: SourceGeneration,
    },
    /// Refresh-specific outcome.
    #[error("transport refresh failed: {0}")]
    Refresh(RefreshFailure),
    /// Replacement request unsupported.
    #[error("transport refresh unsupported: {0}")]
    Unsupported(UnsupportedTransportReason),
    /// Authentication failure.
    #[error("transport refresh authentication failed: {0}")]
    Authentication(AuthenticationFailure),
    /// Network/response failure.
    #[error("transport refresh transport failed: {0}")]
    Transport(TransportFailure),
    /// Redirect policy rejection.
    #[error("transport refresh redirect rejected: {0}")]
    Redirect(RedirectPolicyError),
    /// Provider output contract violation.
    #[error("transport refresh provider contract violated: {0}")]
    ProviderContract(ProviderContractViolation),
    /// Cooperative cancellation.
    #[error("transport refresh cancelled")]
    Cancelled,
}

impl TransportRefreshError {
    /// Переводит только post-provider open-output validation outcomes.
    fn from_open_validation(error: TransportOpenError) -> Self {
        match error {
            TransportOpenError::ProviderUnavailable { provider_id } => {
                Self::ProviderUnavailable { provider_id }
            }
            TransportOpenError::Unsupported(reason) => Self::Unsupported(reason),
            TransportOpenError::Authentication(reason) => Self::Authentication(reason),
            TransportOpenError::Transport(reason) => Self::Transport(reason),
            TransportOpenError::Redirect(reason) => Self::Redirect(reason),
            TransportOpenError::ProviderContract(reason) => Self::ProviderContract(reason),
            TransportOpenError::Cancelled => Self::Cancelled,
        }
    }
}

/// Safe provider output invariant violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderContractViolation {
    /// Provider сменил final target, но заявил zero redirects.
    #[error("final target изменён без redirect hop")]
    FinalTargetChangedWithoutRedirect,
}

/// Адаптирует provider open taxonomy без raw error chain.
fn map_open_error(error: ProviderOpenError) -> TransportOpenError {
    match error {
        ProviderOpenError::Unsupported(reason) => TransportOpenError::Unsupported(reason),
        ProviderOpenError::Authentication(reason) => TransportOpenError::Authentication(reason),
        ProviderOpenError::Transport(reason) => TransportOpenError::Transport(reason),
        ProviderOpenError::Cancelled => TransportOpenError::Cancelled,
    }
}

/// Адаптирует provider refresh taxonomy без raw error chain.
fn map_refresh_error(error: ProviderRefreshError) -> TransportRefreshError {
    match error {
        ProviderRefreshError::Refresh(reason) => TransportRefreshError::Refresh(reason),
        ProviderRefreshError::Unsupported(reason) => TransportRefreshError::Unsupported(reason),
        ProviderRefreshError::Authentication(reason) => {
            TransportRefreshError::Authentication(reason)
        }
        ProviderRefreshError::Transport(reason) => TransportRefreshError::Transport(reason),
        ProviderRefreshError::Cancelled => TransportRefreshError::Cancelled,
    }
}
