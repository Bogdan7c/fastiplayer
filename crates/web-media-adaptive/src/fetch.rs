//! Shared S21T-authorized HTTP execution для manifest и segment owner-ов.

use std::fmt;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use media_core::DemuxSeekCancellationToken;
use source_core::{
    CancellationToken, CurrentThreadAsyncExecutor, HttpBoundedByteRange, HttpRangeResponseMetadata,
    HttpRequestTarget, HttpRequestTargetError, HttpResourceCacheOutcome, HttpResourceDiagnostics,
    HttpRetryAfter, HttpSourceSession, InterruptibleAsyncExecution, ScopedHttpCookieJar,
    ScopedHttpCookieJarError, SourceError, SourceRuntimeConfig,
};
use web_media_transport_api::{
    EndpointExpiryObserver, EndpointExpiryReason, EndpointExpirySignal, MediaComponentIdentity,
    MediaPresentation, RedirectPolicyError, SecretRequestContext, SecretRequestPurpose,
    SourceGeneration, TransportOpenRequest,
};

mod async_job;
mod execution;

pub(crate) use async_job::fetch_with_redirects_async;
pub(crate) use execution::{
    FetchExecutor, FetchJob, FetchOutcome, FetchPurpose, FetchSuccess, wait_for_any_cancellation,
};

use execution::{
    fetch_with_redirects, open_stream_with_redirects, request_material, wait_for_retry,
    wait_for_retry_with_seek,
};

use crate::completed_resource_cache::{CompletedResourceCache, CompletedResourceCacheKey};
use crate::restartable_read_interruption::AdaptiveRestartableReadAttempt;
use crate::streaming_resource::{
    AdaptiveRestartableReadAttemptBinding, AdaptiveStreamingResource, NetworkStreamingResourceOpen,
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
    pub(crate) component: MediaComponentIdentity,
    pub(crate) endpoint_expiry_observer: Option<Arc<dyn EndpointExpiryObserver>>,
    pub(crate) expected_presentation: MediaPresentation,
    pub(crate) limits: AdaptiveTransportLimits,
    pub(crate) retry: AdaptiveRetryPolicy,
    /// Shared completed-only VOD cache одного source context-а.
    completed_resource_cache: Arc<Mutex<CompletedResourceCache>>,
}

impl AdaptiveHttpContext {
    /// Собирает единственную source-core session и scoped ephemeral cookie jar.
    pub fn new(
        request: TransportOpenRequest,
        source_config: &SourceRuntimeConfig,
        limits: AdaptiveTransportLimits,
        retry: AdaptiveRetryPolicy,
    ) -> Result<Self, AdaptiveTransportError> {
        let http_target = request
            .target()
            .as_http()
            .ok_or(AdaptiveTransportError::Target(
                HttpRequestTargetError::UnsupportedScheme,
            ))?;
        let initial_material = request
            .secrets()
            .material_for(http_target, SecretRequestPurpose::Manifest)
            .ok_or(AdaptiveTransportError::SecretScopeRejected)?;
        let cookie_jar = ScopedHttpCookieJar::new(
            request.secrets().scope().request_scope_proof(),
            http_target,
            initial_material.cookies_for_request(),
            initial_material.cookie_seeds_for_request(),
        )
        .map_err(map_cookie_jar_error)?;
        let session = HttpSourceSession::new_with_cookie_jar(source_config, Arc::new(cookie_jar))?;
        let completed_cache_budget =
            usize::try_from(source_config.memory_cache_bytes()).unwrap_or(usize::MAX);
        Ok(Self {
            session,
            secrets: request.secrets().clone(),
            redirects: request.redirects(),
            cancellation: request.cancellation().clone(),
            initial_generation: request.source_generation(),
            component: request.component().clone(),
            endpoint_expiry_observer: request.endpoint_expiry_observer().cloned(),
            expected_presentation: request.presentation(),
            limits,
            retry,
            completed_resource_cache: Arc::new(Mutex::new(CompletedResourceCache::new(
                completed_cache_budget,
            ))),
        })
    }

    /// Возвращает shared cooperative cancellation owner-а.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Возвращает generation, к которой привязан весь immutable HTTP context.
    #[must_use]
    pub const fn source_generation(&self) -> SourceGeneration {
        self.initial_generation
    }

    /// Публикует expiry только для четырёх status codes и никогда не раскрывает target.
    pub(crate) fn observe_endpoint_expiry(
        &self,
        source_generation: SourceGeneration,
        purpose: FetchPurpose,
        source_error: &SourceError,
    ) {
        let SourceError::HttpStatus { status, .. } = source_error else {
            return;
        };
        let Some(reason) = EndpointExpiryReason::from_http_status(status.as_u16()) else {
            return;
        };
        let Some(observer) = self.endpoint_expiry_observer.as_ref() else {
            return;
        };
        observer.observe_endpoint_expiry(EndpointExpirySignal::new(
            self.component.clone(),
            source_generation,
            purpose.expiry_resource_kind(),
            reason,
        ));
    }

    /// Возвращает configured body bound конкретного provider-neutral resource class.
    #[must_use]
    pub const fn maximum_resource_bytes(
        &self,
        purpose: AdaptiveResourcePurpose,
    ) -> std::num::NonZeroUsize {
        match purpose {
            AdaptiveResourcePurpose::Manifest | AdaptiveResourcePurpose::ClockSynchronization => {
                self.limits.maximum_manifest_bytes
            }
            AdaptiveResourcePurpose::MediaSegment
            | AdaptiveResourcePurpose::Initialization
            | AdaptiveResourcePurpose::EncryptionKey => self.limits.maximum_segment_bytes,
        }
    }

    /// Выводит opaque secret-forwarding intent для retained effective target.
    #[must_use]
    pub fn resource_secret_forwarding_for(
        &self,
        target: &HttpRequestTarget,
    ) -> AdaptiveResourceSecretForwarding {
        if self.secrets.scope().allows(target) {
            AdaptiveResourceSecretForwarding::ForwardScoped
        } else {
            AdaptiveResourceSecretForwarding::Suppress
        }
    }

    /// Выполняет один bounded adaptive resource fetch на уже выделенном blocking worker-е.
    ///
    /// Метод намеренно не создаёт второй HTTP client или retry stack. Concrete manifest owner
    /// вызывает его только вне player-owner thread, а shared S31 redirect/cookie/cancel policy
    /// остаётся внутри этого crate.
    pub fn fetch_resource_blocking(
        &self,
        request: AdaptiveResourceFetchRequest,
    ) -> Result<AdaptiveFetchedResource, AdaptiveTransportError> {
        if self.cancellation.is_cancelled() {
            return Err(AdaptiveTransportError::Cancelled);
        }
        if request.generation != self.initial_generation {
            return Err(AdaptiveTransportError::StaleGeneration {
                current: self.initial_generation,
                received: request.generation,
            });
        }
        request.validate_bound(self.limits)?;
        let resource_diagnostics = HttpResourceDiagnostics::started(
            FetchPurpose::from(request.purpose).resource_purpose(),
        );
        let cache_key = self.completed_cache_key(&request);
        let cached_replay = cache_key
            .as_ref()
            .and_then(|cache_key| self.lock_completed_resource_cache().replay(cache_key));
        if let Some(cached_replay) = cached_replay {
            let (replay_chunks, replay_bytes) = cached_replay.diagnostic_shape();
            resource_diagnostics.record_cache_outcome(
                HttpResourceCacheOutcome::Replay,
                replay_chunks,
                replay_bytes,
            );
            return Ok(Self::buffered_resource_from_replay(cached_replay));
        }
        resource_diagnostics.record_cache_outcome(
            if cache_key.is_some() {
                HttpResourceCacheOutcome::Miss
            } else {
                HttpResourceCacheOutcome::Ineligible
            },
            0,
            0,
        );
        let mut attempt = std::num::NonZeroU8::MIN;
        loop {
            let result = fetch_with_redirects(
                self,
                FetchJob {
                    id: 0,
                    generation: request.generation,
                    target: request.target.clone(),
                    byte_range: request.byte_range,
                    maximum_body_bytes: request.maximum_body_bytes,
                    purpose: request.purpose.into(),
                    query_application: request.query_application,
                    secret_forwarding: request.secret_forwarding,
                },
                resource_diagnostics,
            );
            match result {
                Ok(success) => {
                    let fetched_resource = AdaptiveFetchedResource {
                        final_target: success.final_target,
                        bytes: success.bytes,
                        range_metadata: success.range_metadata,
                    };
                    if let Some(cache_key) = cache_key {
                        self.cache_buffered_resource(cache_key, &fetched_resource);
                    }
                    return Ok(fetched_resource);
                }
                Err(error) if error.is_retryable() && attempt < self.retry.maximum_attempts() => {
                    let delay = self
                        .retry
                        .retry_delay_after(attempt, error.http_retry_after());
                    wait_for_retry(self.cancellation(), delay)?;
                    attempt = std::num::NonZeroU8::new(attempt.get().saturating_add(1))
                        .unwrap_or(attempt);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Материализует completed replay для buffered caller-а без нового network side effect.
    fn buffered_resource_from_replay(
        replay: crate::completed_resource_cache::CompletedResourceReplay,
    ) -> AdaptiveFetchedResource {
        let (final_target, chunks, range_metadata) = replay.into_parts();
        let mut bytes = Vec::new();
        for chunk in chunks.iter() {
            bytes.extend_from_slice(chunk);
        }
        AdaptiveFetchedResource {
            final_target,
            bytes,
            range_metadata,
        }
    }

    /// Кладёт только полностью полученный buffered response в общий bounded VOD LRU.
    fn cache_buffered_resource(
        &self,
        cache_key: CompletedResourceCacheKey,
        fetched_resource: &AdaptiveFetchedResource,
    ) {
        self.lock_completed_resource_cache().insert_completed(
            cache_key,
            fetched_resource.final_target.clone(),
            vec![Bytes::copy_from_slice(&fetched_resource.bytes)],
            fetched_resource.range_metadata.clone(),
        );
    }

    /// Открывает bounded resource body до полной загрузки на текущем demux worker-е.
    ///
    /// Current-thread runtime не создаёт дополнительный OS thread. Redirect, cookie,
    /// retry, generation и secret policy остаются теми же, что у buffered path-а.
    pub fn open_resource_streaming_blocking(
        &self,
        request: AdaptiveResourceFetchRequest,
        seek_cancellation: DemuxSeekCancellationToken,
    ) -> Result<AdaptiveStreamingResource, AdaptiveTransportError> {
        self.open_resource_streaming_blocking_with_restartable_read_binding(
            request,
            seek_cancellation,
            AdaptiveRestartableReadAttemptBinding::Absent,
        )
    }

    /// Открывает offside body с отдельным disarmed attempt-ом будущего committed resource-а.
    ///
    /// Сам open/probe слушает только request-scoped cancellation. Caller обязан вызвать
    /// `attempt.arm_as_current()` лишь после successful proof и transactional commit-а.
    pub fn open_resource_streaming_blocking_with_restartable_read_attempt(
        &self,
        request: AdaptiveResourceFetchRequest,
        seek_cancellation: DemuxSeekCancellationToken,
        restartable_read_attempt: AdaptiveRestartableReadAttempt,
    ) -> Result<AdaptiveStreamingResource, AdaptiveTransportError> {
        self.open_resource_streaming_blocking_with_restartable_read_binding(
            request,
            seek_cancellation,
            AdaptiveRestartableReadAttemptBinding::Attempt(restartable_read_attempt),
        )
    }

    /// Общий owner streaming open-а не дублирует redirect/cache/retry semantics двух API.
    fn open_resource_streaming_blocking_with_restartable_read_binding(
        &self,
        request: AdaptiveResourceFetchRequest,
        seek_cancellation: DemuxSeekCancellationToken,
        restartable_read_attempt: AdaptiveRestartableReadAttemptBinding,
    ) -> Result<AdaptiveStreamingResource, AdaptiveTransportError> {
        if self.cancellation.is_cancelled() || seek_cancellation.is_cancelled() {
            return Err(AdaptiveTransportError::Cancelled);
        }
        if request.generation != self.initial_generation {
            return Err(AdaptiveTransportError::StaleGeneration {
                current: self.initial_generation,
                received: request.generation,
            });
        }
        request.validate_bound(self.limits)?;
        let resource_diagnostics = HttpResourceDiagnostics::started(
            FetchPurpose::from(request.purpose).resource_purpose(),
        );
        let cache_key = self.completed_cache_key(&request);
        let (cached_replay, cache_budget_bytes) = match cache_key.as_ref() {
            Some(cache_key) => {
                let mut cache = self.lock_completed_resource_cache();
                (cache.replay(cache_key), cache.budget_bytes())
            }
            None => (None, 0),
        };
        if let Some(cached_replay) = cached_replay {
            let (replay_chunks, replay_bytes) = cached_replay.diagnostic_shape();
            resource_diagnostics.record_cache_outcome(
                HttpResourceCacheOutcome::Replay,
                replay_chunks,
                replay_bytes,
            );
            return Ok(AdaptiveStreamingResource::from_completed_replay(
                self.clone(),
                request.generation,
                request.purpose.into(),
                seek_cancellation,
                cached_replay,
                resource_diagnostics,
                restartable_read_attempt,
            ));
        }
        resource_diagnostics.record_cache_outcome(
            if cache_key.is_some() {
                HttpResourceCacheOutcome::Miss
            } else {
                HttpResourceCacheOutcome::Ineligible
            },
            0,
            0,
        );
        let executor =
            CurrentThreadAsyncExecutor::new().map_err(|_| AdaptiveTransportError::WorkerStopped)?;
        let mut attempt = std::num::NonZeroU8::MIN;
        loop {
            let result = executor.block_on_interruptible(
                open_stream_with_redirects(
                    self,
                    FetchJob {
                        id: 0,
                        generation: request.generation,
                        target: request.target.clone(),
                        byte_range: request.byte_range,
                        maximum_body_bytes: request.maximum_body_bytes,
                        purpose: request.purpose.into(),
                        query_application: request.query_application,
                        secret_forwarding: request.secret_forwarding,
                    },
                    resource_diagnostics,
                ),
                wait_for_any_cancellation(self.cancellation(), &seek_cancellation),
            );
            let result = match result {
                InterruptibleAsyncExecution::Completed(result) => result,
                InterruptibleAsyncExecution::Interrupted => Err(AdaptiveTransportError::Cancelled),
            };
            match result {
                Ok(success) => {
                    return Ok(AdaptiveStreamingResource::from_network(
                        NetworkStreamingResourceOpen {
                            context: self.clone(),
                            source_generation: request.generation,
                            purpose: request.purpose.into(),
                            seek_cancellation,
                            final_target: success.final_target,
                            executor,
                            body: success.body,
                            resource_diagnostics,
                            cache_key,
                            cache_budget_bytes,
                            restartable_read_attempt,
                        },
                    ));
                }
                Err(error) if error.is_retryable() && attempt < self.retry.maximum_attempts() => {
                    let delay = self
                        .retry
                        .retry_delay_after(attempt, error.http_retry_after());
                    wait_for_retry_with_seek(self.cancellation(), &seek_cancellation, delay)?;
                    attempt = std::num::NonZeroU8::new(attempt.get().saturating_add(1))
                        .unwrap_or(attempt);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Создаёт cache identity только для полностью конечных media/init resources.
    fn completed_cache_key(
        &self,
        request: &AdaptiveResourceFetchRequest,
    ) -> Option<CompletedResourceCacheKey> {
        if self.expected_presentation != MediaPresentation::Vod
            || !matches!(
                request.purpose,
                AdaptiveResourcePurpose::MediaSegment | AdaptiveResourcePurpose::Initialization
            )
        {
            return None;
        }

        Some(CompletedResourceCacheKey::new(
            request.target.clone(),
            request.byte_range,
            request.maximum_body_bytes,
            request.purpose,
            request.query_application,
            request.secret_forwarding,
        ))
    }

    /// Poison не скрывается: cache восстанавливается, а diagnostics фиксирует panic boundary.
    pub(crate) fn lock_completed_resource_cache(
        &self,
    ) -> std::sync::MutexGuard<'_, CompletedResourceCache> {
        self.completed_resource_cache
            .lock()
            // Cache ownership остаётся recoverable после чужой panic; payload/accounting
            // invariants дополнительно защищены checked arithmetic и RAII reservations.
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    /// Current committed body read прерван для whole-parser restart-а после accepted seek.
    #[error("adaptive committed resource read interrupted for restart")]
    RestartableReadInterrupted,
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
    /// Caller попытался обойти общий configured bound конкретного resource класса.
    #[error("adaptive resource request превышает configured {purpose:?} bound")]
    ResourceBoundExceeded {
        /// Resource class без locator или secret material.
        purpose: AdaptiveResourcePurpose,
    },
    /// Purpose-specific security policy была ослаблена caller-ом.
    #[error("adaptive resource request violates configured {purpose:?} policy")]
    InvalidResourcePolicy {
        /// Resource class без locator или secret material.
        purpose: AdaptiveResourcePurpose,
    },
}

impl AdaptiveTransportError {
    /// Возвращает HTTP status без URL/request payload для higher-level policy.
    #[must_use]
    pub fn http_status_code(&self) -> Option<u16> {
        match self {
            Self::Source(SourceError::HttpStatus { status, .. }) => Some(status.as_u16()),
            _ => None,
        }
    }
}

/// Provider-neutral purpose одного bounded adaptive resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveResourcePurpose {
    /// Master или media manifest.
    Manifest,
    /// Внешний UTC clock, который никогда не наследует media-source secrets.
    ClockSynchronization,
    /// Media segment/fragment.
    MediaSegment,
    /// Initialization resource, например ISO BMFF init section.
    Initialization,
    /// Encryption key resource.
    EncryptionKey,
}

/// Явно отделяет shared S21T replacement от уже composed provider target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveResourceQueryApplication {
    /// Применить generic purpose-scoped query replacement из S21T material.
    ApplyScopedReplacement,
    /// Слить purpose-scoped query material с target на каждом разрешённом redirect hop.
    MergeScopedAddition,
    /// Намеренно не применять query material, сохранив scoped headers/cookies.
    BypassScopedQuery,
}

/// Явный intent раскрытия transient request secrets для одного resource lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveResourceSecretForwarding {
    /// Применять только material, разрешённый retained S21T scope.
    ForwardScoped,
    /// Не запрашивать headers, cookies или query material у retained secret context.
    Suppress,
}

/// Один generation-fenced bounded resource request без manifest-specific типов.
#[derive(Debug, Clone)]
pub struct AdaptiveResourceFetchRequest {
    generation: SourceGeneration,
    target: HttpRequestTarget,
    byte_range: Option<HttpBoundedByteRange>,
    maximum_body_bytes: std::num::NonZeroUsize,
    purpose: AdaptiveResourcePurpose,
    query_application: AdaptiveResourceQueryApplication,
    secret_forwarding: AdaptiveResourceSecretForwarding,
}

impl AdaptiveResourceFetchRequest {
    /// Создаёт full-body request с явным purpose и query contract.
    #[must_use]
    pub const fn full(
        generation: SourceGeneration,
        target: HttpRequestTarget,
        maximum_body_bytes: std::num::NonZeroUsize,
        purpose: AdaptiveResourcePurpose,
        query_application: AdaptiveResourceQueryApplication,
    ) -> Self {
        Self {
            generation,
            target,
            byte_range: None,
            maximum_body_bytes,
            purpose,
            query_application,
            secret_forwarding: AdaptiveResourceSecretForwarding::ForwardScoped,
        }
    }

    /// Создаёт exact Range request; source-core проверит `206` и `Content-Range`.
    #[must_use]
    pub const fn range(
        generation: SourceGeneration,
        target: HttpRequestTarget,
        byte_range: HttpBoundedByteRange,
        maximum_body_bytes: std::num::NonZeroUsize,
        purpose: AdaptiveResourcePurpose,
        query_application: AdaptiveResourceQueryApplication,
    ) -> Self {
        Self {
            generation,
            target,
            byte_range: Some(byte_range),
            maximum_body_bytes,
            purpose,
            query_application,
            secret_forwarding: AdaptiveResourceSecretForwarding::ForwardScoped,
        }
    }

    /// Создаёт bounded clock request с fail-closed secret/query policy.
    #[must_use]
    pub const fn clock_synchronization(
        generation: SourceGeneration,
        target: HttpRequestTarget,
        maximum_body_bytes: std::num::NonZeroUsize,
    ) -> Self {
        Self {
            generation,
            target,
            byte_range: None,
            maximum_body_bytes,
            purpose: AdaptiveResourcePurpose::ClockSynchronization,
            query_application: AdaptiveResourceQueryApplication::BypassScopedQuery,
            secret_forwarding: AdaptiveResourceSecretForwarding::Suppress,
        }
    }

    /// Заменяет default `ForwardScoped` на caller-proven typed intent.
    #[must_use]
    pub const fn with_secret_forwarding(
        mut self,
        secret_forwarding: AdaptiveResourceSecretForwarding,
    ) -> Self {
        self.secret_forwarding = secret_forwarding;
        self
    }

    fn validate_bound(
        &self,
        limits: AdaptiveTransportLimits,
    ) -> Result<(), AdaptiveTransportError> {
        let maximum_allowed = match self.purpose {
            AdaptiveResourcePurpose::Manifest | AdaptiveResourcePurpose::ClockSynchronization => {
                limits.maximum_manifest_bytes
            }
            AdaptiveResourcePurpose::MediaSegment
            | AdaptiveResourcePurpose::Initialization
            | AdaptiveResourcePurpose::EncryptionKey => limits.maximum_segment_bytes,
        };
        let requested_bytes = self.byte_range.map_or_else(
            || u64::try_from(self.maximum_body_bytes.get()).unwrap_or(u64::MAX),
            |range| u64::try_from(range.length().get()).unwrap_or(u64::MAX),
        );
        let maximum_allowed = u64::try_from(maximum_allowed.get()).unwrap_or(u64::MAX);
        if requested_bytes > maximum_allowed {
            return Err(AdaptiveTransportError::ResourceBoundExceeded {
                purpose: self.purpose,
            });
        }
        if self.purpose == AdaptiveResourcePurpose::ClockSynchronization
            && (self.byte_range.is_some()
                || self.query_application != AdaptiveResourceQueryApplication::BypassScopedQuery
                || self.secret_forwarding != AdaptiveResourceSecretForwarding::Suppress)
        {
            return Err(AdaptiveTransportError::InvalidResourcePolicy {
                purpose: self.purpose,
            });
        }
        Ok(())
    }
}

/// Успешно загруженный resource и effective post-redirect base.
pub struct AdaptiveFetchedResource {
    final_target: HttpRequestTarget,
    bytes: Vec<u8>,
    range_metadata: Option<HttpRangeResponseMetadata>,
}

impl AdaptiveFetchedResource {
    /// Effective target нужен concrete manifest owner-у как base URI.
    #[must_use]
    pub const fn final_target(&self) -> &HttpRequestTarget {
        &self.final_target
    }

    /// Передаёт bytes следующему bounded parser/crypto owner-у.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Возвращает validated wire metadata exact Range response-а.
    #[must_use]
    pub const fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        self.range_metadata.as_ref()
    }

    /// Передаёт владение bytes без дополнительного копирования.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl fmt::Debug for AdaptiveFetchedResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveFetchedResource")
            .field("final_target", &self.final_target)
            .field("bytes", &self.bytes.len())
            .finish()
    }
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
            | Self::RestartableReadInterrupted
            | Self::Source(_)
            | Self::Target(_)
            | Self::Redirect(_)
            | Self::SecretScopeRejected
            | Self::ExplicitCookieHeader
            | Self::WorkerStopped
            | Self::StaleGeneration { .. }
            | Self::ResourceBoundExceeded { .. }
            | Self::InvalidResourcePolicy { .. } => false,
        }
    }

    /// Возвращает только проверенный server delay без raw response header-а.
    pub(crate) fn http_retry_after(&self) -> HttpRetryAfter {
        match self {
            Self::Source(SourceError::HttpStatus { retry_after, .. }) => *retry_after,
            _ => HttpRetryAfter::Unavailable,
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

#[cfg(test)]
mod retry_contract_tests;
