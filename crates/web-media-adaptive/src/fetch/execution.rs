//! Физическое выполнение adaptive HTTP fetch-ов за API владельца context-а.

use std::num::NonZeroUsize;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use media_core::DemuxSeekCancellationToken;
use source_core::{
    CancellationToken, HttpBoundedByteRange, HttpBoundedFetchHop, HttpBoundedFetchKind,
    HttpBoundedFetchRequest, HttpBoundedStreamingBody, HttpBoundedStreamingFetchHop, HttpHeader,
    HttpRangeResponseMetadata, HttpRequestTarget, HttpResourceDiagnostics, HttpResourcePurpose,
};
use web_media_transport_api::{
    EndpointExpiryResourceKind, RedirectHopCount, SecretRequestContext, SecretRequestPurpose,
    SourceGeneration,
};

use super::{
    AdaptiveHttpContext, AdaptiveResourcePurpose, AdaptiveResourceQueryApplication,
    AdaptiveResourceSecretForwarding, AdaptiveTransportError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchPurpose {
    Manifest,
    ClockSynchronization,
    MediaSegment,
    Initialization,
    EncryptionKey,
}

impl From<AdaptiveResourcePurpose> for FetchPurpose {
    fn from(purpose: AdaptiveResourcePurpose) -> Self {
        match purpose {
            AdaptiveResourcePurpose::Manifest => Self::Manifest,
            AdaptiveResourcePurpose::ClockSynchronization => Self::ClockSynchronization,
            AdaptiveResourcePurpose::MediaSegment => Self::MediaSegment,
            AdaptiveResourcePurpose::Initialization => Self::Initialization,
            AdaptiveResourcePurpose::EncryptionKey => Self::EncryptionKey,
        }
    }
}

impl FetchPurpose {
    /// Переводит internal fetch purpose в общий lifecycle vocabulary.
    pub(super) const fn expiry_resource_kind(self) -> EndpointExpiryResourceKind {
        match self {
            Self::Manifest => EndpointExpiryResourceKind::Manifest,
            Self::ClockSynchronization => EndpointExpiryResourceKind::ClockSynchronization,
            Self::MediaSegment => EndpointExpiryResourceKind::MediaSegment,
            Self::Initialization => EndpointExpiryResourceKind::Initialization,
            Self::EncryptionKey => EndpointExpiryResourceKind::EncryptionKey,
        }
    }

    pub(super) const fn secret_purpose(self) -> SecretRequestPurpose {
        match self {
            Self::Manifest | Self::ClockSynchronization => SecretRequestPurpose::Manifest,
            Self::MediaSegment | Self::Initialization => SecretRequestPurpose::MediaSegment,
            Self::EncryptionKey => SecretRequestPurpose::EncryptionKey,
        }
    }

    pub(super) const fn fetch_kind(self) -> HttpBoundedFetchKind {
        match self {
            Self::Manifest | Self::ClockSynchronization => HttpBoundedFetchKind::Metadata,
            Self::MediaSegment | Self::Initialization => HttpBoundedFetchKind::Media,
            Self::EncryptionKey => HttpBoundedFetchKind::Metadata,
        }
    }

    /// Сохраняет точный adaptive purpose в source-owned secret-free telemetry.
    pub(super) const fn resource_purpose(self) -> HttpResourcePurpose {
        match self {
            Self::Manifest => HttpResourcePurpose::Manifest,
            Self::ClockSynchronization => HttpResourcePurpose::ClockSynchronization,
            Self::MediaSegment => HttpResourcePurpose::MediaSegment,
            Self::Initialization => HttpResourcePurpose::Initialization,
            Self::EncryptionKey => HttpResourcePurpose::EncryptionKey,
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
    pub query_application: AdaptiveResourceQueryApplication,
    pub secret_forwarding: AdaptiveResourceSecretForwarding,
}

#[derive(Debug)]
pub(crate) struct FetchSuccess {
    pub final_target: HttpRequestTarget,
    pub bytes: Vec<u8>,
    pub range_metadata: Option<HttpRangeResponseMetadata>,
}

/// Успешно открытый streaming response после общей redirect policy.
pub(super) struct StreamingFetchSuccess {
    /// Effective target последнего разрешённого hop-а.
    pub(super) final_target: HttpRequestTarget,
    /// Открытый bounded response cursor.
    pub(super) body: HttpBoundedStreamingBody,
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
    /// Создаёт bounded pool поверх одного immutable HTTP context-а.
    pub fn start_with_worker_count(
        context: AdaptiveHttpContext,
        worker_count: NonZeroUsize,
    ) -> Result<Self, AdaptiveTransportError> {
        let (command_sender, command_receiver) = mpsc::sync_channel(worker_count.get());
        let (outcome_sender, outcome_receiver) = mpsc::sync_channel(worker_count.get());
        let command_receiver = Arc::new(Mutex::new(command_receiver));
        for worker_index in 0..worker_count.get() {
            let worker_context = context.clone();
            let worker_commands = Arc::clone(&command_receiver);
            let worker_outcomes = outcome_sender.clone();
            thread::Builder::new()
                .name(format!("adaptive-http-fetch-{worker_index}"))
                .spawn(move || {
                    run_fetch_worker(worker_context, worker_commands, worker_outcomes);
                })
                .map_err(|_| AdaptiveTransportError::WorkerStopped)?;
        }
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
    command_receiver: Arc<Mutex<Receiver<FetchJob>>>,
    outcome_sender: SyncSender<FetchOutcome>,
) {
    loop {
        let job = {
            let Ok(receiver) = command_receiver.lock() else {
                return;
            };
            let Ok(job) = receiver.recv() else {
                return;
            };
            job
        };
        let id = job.id;
        let generation = job.generation;
        let resource_diagnostics = HttpResourceDiagnostics::started(job.purpose.resource_purpose());
        let result = fetch_with_redirects(&context, job, resource_diagnostics);
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

pub(super) fn fetch_with_redirects(
    context: &AdaptiveHttpContext,
    job: FetchJob,
    resource_diagnostics: HttpResourceDiagnostics,
) -> Result<FetchSuccess, AdaptiveTransportError> {
    if context.cancellation.is_cancelled() {
        return Err(AdaptiveTransportError::Cancelled);
    }
    let mut target = job.target;
    let mut completed_hops = RedirectHopCount::none();
    let mut forward_secrets = matches!(
        job.secret_forwarding,
        AdaptiveResourceSecretForwarding::ForwardScoped
    );

    loop {
        let (request_target, headers) = request_material(
            &context.secrets,
            &target,
            job.purpose,
            job.query_application,
            forward_secrets,
        )?;
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
        }
        .with_resource_diagnostics(resource_diagnostics);
        let hop = match context
            .session
            .fetch_bounded_single_hop(request, &context.cancellation)
        {
            Ok(hop) => hop,
            Err(source_error) => {
                context.observe_endpoint_expiry(job.generation, job.purpose, &source_error);
                return Err(AdaptiveTransportError::Source(source_error));
            }
        };
        match hop {
            HttpBoundedFetchHop::Complete(response) => {
                let range_metadata = response.range_metadata().cloned();
                return Ok(FetchSuccess {
                    final_target: target,
                    bytes: response.into_bytes(),
                    range_metadata,
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

/// Async-вариант redirect traversal, который возвращает response до body EOF.
pub(super) async fn open_stream_with_redirects(
    context: &AdaptiveHttpContext,
    job: FetchJob,
    resource_diagnostics: HttpResourceDiagnostics,
) -> Result<StreamingFetchSuccess, AdaptiveTransportError> {
    if context.cancellation.is_cancelled() {
        return Err(AdaptiveTransportError::Cancelled);
    }
    let mut target = job.target;
    let mut completed_hops = RedirectHopCount::none();
    let mut forward_secrets = matches!(
        job.secret_forwarding,
        AdaptiveResourceSecretForwarding::ForwardScoped
    );

    loop {
        let (request_target, headers) = request_material(
            &context.secrets,
            &target,
            job.purpose,
            job.query_application,
            forward_secrets,
        )?;
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
        }
        .with_resource_diagnostics(resource_diagnostics);
        let hop = context
            .session
            .open_bounded_single_hop_stream(request, &context.cancellation)
            .await;
        let hop = match hop {
            Ok(hop) => hop,
            Err(source_error) => {
                context.observe_endpoint_expiry(job.generation, job.purpose, &source_error);
                return Err(AdaptiveTransportError::Source(source_error));
            }
        };
        match hop {
            HttpBoundedStreamingFetchHop::Body(body) => {
                return Ok(StreamingFetchSuccess {
                    final_target: target,
                    body,
                });
            }
            HttpBoundedStreamingFetchHop::Redirect(redirect) => {
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

/// Завершается при global source shutdown либо supersede текущего seek intent-а.
pub(crate) async fn wait_for_any_cancellation(
    source_cancellation: &CancellationToken,
    seek_cancellation: &DemuxSeekCancellationToken,
) {
    let mut source_cancelled = std::pin::pin!(source_cancellation.cancelled());
    let mut seek_cancelled = std::pin::pin!(seek_cancellation.cancelled());
    std::future::poll_fn(|context| {
        if source_cancelled.as_mut().poll(context).is_ready()
            || seek_cancelled.as_mut().poll(context).is_ready()
        {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
}

pub(super) fn request_material(
    secrets: &SecretRequestContext,
    target: &HttpRequestTarget,
    purpose: FetchPurpose,
    query_application: AdaptiveResourceQueryApplication,
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
    let request_target = match (query_application, material.query_override_for_request()) {
        (AdaptiveResourceQueryApplication::ApplyScopedReplacement, Some(query)) => {
            target.with_query_override(query.expose_secret_for_request())?
        }
        (AdaptiveResourceQueryApplication::MergeScopedAddition, Some(query)) => {
            target.merge_extractor_query_parameters(query.expose_secret_for_request())?
        }
        (
            AdaptiveResourceQueryApplication::ApplyScopedReplacement
            | AdaptiveResourceQueryApplication::MergeScopedAddition
            | AdaptiveResourceQueryApplication::BypassScopedQuery,
            _,
        ) => target.clone(),
    };
    Ok((request_target, headers))
}

pub(super) fn wait_for_retry(
    cancellation: &CancellationToken,
    delay: Duration,
) -> Result<(), AdaptiveTransportError> {
    let deadline = Instant::now() + delay;
    loop {
        if cancellation.is_cancelled() {
            return Err(AdaptiveTransportError::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}

/// Сохраняет прежний bounded retry wait и дополнительно замечает seek supersede.
pub(super) fn wait_for_retry_with_seek(
    cancellation: &CancellationToken,
    seek_cancellation: &DemuxSeekCancellationToken,
    delay: Duration,
) -> Result<(), AdaptiveTransportError> {
    let deadline = Instant::now() + delay;
    loop {
        if cancellation.is_cancelled() || seek_cancellation.is_cancelled() {
            return Err(AdaptiveTransportError::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}
