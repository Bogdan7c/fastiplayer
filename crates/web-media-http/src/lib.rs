//! Concrete progressive HTTP provider для neutral web transport API.
//!
//! Crate владеет только HTTP component open/refresh policy. Container probing,
//! service descriptors, player lifecycle и UI остаются за внешними owners.

#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;

use media_prefetch::{PrefetchConfig, PrefetchingByteSource};
use source_core::{
    HttpHeader, HttpRedirectRequestBehavior, HttpRequestBody, HttpScheme, HttpSingleHopRequest,
    HttpSourceHop, HttpSourceSession, SourceError, SourceRuntimeConfig,
};
use web_media_transport_api::{
    AuthenticationFailure, ProviderDescriptor, ProviderDescriptorError, ProviderOpenError,
    ProviderOpenOutput, ProviderRefreshError, RedirectHopCount, RefreshSupport,
    SecretRequestPurpose, TransportFailure, TransportInput, TransportOpenRequest,
    TransportProvider, TransportProviderId, TransportProviderIdError, TransportRefreshRequest,
    UnsupportedTransportReason,
};

/// Stable registry identity concrete progressive HTTP provider-а.
pub const WEB_MEDIA_HTTP_PROVIDER_ID: &str = "progressive-http";

/// Может ли текущий redirect hop повторно запросить scoped secret material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretForwarding {
    /// Target остаётся в policy-разрешённой области для scope-проверки.
    Scoped,
    /// Cross-origin redirect продолжает open только без любых secret values.
    Stripped,
}

/// Должен ли следующий redirect hop сохранить исходный request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestBodyForwarding {
    /// `307`/`308` и исходный hop сохраняют body.
    Preserve,
    /// `301`/`302`/`303` переводят последующие hops на `GET` без body.
    Drop,
}

/// Фактическая доставка credentials на конкретном HTTP hop-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretDelivery {
    /// Request не имел credentials, поэтому auth failure означает missing credentials.
    NotSent,
    /// Scoped credentials были отправлены и отвергнуты origin-ом.
    Sent,
    /// Credentials существовали, но redirect policy запретила их пересылку.
    Stripped,
}

/// Ошибка построения immutable provider descriptor-а до network side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebMediaHttpProviderBuildError {
    /// Compile-time provider ID перестал удовлетворять neutral grammar.
    InvalidProviderId(TransportProviderIdError),
    /// Static capability descriptor нарушает transport registry contract.
    InvalidDescriptor(ProviderDescriptorError),
}

impl fmt::Display for WebMediaHttpProviderBuildError {
    /// Форматирует только static provider schema, не request material.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProviderId(source) => {
                write!(formatter, "invalid HTTP provider ID: {source}")
            }
            Self::InvalidDescriptor(source) => {
                write!(formatter, "invalid HTTP provider descriptor: {source}")
            }
        }
    }
}

impl Error for WebMediaHttpProviderBuildError {}

/// Small concrete provider, переиспользующий source-core и media-prefetch owners.
pub struct WebMediaHttpProvider {
    /// Immutable neutral registration descriptor.
    descriptor: ProviderDescriptor,
    /// Validated connection/read/cache runtime policy.
    source_config: SourceRuntimeConfig,
    /// Existing seekable VOD prefetch policy.
    prefetch_config: PrefetchConfig,
}

impl WebMediaHttpProvider {
    /// Создаёт provider без HTTP client-а и без network side effects.
    pub fn new(
        source_config: SourceRuntimeConfig,
        prefetch_config: PrefetchConfig,
    ) -> Result<Self, WebMediaHttpProviderBuildError> {
        let provider_id = TransportProviderId::new(WEB_MEDIA_HTTP_PROVIDER_ID)
            .map_err(WebMediaHttpProviderBuildError::InvalidProviderId)?;
        let descriptor = ProviderDescriptor::new(
            provider_id,
            vec![HttpScheme::Http, HttpScheme::Https],
            RefreshSupport::Supported,
        )
        .map_err(WebMediaHttpProviderBuildError::InvalidDescriptor)?;
        Ok(Self {
            descriptor,
            source_config,
            prefetch_config,
        })
    }

    /// Выполняет open/refresh через один source-core HTTP session.
    fn open_component(
        &self,
        request: &TransportOpenRequest,
    ) -> Result<ProviderOpenOutput, ProviderOpenError> {
        let session = HttpSourceSession::new(&self.source_config)
            .map_err(|source| map_source_open_error(&source, SecretDelivery::NotSent))?;
        let mut current_target = request.target().clone();
        let mut completed_hops = RedirectHopCount::none();
        let mut secret_forwarding = SecretForwarding::Scoped;
        let mut request_body_forwarding = RequestBodyForwarding::Preserve;

        loop {
            if request.cancellation().is_cancelled() {
                return Err(ProviderOpenError::Cancelled);
            }

            let request_material = request_material_for_target(
                request,
                &current_target,
                secret_forwarding,
                request_body_forwarding,
            )?;
            let secret_delivery = request_material.secret_delivery;
            let hop_request = HttpSingleHopRequest::new(
                current_target.clone(),
                request_material.headers,
                request_material.request_body,
            );

            match session.open_single_hop(hop_request, request.cancellation()) {
                Ok(HttpSourceHop::Redirect(redirect)) => {
                    let authorization = request
                        .redirects()
                        .authorize_redirect(&current_target, redirect.target(), completed_hops)
                        .map_err(|_| {
                            ProviderOpenError::Transport(TransportFailure::RedirectRejected)
                        })?;
                    secret_forwarding = if authorization.permits_secret_scope_check() {
                        SecretForwarding::Scoped
                    } else {
                        SecretForwarding::Stripped
                    };
                    if secret_forwarding == SecretForwarding::Scoped
                        && !request.secrets().is_empty()
                        && request
                            .secrets()
                            .material_for(redirect.target(), SecretRequestPurpose::PrimaryResource)
                            .is_none()
                    {
                        return Err(ProviderOpenError::Authentication(
                            AuthenticationFailure::SecretScopeRejected,
                        ));
                    }
                    if redirect.request_behavior()
                        == HttpRedirectRequestBehavior::SwitchToGetWithoutBody
                    {
                        request_body_forwarding = RequestBodyForwarding::Drop;
                    }
                    completed_hops =
                        RedirectHopCount::new(completed_hops.value().checked_add(1).ok_or(
                            ProviderOpenError::Transport(TransportFailure::RedirectRejected),
                        )?);
                    current_target = redirect.target().clone();
                }
                Ok(HttpSourceHop::Seekable(source)) => {
                    let prefetch_source =
                        PrefetchingByteSource::new(Box::new(source), self.prefetch_config)
                            .map_err(|_| {
                                ProviderOpenError::Transport(TransportFailure::NetworkUnavailable)
                            })?;
                    let input =
                        TransportInput::seekable(Box::new(prefetch_source)).map_err(|_| {
                            ProviderOpenError::Transport(TransportFailure::InvalidResponse)
                        })?;
                    return Ok(ProviderOpenOutput::new(
                        current_target,
                        completed_hops,
                        request.presentation(),
                        input,
                    ));
                }
                Ok(HttpSourceHop::Streaming(source)) => {
                    return Ok(ProviderOpenOutput::new(
                        current_target,
                        completed_hops,
                        request.presentation(),
                        TransportInput::streaming(source),
                    ));
                }
                Err(source) => {
                    return Err(map_source_open_error(&source, secret_delivery));
                }
            }
        }
    }
}

impl fmt::Debug for WebMediaHttpProvider {
    /// Не раскрывает runtime request material либо HTTP client internals.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaHttpProvider")
            .field("descriptor", &self.descriptor)
            .field("source_config", &self.source_config)
            .field("prefetch_config", &self.prefetch_config)
            .finish()
    }
}

impl TransportProvider for WebMediaHttpProvider {
    /// Возвращает immutable HTTP/HTTPS capability registration.
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    /// Открывает exact component без service/container knowledge.
    fn open(
        &self,
        request: &TransportOpenRequest,
    ) -> Result<ProviderOpenOutput, ProviderOpenError> {
        self.open_component(request)
    }

    /// Переоткрывает только registry-validated replacement request.
    fn refresh(
        &self,
        request: &TransportRefreshRequest,
    ) -> Result<ProviderOpenOutput, ProviderRefreshError> {
        self.open_component(request.replacement())
            .map_err(map_refresh_error)
    }
}

/// Material одного hop-а после redirect/scope проверки.
struct RequestMaterial {
    /// Headers, которые source-core имеет право отправить.
    headers: Vec<HttpHeader>,
    /// Typed request body для разрешённого primary-resource method-а.
    request_body: HttpRequestBody,
    /// Typed auth classification фактически выполненного hop-а.
    secret_delivery: SecretDelivery,
}

/// Извлекает секреты только через intent-named S21T scope boundary.
fn request_material_for_target(
    request: &TransportOpenRequest,
    target: &source_core::HttpRequestTarget,
    secret_forwarding: SecretForwarding,
    request_body_forwarding: RequestBodyForwarding,
) -> Result<RequestMaterial, ProviderOpenError> {
    if request.secrets().is_empty() {
        return Ok(RequestMaterial {
            headers: Vec::new(),
            request_body: HttpRequestBody::Absent,
            secret_delivery: SecretDelivery::NotSent,
        });
    }
    if secret_forwarding == SecretForwarding::Stripped {
        return Ok(RequestMaterial {
            headers: Vec::new(),
            request_body: HttpRequestBody::Absent,
            secret_delivery: SecretDelivery::Stripped,
        });
    }

    let material = request
        .secrets()
        .material_for(target, SecretRequestPurpose::PrimaryResource)
        .ok_or(ProviderOpenError::Authentication(
            AuthenticationFailure::SecretScopeRejected,
        ))?;
    let mut headers = material.headers_for_request().to_vec();
    if let Some(serialized_cookies) = material.cookies_for_request() {
        if headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("cookie"))
        {
            return Err(ProviderOpenError::Unsupported(
                UnsupportedTransportReason::RequestMaterial,
            ));
        }
        let cookie_value = std::str::from_utf8(serialized_cookies).map_err(|_| {
            ProviderOpenError::Unsupported(UnsupportedTransportReason::RequestMaterial)
        })?;
        headers.push(HttpHeader::new("cookie", cookie_value));
    }
    let request_body = match (request_body_forwarding, material.request_data_for_request()) {
        (RequestBodyForwarding::Preserve, Some(bytes)) => HttpRequestBody::Bytes(bytes.to_vec()),
        (RequestBodyForwarding::Preserve | RequestBodyForwarding::Drop, None)
        | (RequestBodyForwarding::Drop, Some(_)) => HttpRequestBody::Absent,
    };
    let secret_delivery = if headers.is_empty() && !request_body.is_present() {
        SecretDelivery::NotSent
    } else {
        SecretDelivery::Sent
    };

    Ok(RequestMaterial {
        headers,
        request_body,
        secret_delivery,
    })
}

/// Схлопывает source implementation details только в разрешённую S21T taxonomy.
fn map_source_open_error(
    source: &SourceError,
    secret_delivery: SecretDelivery,
) -> ProviderOpenError {
    match source {
        SourceError::Cancelled => ProviderOpenError::Cancelled,
        SourceError::HttpTimeout { .. } => ProviderOpenError::Transport(TransportFailure::Timeout),
        SourceError::HttpStatus { status, .. } if matches!(status.as_u16(), 401 | 403 | 407) => {
            let failure = match secret_delivery {
                SecretDelivery::Stripped => AuthenticationFailure::SecretScopeRejected,
                SecretDelivery::Sent => AuthenticationFailure::CredentialsRejected,
                SecretDelivery::NotSent => AuthenticationFailure::CredentialsMissing,
            };
            ProviderOpenError::Authentication(failure)
        }
        SourceError::InvalidHttpRedirect { .. }
        | SourceError::InvalidContentRange { .. }
        | SourceError::HttpRangeUnsupported { .. }
        | SourceError::NotSeekable { .. }
        | SourceError::InvalidHttpHeaderName { .. }
        | SourceError::InvalidHttpHeaderValue { .. } => {
            ProviderOpenError::Transport(TransportFailure::InvalidResponse)
        }
        SourceError::UnexpectedEof { .. } | SourceError::HttpBodyRead { .. } => {
            ProviderOpenError::Transport(TransportFailure::Interrupted)
        }
        SourceError::InvalidConfig { .. }
        | SourceError::LocalIo { .. }
        | SourceError::HttpClientBuild { .. }
        | SourceError::HttpRequest { .. }
        | SourceError::HttpStatus { .. } => {
            ProviderOpenError::Transport(TransportFailure::NetworkUnavailable)
        }
    }
}

/// Переводит open taxonomy в refresh taxonomy без потери категории.
fn map_refresh_error(source: ProviderOpenError) -> ProviderRefreshError {
    match source {
        ProviderOpenError::Unsupported(reason) => ProviderRefreshError::Unsupported(reason),
        ProviderOpenError::Authentication(reason) => ProviderRefreshError::Authentication(reason),
        ProviderOpenError::Transport(reason) => ProviderRefreshError::Transport(reason),
        ProviderOpenError::Cancelled => ProviderRefreshError::Cancelled,
    }
}

#[cfg(test)]
mod tests;
