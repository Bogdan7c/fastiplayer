//! Pull-based lifecycle network и completed-cache adaptive response body.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use media_core::DemuxSeekCancellationToken;
use source_core::{
    CancellationToken, CurrentThreadAsyncExecutor, HttpBoundedStreamingBody,
    HttpRangeResponseMetadata, HttpRequestAttemptId, HttpRequestTarget, HttpResourceCorrelationId,
    HttpResourceDiagnostics, InterruptibleAsyncExecution, SourceError,
};
use web_media_transport_api::SourceGeneration;

use crate::completed_resource_cache::{
    CompletedResourceCacheKey, CompletedResourceReplay, CompletedResourceReservationOutcome,
    completed_chunk_charge_bytes, completed_entry_base_charge_bytes,
};
use crate::fetch::{
    AdaptiveHttpContext, AdaptiveTransportError, FetchPurpose, wait_for_any_cancellation,
};
use crate::restartable_read_interruption::{
    AdaptiveRestartableReadAttempt, AdaptiveRestartableReadCompletion,
    AdaptiveRestartableReadGuard, AdaptiveRestartableReadStart,
};

/// Explicit attachment сохраняет legacy streaming open без мутного `Option`.
#[derive(Clone)]
pub(crate) enum AdaptiveRestartableReadAttemptBinding {
    /// Legacy/generic resource не участвует в committed-read interruption protocol-е.
    Absent,
    /// Attempt прикреплён до move, но остаётся disarmed до owner commit-а.
    Attempt(AdaptiveRestartableReadAttempt),
}

impl AdaptiveRestartableReadAttemptBinding {
    /// Network path atomically занимает current slot либо сохраняет offside disarmed.
    fn begin_network_read(&self) -> AdaptiveRestartableReadStart {
        match self {
            Self::Absent => AdaptiveRestartableReadStart::Disarmed,
            Self::Attempt(attempt) => attempt.begin_network_read(),
        }
    }
}

/// Pull cursor либо читает active HTTP response, либо shallow cached replay.
enum AdaptiveStreamingResourceBody {
    Network(Box<NetworkStreamingResourceBody>),
    CompletedReplay {
        chunks: Arc<[Bytes]>,
        next_chunk_index: usize,
        received_body_bytes: usize,
        range_metadata: Option<HttpRangeResponseMetadata>,
    },
    Closed {
        received_body_bytes: usize,
        range_metadata: Option<HttpRangeResponseMetadata>,
    },
}

/// Большой runtime/body state boxed только для network path-а, не для cached chunks.
struct NetworkStreamingResourceBody {
    executor: CurrentThreadAsyncExecutor,
    body: HttpBoundedStreamingBody,
}

/// Internal read outcome не смешивает restartable unwind с source cancellation/failure.
enum AdaptiveStreamingReadResult {
    /// Existing HTTP/source result сохраняет прежнюю mapping semantics.
    Source(Result<Option<Bytes>, SourceError>),
    /// Current committed body future уничтожен по отдельному active-read signal-у.
    RestartableReadInterrupted,
}

impl NetworkStreamingResourceBody {
    /// Ждёт chunk и все три независимые interrupt authority одним executor future.
    fn next_chunk(
        &mut self,
        source_cancellation: &CancellationToken,
        seek_cancellation: &DemuxSeekCancellationToken,
        restartable_read_attempt: &AdaptiveRestartableReadAttemptBinding,
    ) -> AdaptiveStreamingReadResult {
        match restartable_read_attempt.begin_network_read() {
            AdaptiveRestartableReadStart::Disarmed => {
                let execution = self.executor.block_on_interruptible(
                    self.body.next_chunk(source_cancellation),
                    wait_for_any_cancellation(source_cancellation, seek_cancellation),
                );
                AdaptiveStreamingReadResult::Source(match execution {
                    InterruptibleAsyncExecution::Completed(result) => result,
                    InterruptibleAsyncExecution::Interrupted => Err(SourceError::Cancelled),
                })
            }
            AdaptiveRestartableReadStart::InterruptedOrSuperseded => {
                AdaptiveStreamingReadResult::RestartableReadInterrupted
            }
            AdaptiveRestartableReadStart::Armed(read_guard) => {
                let execution = self.executor.block_on_interruptible(
                    self.body.next_chunk(source_cancellation),
                    wait_for_cancellation_or_restartable_read(
                        source_cancellation,
                        seek_cancellation,
                        &read_guard,
                    ),
                );
                if source_cancellation.is_cancelled() || seek_cancellation.is_cancelled() {
                    return AdaptiveStreamingReadResult::Source(Err(SourceError::Cancelled));
                }
                match execution {
                    InterruptibleAsyncExecution::Interrupted => {
                        AdaptiveStreamingReadResult::RestartableReadInterrupted
                    }
                    InterruptibleAsyncExecution::Completed(result) => match read_guard.finish() {
                        AdaptiveRestartableReadCompletion::Completed => {
                            AdaptiveStreamingReadResult::Source(result)
                        }
                        AdaptiveRestartableReadCompletion::InterruptedOrSuperseded => {
                            AdaptiveStreamingReadResult::RestartableReadInterrupted
                        }
                    },
                }
            }
        }
    }
}

/// Один future ждёт global shutdown, request supersede и exact committed attempt epoch.
async fn wait_for_cancellation_or_restartable_read(
    source_cancellation: &CancellationToken,
    seek_cancellation: &DemuxSeekCancellationToken,
    read_guard: &AdaptiveRestartableReadGuard,
) {
    let mut ordinary_cancellation = std::pin::pin!(wait_for_any_cancellation(
        source_cancellation,
        seek_cancellation
    ));
    let mut restartable_interruption = std::pin::pin!(read_guard.interrupted());
    std::future::poll_fn(|context| {
        if ordinary_cancellation.as_mut().poll(context).is_ready()
            || restartable_interruption.as_mut().poll(context).is_ready()
        {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
}

impl AdaptiveStreamingResourceBody {
    /// Возвращает число bytes, уже отданных текущему consumer-у.
    const fn received_body_bytes(&self) -> usize {
        match self {
            Self::Network(network) => network.body.received_body_bytes(),
            Self::CompletedReplay {
                received_body_bytes,
                ..
            }
            | Self::Closed {
                received_body_bytes,
                ..
            } => *received_body_bytes,
        }
    }

    /// Возвращает validated Range metadata для network либо cached response-а.
    fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        match self {
            Self::Network(network) => network.body.range_metadata(),
            Self::CompletedReplay { range_metadata, .. } => range_metadata.as_ref(),
            Self::Closed { range_metadata, .. } => range_metadata.as_ref(),
        }
    }

    /// Немедленно drop-ает active HTTP response/runtime, сохраняя только accounting metadata.
    fn close(&mut self) {
        let received_body_bytes = self.received_body_bytes();
        let range_metadata = self.range_metadata().cloned();
        *self = Self::Closed {
            received_body_bytes,
            range_metadata,
        };
    }
}

/// Bounded local accumulator не становится shared entry до validated EOF.
struct PendingCompletedResourceAdmission {
    context: AdaptiveHttpContext,
    key: Option<CompletedResourceCacheKey>,
    maximum_cached_bytes: usize,
    body_bytes: usize,
    reservation_charge_bytes: usize,
    chunks: Vec<Bytes>,
}

/// Typed result не смешивает ignored empty transport chunk и budget exhaustion.
enum PendingChunkRecordOutcome {
    Recorded,
    IgnoredEmpty,
    BudgetExceeded,
}

impl PendingCompletedResourceAdmission {
    /// Резервирует initial entry/key/target metadata в общем source-local budget-е.
    fn new(
        context: AdaptiveHttpContext,
        key: CompletedResourceCacheKey,
        final_target: &HttpRequestTarget,
        maximum_cached_bytes: usize,
    ) -> Option<Self> {
        let initial_charge_bytes = completed_entry_base_charge_bytes(&key, final_target)?;
        if maximum_cached_bytes == 0 {
            return None;
        }
        let reservation_outcome = context
            .lock_completed_resource_cache()
            .reserve_pending(initial_charge_bytes);
        if reservation_outcome == CompletedResourceReservationOutcome::BudgetExceeded {
            return None;
        }
        Some(Self {
            context,
            key: Some(key),
            maximum_cached_bytes,
            body_bytes: 0,
            reservation_charge_bytes: initial_charge_bytes,
            chunks: Vec::new(),
        })
    }

    /// Retain-ит непустой chunk только после общей reservation без payload copy.
    fn record_chunk(&mut self, chunk: &Bytes) -> PendingChunkRecordOutcome {
        if chunk.is_empty() {
            return PendingChunkRecordOutcome::IgnoredEmpty;
        }
        let Some(next_body_bytes) = self.body_bytes.checked_add(chunk.len()) else {
            return PendingChunkRecordOutcome::BudgetExceeded;
        };
        if next_body_bytes > self.maximum_cached_bytes {
            return PendingChunkRecordOutcome::BudgetExceeded;
        }
        let Some(chunk_charge_bytes) = completed_chunk_charge_bytes(chunk) else {
            return PendingChunkRecordOutcome::BudgetExceeded;
        };
        let Some(next_reservation_charge_bytes) = self
            .reservation_charge_bytes
            .checked_add(chunk_charge_bytes)
        else {
            return PendingChunkRecordOutcome::BudgetExceeded;
        };
        let reservation_outcome = self
            .context
            .lock_completed_resource_cache()
            .reserve_pending(chunk_charge_bytes);
        if reservation_outcome == CompletedResourceReservationOutcome::BudgetExceeded {
            return PendingChunkRecordOutcome::BudgetExceeded;
        }

        self.body_bytes = next_body_bytes;
        self.reservation_charge_bytes = next_reservation_charge_bytes;
        self.chunks.push(chunk.clone());
        PendingChunkRecordOutcome::Recorded
    }

    /// Переводит reservation в completed LRU entry только после validated EOF.
    fn commit(
        mut self,
        final_target: HttpRequestTarget,
        range_metadata: Option<HttpRangeResponseMetadata>,
    ) {
        if self.body_bytes == 0 {
            return;
        }
        let key = self
            .key
            .take()
            .expect("active pending admission обязан владеть cache key");
        let chunks: Arc<[Bytes]> = std::mem::take(&mut self.chunks).into();
        let reservation_charge_bytes = self.reservation_charge_bytes;
        self.context.lock_completed_resource_cache().commit_pending(
            reservation_charge_bytes,
            key,
            final_target,
            chunks,
            range_metadata,
        );
        self.reservation_charge_bytes = 0;
    }
}

impl Drop for PendingCompletedResourceAdmission {
    /// Cancel/error/resource drop всегда возвращают shared pending reservation владельцу cache.
    fn drop(&mut self) {
        if self.reservation_charge_bytes == 0 {
            return;
        }
        self.context
            .lock_completed_resource_cache()
            .release_pending(self.reservation_charge_bytes);
        self.reservation_charge_bytes = 0;
    }
}

/// Открытый bounded HTTP body, который demux worker читает pull-based.
pub struct AdaptiveStreamingResource {
    /// Shared policy/expiry owner нужен для cancellation и typed observation.
    context: AdaptiveHttpContext,
    /// Generation сохраняет expiry fencing исходного запроса.
    source_generation: SourceGeneration,
    /// Resource class сохраняет typed expiry semantics.
    purpose: FetchPurpose,
    /// Отмена конкретного seek немедленно drop-ает pending HTTP future.
    seek_cancellation: DemuxSeekCancellationToken,
    /// Attempt остаётся disarmed во время proof и arm-ится HLS owner-ом после commit-а.
    restartable_read_attempt: AdaptiveRestartableReadAttemptBinding,
    /// Effective post-redirect target нужен только provider owner-у.
    final_target: HttpRequestTarget,
    /// Единственный network cursor либо completed shallow replay.
    body: AdaptiveStreamingResourceBody,
    /// Pending chunks ограничены cache budget и публикуются только на HTTP EOF.
    pending_cache_admission: Option<PendingCompletedResourceAdmission>,
    /// EOF/cache admission выполняются ровно один раз.
    body_completed: bool,
    /// Accepted active interrupt остаётся terminal при ошибочном повторном read-е.
    restartable_read_interrupted: bool,
    /// Secret-free identity связывает network attempt либо cache replay с logical resource-ом.
    resource_diagnostics: HttpResourceDiagnostics,
    /// Physical id сохраняется после EOF; у cache replay его принципиально нет.
    network_request_attempt_id: Option<HttpRequestAttemptId>,
}

/// Named handoff request/open owner-а в pull-based body lifecycle.
pub(crate) struct NetworkStreamingResourceOpen {
    pub(crate) context: AdaptiveHttpContext,
    pub(crate) source_generation: SourceGeneration,
    pub(crate) purpose: FetchPurpose,
    pub(crate) seek_cancellation: DemuxSeekCancellationToken,
    pub(crate) restartable_read_attempt: AdaptiveRestartableReadAttemptBinding,
    pub(crate) final_target: HttpRequestTarget,
    pub(crate) executor: CurrentThreadAsyncExecutor,
    pub(crate) body: HttpBoundedStreamingBody,
    pub(crate) resource_diagnostics: HttpResourceDiagnostics,
    pub(crate) cache_key: Option<CompletedResourceCacheKey>,
    pub(crate) cache_budget_bytes: usize,
}

impl AdaptiveStreamingResource {
    /// Создаёт network cursor с completed-only admission после validated EOF.
    pub(crate) fn from_network(open: NetworkStreamingResourceOpen) -> Self {
        let network_request_attempt_id = open.body.request_attempt_id();
        let pending_cache_admission = open.cache_key.and_then(|key| {
            PendingCompletedResourceAdmission::new(
                open.context.clone(),
                key,
                &open.final_target,
                open.cache_budget_bytes,
            )
        });
        Self {
            context: open.context,
            source_generation: open.source_generation,
            purpose: open.purpose,
            seek_cancellation: open.seek_cancellation,
            restartable_read_attempt: open.restartable_read_attempt,
            final_target: open.final_target,
            body: AdaptiveStreamingResourceBody::Network(Box::new(NetworkStreamingResourceBody {
                executor: open.executor,
                body: open.body,
            })),
            pending_cache_admission,
            body_completed: false,
            restartable_read_interrupted: false,
            resource_diagnostics: open.resource_diagnostics,
            network_request_attempt_id: Some(network_request_attempt_id),
        }
    }

    /// Создаёт network-free replay с теми же generation/cancellation checks.
    pub(crate) fn from_completed_replay(
        context: AdaptiveHttpContext,
        source_generation: SourceGeneration,
        purpose: FetchPurpose,
        seek_cancellation: DemuxSeekCancellationToken,
        replay: CompletedResourceReplay,
        resource_diagnostics: HttpResourceDiagnostics,
        restartable_read_attempt: AdaptiveRestartableReadAttemptBinding,
    ) -> Self {
        let (final_target, chunks, range_metadata) = replay.into_parts();
        Self {
            context,
            source_generation,
            purpose,
            seek_cancellation,
            restartable_read_attempt,
            final_target,
            body: AdaptiveStreamingResourceBody::CompletedReplay {
                chunks,
                next_chunk_index: 0,
                received_body_bytes: 0,
                range_metadata,
            },
            pending_cache_admission: None,
            body_completed: false,
            restartable_read_interrupted: false,
            resource_diagnostics,
            network_request_attempt_id: None,
        }
    }

    /// Возвращает effective post-redirect target без раскрытия в diagnostics.
    #[must_use]
    pub const fn final_target(&self) -> &HttpRequestTarget {
        &self.final_target
    }

    /// Читает следующий transport chunk либо validated HTTP EOF.
    ///
    /// Source-owned interruptible executor физически drop-ает pending
    /// `Response::chunk()` future при supersede или global shutdown.
    pub fn next_chunk(&mut self) -> Result<Option<Bytes>, AdaptiveTransportError> {
        if self.context.cancellation.is_cancelled() || self.seek_cancellation.is_cancelled() {
            self.pending_cache_admission = None;
            self.body.close();
            return Err(AdaptiveTransportError::Cancelled);
        }
        if self.restartable_read_interrupted {
            return Err(AdaptiveTransportError::RestartableReadInterrupted);
        }
        if self.body_completed {
            return Ok(None);
        }
        let source_cancellation = self.context.cancellation();
        let seek_cancellation = &self.seek_cancellation;
        let result = match &mut self.body {
            AdaptiveStreamingResourceBody::Network(network) => network.next_chunk(
                source_cancellation,
                seek_cancellation,
                &self.restartable_read_attempt,
            ),
            AdaptiveStreamingResourceBody::CompletedReplay {
                chunks,
                next_chunk_index,
                received_body_bytes,
                ..
            } => {
                AdaptiveStreamingReadResult::Source(match chunks.get(*next_chunk_index).cloned() {
                    Some(chunk) => {
                        *next_chunk_index = next_chunk_index.saturating_add(1);
                        *received_body_bytes = received_body_bytes.saturating_add(chunk.len());
                        Ok(Some(chunk))
                    }
                    None => Ok(None),
                })
            }
            AdaptiveStreamingResourceBody::Closed { .. } => {
                AdaptiveStreamingReadResult::Source(Ok(None))
            }
        };
        match result {
            AdaptiveStreamingReadResult::Source(Ok(Some(chunk))) => {
                let cache_admission_exceeded =
                    self.pending_cache_admission
                        .as_mut()
                        .is_some_and(|admission| {
                            matches!(
                                admission.record_chunk(&chunk),
                                PendingChunkRecordOutcome::BudgetExceeded
                            )
                        });
                if cache_admission_exceeded {
                    self.pending_cache_admission = None;
                }
                Ok(Some(chunk))
            }
            AdaptiveStreamingReadResult::Source(Ok(None)) => {
                self.body_completed = true;
                let body_bytes = self.body.received_body_bytes();
                let range_metadata = self.body.range_metadata().cloned();
                if let Some(admission) = self.pending_cache_admission.take() {
                    debug_assert_eq!(admission.body_bytes, body_bytes);
                    admission.commit(self.final_target.clone(), range_metadata);
                }
                self.body.close();
                Ok(None)
            }
            AdaptiveStreamingReadResult::Source(Err(SourceError::Cancelled)) => {
                self.pending_cache_admission = None;
                self.body.close();
                Err(AdaptiveTransportError::Cancelled)
            }
            AdaptiveStreamingReadResult::Source(Err(source_error)) => {
                self.pending_cache_admission = None;
                self.body.close();
                self.context.observe_endpoint_expiry(
                    self.source_generation,
                    self.purpose,
                    &source_error,
                );
                Err(AdaptiveTransportError::Source(source_error))
            }
            AdaptiveStreamingReadResult::RestartableReadInterrupted => {
                self.pending_cache_admission = None;
                self.restartable_read_interrupted = true;
                self.body.close();
                Err(AdaptiveTransportError::RestartableReadInterrupted)
            }
        }
    }

    /// Возвращает transport-accounted bytes без хранения полного body.
    #[must_use]
    pub const fn received_body_bytes(&self) -> usize {
        self.body.received_body_bytes()
    }

    /// Возвращает validated exact Range metadata.
    #[must_use]
    pub fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        self.body.range_metadata()
    }

    /// Возвращает typed logical resource id без locator/hash material.
    #[must_use]
    pub(crate) const fn resource_correlation_id(&self) -> HttpResourceCorrelationId {
        self.resource_diagnostics.correlation_id()
    }

    /// Network path имеет physical request id; cache replay принципиально не имеет его.
    #[must_use]
    pub(crate) const fn network_request_attempt_id(&self) -> Option<HttpRequestAttemptId> {
        self.network_request_attempt_id
    }
}

impl fmt::Debug for AdaptiveStreamingResource {
    /// Не форматирует locator, headers, cookies или query material.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveStreamingResource")
            .field("source_generation", &self.source_generation)
            .field("purpose", &self.purpose)
            .field("resource_id", &self.resource_correlation_id())
            .field("network_request_id", &self.network_request_attempt_id())
            .field("received_body_bytes", &self.received_body_bytes())
            .finish_non_exhaustive()
    }
}
