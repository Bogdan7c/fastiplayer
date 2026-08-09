//! Concrete progressive HTTP provider для neutral web transport API.
//!
//! Crate владеет только HTTP component open/refresh policy. Container probing,
//! service descriptors, player lifecycle и UI остаются за внешними owners.

#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use media_prefetch::{PrefetchConfig, PrefetchingByteSource};
use source_core::{
    HttpHeader, HttpRequestBody, HttpRequestTarget, HttpScheme, HttpSingleHopRequest,
    HttpSourceHop, HttpSourceSession, ScopedHttpCookieJar, ScopedHttpCookieJarError, SourceError,
    SourceRuntimeConfig,
};
use web_media_transport_api::{
    AuthenticationFailure, HttpRangeRequestLimit, ProviderDescriptor, ProviderDescriptorError,
    ProviderOpenError, ProviderOpenOutput, ProviderRefreshError, RefreshSupport,
    SecretRequestContext, SecretRequestPurpose, TransportFailure, TransportInput,
    TransportOpenRequest, TransportProvider, TransportProviderId, TransportProviderIdError,
    TransportRefreshRequest, TransportScheme, UnsupportedTransportReason,
};

use crate::range_redirect::{RedirectChainState, ScopedRangeRedirectHandler};

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
            vec![
                TransportScheme::Http(HttpScheme::Http),
                TransportScheme::Http(HttpScheme::Https),
            ],
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
        let cookie_jar = scoped_cookie_jar_for_request(request)?;
        let session = HttpSourceSession::new_with_cookie_jar(&self.source_config, cookie_jar)
            .map_err(|source| map_source_open_error(&source, SecretDelivery::NotSent))?;
        let mut current_target = http_request_target(request)?.clone();
        let mut redirect_state = RedirectChainState::initial();

        loop {
            if request.cancellation().is_cancelled() {
                return Err(ProviderOpenError::Cancelled);
            }

            let request_material = request_material_for_target(
                request.secrets(),
                &current_target,
                redirect_state.secret_forwarding(),
                redirect_state.request_body_forwarding(),
            )?;
            let secret_delivery = request_material.secret_delivery;
            let hop_request = HttpSingleHopRequest::new(
                current_target.clone(),
                request_material.headers,
                request_material.request_body,
            );

            match session.open_single_hop(hop_request, request.cancellation()) {
                Ok(HttpSourceHop::Redirect(redirect)) => {
                    redirect_state = redirect_state.authorize_next(
                        request.redirects(),
                        request.secrets(),
                        &current_target,
                        &redirect,
                    )?;
                    current_target = redirect.target().clone();
                }
                Ok(HttpSourceHop::Seekable(source)) => {
                    let effective_prefetch_config = prefetch_config_with_source_limit(
                        self.prefetch_config,
                        request.http_range_request_limit(),
                    )?;
                    let source = source.with_range_redirect_handler(Box::new(
                        ScopedRangeRedirectHandler::new(
                            request.redirects(),
                            request.secrets().clone(),
                            redirect_state,
                        ),
                    ));
                    let prefetch_source =
                        PrefetchingByteSource::new(Box::new(source), effective_prefetch_config)
                            .map_err(|_| {
                                ProviderOpenError::Transport(TransportFailure::NetworkUnavailable)
                            })?;
                    let input =
                        TransportInput::seekable(Box::new(prefetch_source)).map_err(|_| {
                            ProviderOpenError::Transport(TransportFailure::InvalidResponse)
                        })?;
                    return Ok(ProviderOpenOutput::new(
                        current_target,
                        redirect_state.completed_hops(),
                        request.presentation(),
                        input,
                    ));
                }
                Ok(HttpSourceHop::Streaming(source)) => {
                    return Ok(ProviderOpenOutput::new(
                        current_target,
                        redirect_state.completed_hops(),
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

/// Совмещает global prefetch policy с более строгим source-specific Range limit.
///
/// Window остаётся global memory policy, а оба фактических read chunk-а
/// ограничиваются extractor-provided верхней границей.
fn prefetch_config_with_source_limit(
    default_config: PrefetchConfig,
    source_limit: Option<HttpRangeRequestLimit>,
) -> Result<PrefetchConfig, ProviderOpenError> {
    let Some(source_limit) = source_limit else {
        return Ok(default_config);
    };
    let maximum_bytes = source_limit.maximum_bytes();
    PrefetchConfig::new(
        default_config.initial_chunk_bytes().min(maximum_bytes),
        default_config.chunk_bytes().min(maximum_bytes),
        default_config.window_bytes(),
    )
    .map_err(|_| ProviderOpenError::Transport(TransportFailure::InvalidResponse))
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

/// Возвращает HTTP-only target либо typed scheme rejection для non-HTTP request-ов.
fn http_request_target(
    request: &TransportOpenRequest,
) -> Result<&HttpRequestTarget, ProviderOpenError> {
    request
        .target()
        .as_http()
        .ok_or(ProviderOpenError::Unsupported(
            UnsupportedTransportReason::Scheme,
        ))
}

/// Создаёт отдельный jar каждого component open/refresh generation-а.
fn scoped_cookie_jar_for_request(
    request: &TransportOpenRequest,
) -> Result<Arc<ScopedHttpCookieJar>, ProviderOpenError> {
    let http_target = http_request_target(request)?;
    let initial_material = request
        .secrets()
        .material_for(http_target, SecretRequestPurpose::PrimaryResource)
        .ok_or(ProviderOpenError::Authentication(
            AuthenticationFailure::SecretScopeRejected,
        ))?;
    let cookie_jar = ScopedHttpCookieJar::new(
        request.secrets().scope().request_scope_proof(),
        http_target,
        initial_material.cookies_for_request(),
        initial_material.cookie_seeds_for_request(),
    )
    .map_err(|error| match error {
        ScopedHttpCookieJarError::InitialTargetOutsideScope => {
            ProviderOpenError::Authentication(AuthenticationFailure::SecretScopeRejected)
        }
        ScopedHttpCookieJarError::InvalidSerializedCookies => {
            ProviderOpenError::Unsupported(UnsupportedTransportReason::RequestMaterial)
        }
    })?;
    Ok(Arc::new(cookie_jar))
}

/// Извлекает секреты только через intent-named S21T scope boundary.
fn request_material_for_target(
    secrets: &SecretRequestContext,
    target: &HttpRequestTarget,
    secret_forwarding: SecretForwarding,
    request_body_forwarding: RequestBodyForwarding,
) -> Result<RequestMaterial, ProviderOpenError> {
    if secrets.is_empty() {
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

    let material = secrets
        .material_for(target, SecretRequestPurpose::PrimaryResource)
        .ok_or(ProviderOpenError::Authentication(
            AuthenticationFailure::SecretScopeRejected,
        ))?;
    let headers = material.headers_for_request().to_vec();
    if headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case("cookie"))
    {
        return Err(ProviderOpenError::Unsupported(
            UnsupportedTransportReason::RequestMaterial,
        ));
    }
    let request_body = match (request_body_forwarding, material.request_data_for_request()) {
        (RequestBodyForwarding::Preserve, Some(bytes)) => HttpRequestBody::Bytes(bytes.to_vec()),
        (RequestBodyForwarding::Preserve | RequestBodyForwarding::Drop, None)
        | (RequestBodyForwarding::Drop, Some(_)) => HttpRequestBody::Absent,
    };
    let has_scoped_cookies =
        material.cookies_for_request().is_some() || !material.cookie_seeds_for_request().is_empty();
    let secret_delivery = if headers.is_empty() && !request_body.is_present() && !has_scoped_cookies
    {
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
        | SourceError::HttpBodyTooLarge { .. }
        | SourceError::InvalidContentRange { .. }
        | SourceError::HttpRangeUnsupported { .. }
        | SourceError::HttpRequestPolicyRejected { .. }
        | SourceError::HttpRepresentationChanged { .. }
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
        SourceError::HttpRangeRedirectRejected { .. } => {
            ProviderOpenError::Transport(TransportFailure::RedirectRejected)
        }
        SourceError::FtpTransport { .. } => {
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

mod range_redirect;

#[cfg(test)]
mod range_redirect_tests;

#[cfg(test)]
mod tests;
