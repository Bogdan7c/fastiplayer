//! Nonblocking bounded manifest fetch/refresh lifecycle.

use std::num::NonZeroU8;
use std::time::{Duration, Instant};

use bytes::Bytes;
use source_core::HttpRequestTarget;
use web_media_transport_api::SourceGeneration;

use crate::fetch::{FetchExecutor, FetchJob, FetchOutcome, FetchPurpose};
use crate::{AdaptiveHttpContext, AdaptiveTransportError};

/// Effective manifest response target, используемый как URI base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestBaseUri(HttpRequestTarget);

impl ManifestBaseUri {
    /// Разрешает protocol-owned relative reference по WHATWG URL rules.
    pub fn resolve(
        &self,
        reference: &str,
    ) -> Result<HttpRequestTarget, source_core::HttpRequestTargetError> {
        self.0.resolve_reference(reference)
    }

    /// Возвращает safe validated effective target.
    #[must_use]
    pub const fn effective_target(&self) -> &HttpRequestTarget {
        &self.0
    }
}

/// Caller request initial manifest-а либо live refresh-а.
#[derive(Debug, Clone)]
pub struct ManifestFetchRequest {
    /// Exact manifest target.
    target: HttpRequestTarget,
    /// Source generation, которой принадлежит fetch.
    generation: SourceGeneration,
}

impl ManifestFetchRequest {
    /// Создаёт generation-fenced manifest request.
    #[must_use]
    pub const fn new(target: HttpRequestTarget, generation: SourceGeneration) -> Self {
        Self { target, generation }
    }

    /// Возвращает generation для refresh fencing.
    #[must_use]
    pub const fn generation(&self) -> SourceGeneration {
        self.generation
    }
}

/// Bounded raw manifest после redirect resolution, до concrete parsing.
#[derive(Debug, Clone)]
pub struct ManifestResource {
    generation: SourceGeneration,
    base_uri: ManifestBaseUri,
    bytes: Bytes,
}

impl ManifestResource {
    /// Generation fetch-а.
    #[must_use]
    pub const fn generation(&self) -> SourceGeneration {
        self.generation
    }

    /// Effective response base URI.
    #[must_use]
    pub const fn base_uri(&self) -> &ManifestBaseUri {
        &self.base_uri
    }

    /// Exact raw manifest body.
    #[must_use]
    pub const fn bytes(&self) -> &Bytes {
        &self.bytes
    }
}

/// Explicit manifest readiness без `None == pending/EOF`.
#[derive(Debug)]
pub enum ManifestPoll {
    /// Новый generation-matched body готов к concrete parsing.
    Ready(ManifestResource),
    /// Fetch/retry ещё выполняется; player-owner не должен ждать.
    TemporarilyUnavailable {
        /// Earliest useful next poll.
        retry_after: Duration,
    },
    /// Typed terminal failure current request-а.
    Failed(AdaptiveTransportError),
    /// Shared cancellation завершила lifecycle.
    Cancelled,
}

#[derive(Debug, Clone)]
struct PendingManifest {
    request: ManifestFetchRequest,
    attempt: NonZeroU8,
    retry_not_before: Instant,
    job_id: u64,
    submitted: bool,
}

/// Bounded one-resource-at-a-time manifest fetch owner.
pub struct AdaptiveManifestFetcher {
    context: AdaptiveHttpContext,
    executor: FetchExecutor,
    current: Option<PendingManifest>,
    current_generation: SourceGeneration,
    generation_claimed: bool,
    next_job_id: u64,
}

impl AdaptiveManifestFetcher {
    /// Создаёт worker, но не выполняет network I/O до `request`.
    pub fn new(context: AdaptiveHttpContext) -> Result<Self, AdaptiveTransportError> {
        let executor = FetchExecutor::start(context.clone())?;
        Ok(Self {
            current_generation: context.initial_generation,
            context,
            executor,
            current: None,
            generation_claimed: false,
            next_job_id: 1,
        })
    }

    /// Начинает initial fetch либо заменяет его strictly current/newer refresh-ом.
    pub fn request(
        &mut self,
        request: ManifestFetchRequest,
        now: Instant,
    ) -> Result<(), AdaptiveTransportError> {
        if request.generation < self.current_generation
            || (self.generation_claimed && request.generation == self.current_generation)
        {
            return Err(AdaptiveTransportError::StaleGeneration {
                current: self.current_generation,
                received: request.generation,
            });
        }
        self.current_generation = request.generation;
        self.generation_claimed = true;
        let job_id = self.allocate_job_id();
        self.current = Some(PendingManifest {
            request,
            attempt: NonZeroU8::MIN,
            retry_not_before: now,
            job_id,
            submitted: false,
        });
        Ok(())
    }

    /// Poll-ит worker channels и никогда не ждёт network/thread completion.
    pub fn poll(&mut self, now: Instant) -> ManifestPoll {
        if self.context.cancellation.is_cancelled() {
            return ManifestPoll::Cancelled;
        }
        match self.executor.try_receive() {
            Ok(Some(outcome)) => {
                if let Some(result) = self.accept_outcome(outcome, now) {
                    return result;
                }
            }
            Ok(None) => {}
            Err(error) => return ManifestPoll::Failed(error),
        }

        let Some(pending) = &mut self.current else {
            return ManifestPoll::TemporarilyUnavailable {
                retry_after: Duration::from_millis(1),
            };
        };
        if now < pending.retry_not_before {
            return ManifestPoll::TemporarilyUnavailable {
                retry_after: pending.retry_not_before.duration_since(now),
            };
        }
        if !pending.submitted {
            let job = FetchJob {
                id: pending.job_id,
                generation: pending.request.generation,
                target: pending.request.target.clone(),
                byte_range: None,
                maximum_body_bytes: self.context.limits.maximum_manifest_bytes,
                purpose: FetchPurpose::Manifest,
                query_application:
                    crate::fetch::AdaptiveResourceQueryApplication::ApplyScopedReplacement,
            };
            match self.executor.try_submit(job) {
                Ok(submitted) => pending.submitted = submitted,
                Err(error) => return ManifestPoll::Failed(error),
            }
        }
        ManifestPoll::TemporarilyUnavailable {
            retry_after: Duration::from_millis(1),
        }
    }

    fn accept_outcome(&mut self, outcome: FetchOutcome, now: Instant) -> Option<ManifestPoll> {
        let pending = self.current.as_mut()?;
        if outcome.id != pending.job_id || outcome.generation != self.current_generation {
            return None;
        }
        match outcome.result {
            Ok(success) => {
                let generation = pending.request.generation;
                self.current = None;
                Some(ManifestPoll::Ready(ManifestResource {
                    generation,
                    base_uri: ManifestBaseUri(success.final_target),
                    bytes: Bytes::from(success.bytes),
                }))
            }
            Err(AdaptiveTransportError::Cancelled) => {
                self.current = None;
                Some(ManifestPoll::Cancelled)
            }
            Err(error)
                if error.is_retryable()
                    && pending.attempt.get() < self.context.retry.maximum_attempts().get() =>
            {
                let delay = self.context.retry.backoff_after(pending.attempt);
                pending.attempt =
                    NonZeroU8::new(pending.attempt.get() + 1).expect("bounded attempt");
                pending.retry_not_before = now + delay;
                pending.job_id = self.next_job_id;
                self.next_job_id = self.next_job_id.wrapping_add(1).max(1);
                pending.submitted = false;
                Some(ManifestPoll::TemporarilyUnavailable { retry_after: delay })
            }
            Err(error) => {
                self.current = None;
                Some(ManifestPoll::Failed(error))
            }
        }
    }

    fn allocate_job_id(&mut self) -> u64 {
        let allocated = self.next_job_id;
        self.next_job_id = self.next_job_id.wrapping_add(1).max(1);
        allocated
    }
}
