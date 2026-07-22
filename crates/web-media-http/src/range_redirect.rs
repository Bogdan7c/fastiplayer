//! Transport-owned policy для redirect-ов последующих HTTP Range request-ов.
//!
//! `source-core` владеет физическими request-ами и method/body mechanics, а этот
//! модуль повторно применяет bounded redirect policy и secret scope каждого
//! нового hop-а. Reqwest automatic redirects намеренно не используются.

use source_core::{
    HttpRangeRedirectBodyForwarding, HttpRangeRedirectHandler, HttpRangeRedirectHopCount,
    HttpRangeRedirectRejection, HttpRangeRedirectRequestMaterial, HttpRedirectHop,
    HttpRedirectRequestBehavior, HttpRequestTarget,
};
use web_media_transport_api::{
    AuthenticationFailure, ProviderOpenError, RedirectHopCount, RedirectPolicy,
    SecretRequestContext, TransportFailure, UnsupportedTransportReason,
};

use super::{RequestBodyForwarding, SecretForwarding, request_material_for_target};

/// Полное policy state одной redirect chain до следующего hop-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RedirectChainState {
    /// Число уже завершённых hop-ов initial open-а.
    completed_hops: RedirectHopCount,
    /// Sticky запрет повторной доставки secret material.
    secret_forwarding: SecretForwarding,
    /// Sticky переход на GET без исходного body.
    request_body_forwarding: RequestBodyForwarding,
}

impl RedirectChainState {
    /// Начинает initial component open с полного scoped material.
    pub(super) const fn initial() -> Self {
        Self {
            completed_hops: RedirectHopCount::none(),
            secret_forwarding: SecretForwarding::Scoped,
            request_body_forwarding: RequestBodyForwarding::Preserve,
        }
    }

    /// Возвращает фактическое число initial open redirect-ов для output identity.
    pub(super) const fn completed_hops(self) -> RedirectHopCount {
        self.completed_hops
    }

    /// Возвращает sticky secret state для material текущего hop-а.
    pub(super) const fn secret_forwarding(self) -> SecretForwarding {
        self.secret_forwarding
    }

    /// Возвращает sticky body state для material текущего hop-а.
    pub(super) const fn request_body_forwarding(self) -> RequestBodyForwarding {
        self.request_body_forwarding
    }

    /// Авторизует следующий hop и монотонно ужесточает forwarding state.
    pub(super) fn authorize_next(
        self,
        redirect_policy: RedirectPolicy,
        secrets: &SecretRequestContext,
        current_target: &HttpRequestTarget,
        redirect: &HttpRedirectHop,
    ) -> Result<Self, ProviderOpenError> {
        let authorization = redirect_policy
            .authorize_redirect(current_target, redirect.target(), self.completed_hops)
            .map_err(|_| ProviderOpenError::Transport(TransportFailure::RedirectRejected))?;
        let secret_forwarding = if self.secret_forwarding == SecretForwarding::Stripped
            || !authorization.permits_secret_scope_check()
        {
            SecretForwarding::Stripped
        } else {
            SecretForwarding::Scoped
        };
        if secret_forwarding == SecretForwarding::Scoped
            && !secrets.is_empty()
            && secrets
                .material_for(
                    redirect.target(),
                    web_media_transport_api::SecretRequestPurpose::PrimaryResource,
                )
                .is_none()
        {
            return Err(ProviderOpenError::Authentication(
                AuthenticationFailure::SecretScopeRejected,
            ));
        }
        let request_body_forwarding = if self.request_body_forwarding == RequestBodyForwarding::Drop
            || redirect.request_behavior() == HttpRedirectRequestBehavior::SwitchToGetWithoutBody
        {
            RequestBodyForwarding::Drop
        } else {
            RequestBodyForwarding::Preserve
        };
        let next_completed_hops =
            RedirectHopCount::new(self.completed_hops.value().checked_add(1).ok_or(
                ProviderOpenError::Transport(TransportFailure::RedirectRejected),
            )?);

        Ok(Self {
            completed_hops: next_completed_hops,
            secret_forwarding,
            request_body_forwarding,
        })
    }
}

/// Least-authority handler поздних redirect-ов seekable Range source-а.
pub(super) struct ScopedRangeRedirectHandler {
    /// Immutable bounded redirect policy component request-а.
    redirect_policy: RedirectPolicy,
    /// Ephemeral scoped secrets без остальных полей transport request-а.
    secrets: SecretRequestContext,
    /// Forwarding state стабильного base request-а после initial redirects.
    base_state: RedirectChainState,
    /// Mutable sticky state только текущего логического Range read-а.
    active_state: RedirectChainState,
}

impl ScopedRangeRedirectHandler {
    /// Создаёт handler без network side effects и без владения source mechanics.
    pub(super) fn new(
        redirect_policy: RedirectPolicy,
        secrets: SecretRequestContext,
        initial_open_state: RedirectChainState,
    ) -> Self {
        let base_state = RedirectChainState {
            completed_hops: RedirectHopCount::none(),
            secret_forwarding: initial_open_state.secret_forwarding,
            request_body_forwarding: initial_open_state.request_body_forwarding,
        };
        Self {
            redirect_policy,
            secrets,
            base_state,
            active_state: base_state,
        }
    }
}

impl HttpRangeRedirectHandler for ScopedRangeRedirectHandler {
    /// Сбрасывает sticky state перед каждым read и перед каждым retry.
    fn begin_range_request(&mut self) {
        self.active_state = self.base_state;
    }

    /// Возвращает только scope-filtered headers и least-authority body policy.
    fn material_for_redirect(
        &mut self,
        current_target: &HttpRequestTarget,
        redirect: &HttpRedirectHop,
        completed_hops: HttpRangeRedirectHopCount,
    ) -> Result<HttpRangeRedirectRequestMaterial, HttpRangeRedirectRejection> {
        if self.active_state.completed_hops.value() != completed_hops.value() {
            return Err(HttpRangeRedirectRejection::PolicyRejected);
        }
        let next_state = self
            .active_state
            .authorize_next(
                self.redirect_policy,
                &self.secrets,
                current_target,
                redirect,
            )
            .map_err(map_range_redirect_error)?;
        let request_material = request_material_for_target(
            &self.secrets,
            redirect.target(),
            next_state.secret_forwarding,
            next_state.request_body_forwarding,
        )
        .map_err(map_range_redirect_error)?;
        let body_forwarding = if next_state.request_body_forwarding
            == RequestBodyForwarding::Preserve
            && request_material.request_body.is_present()
        {
            HttpRangeRedirectBodyForwarding::PreserveCurrent
        } else {
            HttpRangeRedirectBodyForwarding::Drop
        };
        self.active_state = next_state;

        Ok(HttpRangeRedirectRequestMaterial::new(
            request_material.headers,
            body_forwarding,
        ))
    }
}

/// Схлопывает transport taxonomy в safe source boundary без operational data.
fn map_range_redirect_error(error: ProviderOpenError) -> HttpRangeRedirectRejection {
    match error {
        ProviderOpenError::Transport(TransportFailure::RedirectRejected) => {
            HttpRangeRedirectRejection::PolicyRejected
        }
        ProviderOpenError::Authentication(AuthenticationFailure::SecretScopeRejected) => {
            HttpRangeRedirectRejection::SecretScopeRejected
        }
        ProviderOpenError::Unsupported(UnsupportedTransportReason::RequestMaterial) => {
            HttpRangeRedirectRejection::RequestMaterialRejected
        }
        ProviderOpenError::Unsupported(_)
        | ProviderOpenError::Authentication(_)
        | ProviderOpenError::Transport(_)
        | ProviderOpenError::Cancelled => HttpRangeRedirectRejection::RequestMaterialRejected,
    }
}
