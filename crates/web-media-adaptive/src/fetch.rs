//! Shared S21T-authorized HTTP execution для manifest и segment owner-ов.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;

use source_core::{
    CancellationToken, HttpBoundedByteRange, HttpBoundedFetchHop, HttpBoundedFetchKind,
    HttpBoundedFetchRequest, HttpHeader, HttpRequestTarget, HttpSourceSession, ScopedHttpCookieJar,
    ScopedHttpCookieJarError, SourceError, SourceRuntimeConfig,
};
use web_media_transport_api::{
    MediaPresentation, RedirectHopCount, RedirectPolicyError, SecretRequestContext,
    SecretRequestPurpose, SourceGeneration, TransportOpenRequest,
};

use crate::{AdaptiveRetryPolicy, AdaptiveTransportLimits};

/// Immutable shared request policy одного adaptive component generation lineage.
#[derive(Clone)]
pub struct AdaptiveHttpContext {
    pub(crate) session: HttpSourceSession,
    pub(crate) secrets: SecretRequestContext,
    pub(crate) redirects: web_media_transport_api::RedirectPolicy,
    pub(crate) cancellation: CancellationToken,
    pub(crate) initial_generation: SourceGeneration,
    pub(crate) expected_presentation: MediaPresentation,
    pub(crate) limits: AdaptiveTransportLimits,
    pub(crate) retry: AdaptiveRetryPolicy,
}

impl AdaptiveHttpContext {
    /// Собирает единственную source-core session и scoped ephemeral cookie jar.
    pub fn new(
        request: TransportOpenRequest,
        source_config: &SourceRuntimeConfig,
        limits: AdaptiveTransportLimits,
        retry: AdaptiveRetryPolicy,
    ) -> Result<Self, AdaptiveTransportError> {
        let initial_material = request
            .secrets()
            .material_for(request.target(), SecretRequestPurpose::Manifest)
            .ok_or(AdaptiveTransportError::SecretScopeRejected)?;
        let cookie_jar = ScopedHttpCookieJar::new(
            request.secrets().scope().request_scope_proof(),
            request.target(),
            initial_material.cookies_for_request(),
        )
        .map_err(map_cookie_jar_error)?;
        let session = HttpSourceSession::new_with_cookie_jar(source_config, Arc::new(cookie_jar))?;
        Ok(Self {
            session,
            secrets: request.secrets().clone(),
            redirects: request.redirects(),
            cancellation: request.cancellation().clone(),
            initial_generation: request.source_generation(),
            expected_presentation: request.presentation(),
            limits,
            retry,
        })
    }

    /// Возвращает shared cooperative cancellation owner-а.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl fmt::Debug for AdaptiveHttpContext {
    /// Не форматирует target, headers, cookies или query override.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveHttpContext")
            .field("session", &self.session)
            .field("secrets", &self.secrets)
            .field("redirects", &self.redirects)
            .field("initial_generation", &self.initial_generation)
            .field("limits", &self.limits)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

/// Secret-safe typed adaptive transport failure.
#[derive(Debug, thiserror::Error)]
pub enum AdaptiveTransportError {
    /// Shared caller отменил lifecycle.
    #[error("adaptive transport cancelled")]
    Cancelled,
    /// Low-level source-core request/body/range failure.
    #[error("adaptive HTTP resource failure: {0}")]
    Source(#[from] SourceError),
    /// Relative/query target нельзя безопасно выразить как HTTP(S) locator.
    #[error("adaptive HTTP target resolution failed: {0}")]
    Target(#[from] source_core::HttpRequestTargetError),
    /// S21T redirect policy отклонила hop.
    #[error("adaptive redirect rejected: {0}")]
    Redirect(#[from] RedirectPolicyError),
    /// Transient secret material нельзя передать этому target/purpose.
    #[error("adaptive secret scope rejected request target")]
    SecretScopeRejected,
    /// Serialized Cookie обязан жить в scoped jar, а не в generic header list.
    #[error("adaptive request headers contain forbidden explicit Cookie")]
    ExplicitCookieHeader,
    /// Bounded worker lifecycle неожиданно завершился.
    #[error("adaptive HTTP worker stopped")]
    WorkerStopped,
    /// Async result/request принадлежит уже superseded source generation.
    #[error("stale adaptive source generation: current {current:?}, received {received:?}")]
    StaleGeneration {
        /// Active owner generation.
        current: SourceGeneration,
        /// Rejected generation.
        received: SourceGeneration,
    },
}

impl AdaptiveTransportError {
    /// Отделяет transient network failure от permanent policy/shape failure.
    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Self::Source(SourceError::HttpTimeout { .. })
            | Self::Source(SourceError::HttpRequest { .. })
            | Self::Source(SourceError::HttpBodyRead { .. })
            | Self::Source(SourceError::UnexpectedEof { .. }) => true,
            Self::Source(SourceError::HttpStatus { status, .. }) => {
                status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429
            }
            Self::Cancelled
            | Self::Source(_)
            | Self::Target(_)
            | Self::Redirect(_)
            | Self::SecretScopeRejected
            | Self::ExplicitCookieHeader
            | Self::WorkerStopped
            | Self::StaleGeneration { .. } => false,
        }
    }
}

fn map_cookie_jar_error(error: ScopedHttpCookieJarError) -> AdaptiveTransportError {
    match error {
        ScopedHttpCookieJarError::InitialTargetOutsideScope => {
            AdaptiveTransportError::SecretScopeRejected
        }
        ScopedHttpCookieJarError::InvalidSerializedCookies => {
            AdaptiveTransportError::ExplicitCookieHeader
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchPurpose {
    Manifest,
    MediaSegment,
}

impl FetchPurpose {
    const fn secret_purpose(self) -> SecretRequestPurpose {
        match self {
            Self::Manifest => SecretRequestPurpose::Manifest,
            Self::MediaSegment => SecretRequestPurpose::MediaSegment,
        }
    }

    const fn fetch_kind(self) -> HttpBoundedFetchKind {
        match self {
            Self::Manifest => HttpBoundedFetchKind::Metadata,
            Self::MediaSegment => HttpBoundedFetchKind::Media,
        }
    }
}

#[derive(Debug)]
pub(crate) struct FetchJob {
    pub id: u64,
    pub generation: SourceGeneration,
    pub target: HttpRequestTarget,
    pub byte_range: Option<HttpBoundedByteRange>,
    pub maximum_body_bytes: std::num::NonZeroUsize,
    pub purpose: FetchPurpose,
}

#[derive(Debug)]
pub(crate) struct FetchSuccess {
    pub final_target: HttpRequestTarget,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct FetchOutcome {
    pub id: u64,
    pub generation: SourceGeneration,
    pub result: Result<FetchSuccess, AdaptiveTransportError>,
}

pub(crate) struct FetchExecutor {
    command_sender: Option<SyncSender<FetchJob>>,
    outcome_receiver: Receiver<FetchOutcome>,
}

impl FetchExecutor {
    pub fn start(context: AdaptiveHttpContext) -> Result<Self, AdaptiveTransportError> {
        let (command_sender, command_receiver) = mpsc::sync_channel(1);
        let (outcome_sender, outcome_receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("adaptive-http-fetch".to_owned())
            .spawn(move || run_fetch_worker(context, command_receiver, outcome_sender))
            .map_err(|_| AdaptiveTransportError::WorkerStopped)?;
        Ok(Self {
            command_sender: Some(command_sender),
            outcome_receiver,
        })
    }

    pub fn try_submit(&self, job: FetchJob) -> Result<bool, AdaptiveTransportError> {
        let Some(sender) = &self.command_sender else {
            return Err(AdaptiveTransportError::WorkerStopped);
        };
        match sender.try_send(job) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => Err(AdaptiveTransportError::WorkerStopped),
        }
    }

    pub fn try_receive(&self) -> Result<Option<FetchOutcome>, AdaptiveTransportError> {
        match self.outcome_receiver.try_recv() {
            Ok(outcome) => Ok(Some(outcome)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(AdaptiveTransportError::WorkerStopped),
        }
    }
}

impl Drop for FetchExecutor {
    fn drop(&mut self) {
        self.command_sender.take();
    }
}

fn run_fetch_worker(
    context: AdaptiveHttpContext,
    command_receiver: Receiver<FetchJob>,
    outcome_sender: SyncSender<FetchOutcome>,
) {
    while let Ok(job) = command_receiver.recv() {
        let id = job.id;
        let generation = job.generation;
        let result = fetch_with_redirects(&context, job);
        if outcome_sender
            .send(FetchOutcome {
                id,
                generation,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

fn fetch_with_redirects(
    context: &AdaptiveHttpContext,
    job: FetchJob,
) -> Result<FetchSuccess, AdaptiveTransportError> {
    if context.cancellation.is_cancelled() {
        return Err(AdaptiveTransportError::Cancelled);
    }
    let mut target = job.target;
    let mut completed_hops = RedirectHopCount::none();
    let mut forward_secrets = true;

    loop {
        let (request_target, headers) =
            request_material(&context.secrets, &target, job.purpose, forward_secrets)?;
        let request = match job.byte_range {
            Some(byte_range) => HttpBoundedFetchRequest::range(
                request_target,
                headers,
                byte_range,
                job.purpose.fetch_kind(),
            ),
            None => HttpBoundedFetchRequest::full(
                request_target,
                headers,
                job.maximum_body_bytes,
                job.purpose.fetch_kind(),
            ),
        };
        match context
            .session
            .fetch_bounded_single_hop(request, &context.cancellation)?
        {
            HttpBoundedFetchHop::Complete(response) => {
                return Ok(FetchSuccess {
                    final_target: target,
                    bytes: response.into_bytes(),
                });
            }
            HttpBoundedFetchHop::Redirect(redirect) => {
                let authorization = context.redirects.authorize_redirect(
                    &target,
                    redirect.target(),
                    completed_hops,
                )?;
                forward_secrets &= authorization.permits_secret_scope_check();
                target = redirect.target().clone();
                completed_hops = RedirectHopCount::new(completed_hops.value().saturating_add(1));
            }
        }
    }
}

fn request_material(
    secrets: &SecretRequestContext,
    target: &HttpRequestTarget,
    purpose: FetchPurpose,
    forward_secrets: bool,
) -> Result<(HttpRequestTarget, Vec<HttpHeader>), AdaptiveTransportError> {
    if !forward_secrets || secrets.is_empty() {
        return Ok((target.clone(), Vec::new()));
    }
    let material = secrets
        .material_for(target, purpose.secret_purpose())
        .ok_or(AdaptiveTransportError::SecretScopeRejected)?;
    let headers = material.headers_for_request().to_vec();
    if headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case("cookie"))
    {
        return Err(AdaptiveTransportError::ExplicitCookieHeader);
    }
    let request_target = match material.query_override_for_request() {
        Some(query) => target.with_query_override(query.expose_secret_for_request())?,
        None => target.clone(),
    };
    Ok((request_target, headers))
}
