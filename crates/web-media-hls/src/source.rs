use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use demux_api::{
    DemuxInput, OrderedSegment, OrderedSegmentReadError, OrderedSegmentSequence,
    OrderedSegmentSource,
};
use media_core::DemuxSeekCancellationToken;
use source_core::{CancellationToken, SourceError};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_transport_api::SourceGeneration;
use zeroize::Zeroizing;

use crate::active_read::{HlsActiveReadError, HlsEpochActiveReadLifecycle};
use crate::plan::{
    HlsEpochPlan, HlsSegmentRestartCoordinate, PlannedEncryption, PlannedKeySource, PlannedResource,
};
use crate::{
    HlsEndpointRefreshReason, HlsRequiredContainer, SecretAes128Key, decrypt_aes128_cbc_pkcs7,
};

mod streaming;

use self::streaming::HlsResourceStreamState;

/// Resource class без locator-а или key bytes для live expiry policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsRefreshableResourceKind {
    MediaOrInitialization,
    EncryptionKey,
}

/// HLS-private observation boundary; generic demux/read event API не расширяется.
pub(crate) trait HlsResourceExpiryObserver: Send + Sync {
    fn observe_refreshable_expiry(
        &self,
        reason: HlsEndpointRefreshReason,
        resource_kind: HlsRefreshableResourceKind,
    );
}

/// Secret-free категория terminal body failure, при которой допустим один fresh restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum HlsTransientBodyFailureCategory {
    /// Body read превысил configured deadline уже после успешного HTTP open-а.
    Timeout = 1,
    /// Transport не смог продолжить чтение уже открытого response body.
    Read = 2,
    /// Validated body завершился раньше объявленной длины или exact range-а.
    UnexpectedEof = 3,
}

impl HlsTransientBodyFailureCategory {
    /// Классифицирует только body-stage transient failures; policy/status ошибки не повторяются.
    fn from_transport_error(error: &AdaptiveTransportError) -> Option<Self> {
        match error {
            AdaptiveTransportError::Source(SourceError::HttpTimeout { .. }) => Some(Self::Timeout),
            AdaptiveTransportError::Source(SourceError::HttpBodyRead { .. }) => Some(Self::Read),
            AdaptiveTransportError::Source(SourceError::UnexpectedEof { .. }) => {
                Some(Self::UnexpectedEof)
            }
            AdaptiveTransportError::Cancelled
            | AdaptiveTransportError::RestartableReadInterrupted
            | AdaptiveTransportError::Source(_)
            | AdaptiveTransportError::Target(_)
            | AdaptiveTransportError::Redirect(_)
            | AdaptiveTransportError::SecretScopeRejected
            | AdaptiveTransportError::ExplicitCookieHeader
            | AdaptiveTransportError::WorkerStopped
            | AdaptiveTransportError::StaleGeneration { .. }
            | AdaptiveTransportError::ResourceBoundExceeded { .. }
            | AdaptiveTransportError::InvalidResourcePolicy { .. } => None,
        }
    }
}

/// Typed snapshot attempt-local transport evidence без locator-а или payload-а.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HlsResourceAttemptFailure {
    /// Source не наблюдал разрешённую transient body category.
    #[default]
    None,
    /// Source завершился разрешённой transient body category.
    TransientBody(HlsTransientBodyFailureCategory),
}

/// Shared one-shot evidence между временным source и manifest candidate owner-ом.
#[derive(Clone, Debug, Default)]
pub(crate) struct SharedHlsResourceAttemptFailure {
    /// `0` означает отсутствие evidence; остальные значения соответствуют typed category.
    state: Arc<AtomicU8>,
}

impl SharedHlsResourceAttemptFailure {
    /// Запоминает первую terminal category; последующие ошибки не переписывают evidence.
    fn record(&self, category: HlsTransientBodyFailureCategory) {
        let _ = self
            .state
            .compare_exchange(0, category as u8, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Возвращает immutable snapshot для bounded retry decision-а.
    pub(crate) fn snapshot(&self) -> HlsResourceAttemptFailure {
        match self.state.load(Ordering::Acquire) {
            0 => HlsResourceAttemptFailure::None,
            value if value == HlsTransientBodyFailureCategory::Timeout as u8 => {
                HlsResourceAttemptFailure::TransientBody(HlsTransientBodyFailureCategory::Timeout)
            }
            value if value == HlsTransientBodyFailureCategory::Read as u8 => {
                HlsResourceAttemptFailure::TransientBody(HlsTransientBodyFailureCategory::Read)
            }
            value if value == HlsTransientBodyFailureCategory::UnexpectedEof as u8 => {
                HlsResourceAttemptFailure::TransientBody(
                    HlsTransientBodyFailureCategory::UnexpectedEof,
                )
            }
            value => unreachable!("недопустимая HLS resource attempt failure category: {value}"),
        }
    }
}

/// Named observation intent: обычные opens ничего не записывают, manifest attempt capture-ит evidence.
#[derive(Clone, Debug, Default)]
pub(crate) enum HlsResourceAttemptObserver {
    /// Initial/live/legacy opens не участвуют в manifest retry transaction.
    #[default]
    Disabled,
    /// Временный manifest attempt пишет terminal category в собственный shared state.
    Capture(SharedHlsResourceAttemptFailure),
}

impl HlsResourceAttemptObserver {
    /// Самодокументируемый intent обычного source open-а без retry observation.
    pub(crate) const fn disabled() -> Self {
        Self::Disabled
    }

    /// Привязывает observer к attempt-local state, который остаётся у manifest owner-а.
    pub(crate) fn capture(failure: SharedHlsResourceAttemptFailure) -> Self {
        Self::Capture(failure)
    }

    /// Записывает только разрешённые transient body categories.
    pub(crate) fn observe_transport_error(&self, error: &AdaptiveTransportError) {
        let Self::Capture(failure) = self else {
            return;
        };
        if let Some(category) = HlsTransientBodyFailureCategory::from_transport_error(error) {
            failure.record(category);
        }
    }
}

/// Lazy finite source одного epoch; network/key/decrypt выполняются на demux worker-е.
pub(crate) struct HlsEpochSegmentSource {
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    resources: std::vec::IntoIter<PlannedResource>,
    next_sequence: u64,
    next_byte_position: u64,
    maximum_key_resource_bytes: NonZeroUsize,
    cached_key: SharedHlsKeyCache,
    expiry_observer: Option<Arc<dyn HlsResourceExpiryObserver>>,
    media_spans: SharedHlsMediaSpanIndex,
    seek_cancellation: DemuxSeekCancellationToken,
    resource_attempt_observer: HlsResourceAttemptObserver,
    active_read: HlsSourceActiveReadLifecycle,
    stream_state: HlsResourceStreamState,
}

/// Live/fMP4 сохраняют прежний transport path; static TS opt-in-ится явно.
#[derive(Clone)]
enum HlsSourceActiveReadLifecycle {
    Disabled,
    Restartable(HlsEpochActiveReadLifecycle),
}

/// Exact plaintext range одного media resource-а в virtual ordered input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlsMediaResourceSpan {
    start: u64,
    end: u64,
    restart_segment: HlsSegmentRestartCoordinate,
}

/// Shared HLS-private provenance между lazy source и component demuxer-ом.
#[derive(Clone, Debug, Default)]
pub(crate) struct SharedHlsMediaSpanIndex {
    spans: Arc<Mutex<Vec<HlsMediaResourceSpan>>>,
}

impl SharedHlsMediaSpanIndex {
    /// Регистрирует exact plaintext interval уже fetched/decrypted media resource-а.
    fn observe_media_resource(
        &self,
        start: u64,
        end: u64,
        restart_segment: HlsSegmentRestartCoordinate,
    ) -> Result<(), HlsSegmentSourceError> {
        if start == end {
            return Ok(());
        }
        let mut spans = self
            .spans
            .lock()
            .map_err(|_| HlsSegmentSourceError::MediaSpanIndexPoisoned)?;
        if let Some(active) = spans
            .last_mut()
            .filter(|span| span.start == start && span.restart_segment == restart_segment)
        {
            active.end = active.end.max(end);
        } else {
            spans.push(HlsMediaResourceSpan {
                start,
                end,
                restart_segment,
            });
        }
        Ok(())
    }

    /// Разрешает packet source-position только по concrete registered plaintext span-у.
    pub(crate) fn restart_segment_for_byte_position(
        &self,
        byte_position: u64,
    ) -> Result<Option<HlsSegmentRestartCoordinate>, HlsSegmentSourceError> {
        let spans = self
            .spans
            .lock()
            .map_err(|_| HlsSegmentSourceError::MediaSpanIndexPoisoned)?;
        let insertion_index = spans.partition_point(|span| span.start <= byte_position);
        let Some(candidate_index) = insertion_index.checked_sub(1) else {
            return Ok(None);
        };
        Ok(spans
            .get(candidate_index)
            .filter(|span| byte_position < span.end)
            .map(|span| span.restart_segment))
    }
}

/// Current epoch-local key; identity не содержит URL/key bytes.
struct CachedKey {
    identity: u64,
    key: SecretAes128Key,
}

/// Snapshot-scoped key cache для segment-scoped live demux.
///
/// Новый accepted manifest snapshot получает новый cache, поэтому один лишь
/// совпавший URI не переносит старый key material через refresh.
#[derive(Clone, Default)]
pub(crate) struct SharedHlsKeyCache {
    cached: Arc<Mutex<Option<CachedKey>>>,
}

impl SharedHlsKeyCache {
    pub(crate) fn clear(&self) -> Result<(), HlsSegmentSourceError> {
        *self
            .cached
            .lock()
            .map_err(|_| HlsSegmentSourceError::KeyCachePoisoned)? = None;
        Ok(())
    }
}

impl HlsEpochSegmentSource {
    pub(crate) fn new(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        epoch: HlsEpochPlan,
        maximum_key_resource_bytes: NonZeroUsize,
    ) -> Self {
        Self::new_with_key_cache(
            http,
            generation,
            epoch,
            maximum_key_resource_bytes,
            SharedHlsKeyCache::default(),
        )
    }

    /// Создаёт static VOD source с exact packet-to-media provenance observer-ом.
    pub(crate) fn new_with_media_span_index(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        epoch: HlsEpochPlan,
        maximum_key_resource_bytes: NonZeroUsize,
        media_spans: SharedHlsMediaSpanIndex,
    ) -> Self {
        Self::new_with_all_observers(
            http,
            generation,
            epoch,
            maximum_key_resource_bytes,
            SharedHlsKeyCache::default(),
            None,
            media_spans,
        )
    }

    pub(crate) fn new_with_key_cache(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        epoch: HlsEpochPlan,
        maximum_key_resource_bytes: NonZeroUsize,
        cached_key: SharedHlsKeyCache,
    ) -> Self {
        Self::new_with_key_cache_and_observer(
            http,
            generation,
            epoch,
            maximum_key_resource_bytes,
            cached_key,
            None,
        )
    }

    pub(crate) fn new_with_key_cache_and_observer(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        epoch: HlsEpochPlan,
        maximum_key_resource_bytes: NonZeroUsize,
        cached_key: SharedHlsKeyCache,
        expiry_observer: Option<Arc<dyn HlsResourceExpiryObserver>>,
    ) -> Self {
        Self::new_with_all_observers(
            http,
            generation,
            epoch,
            maximum_key_resource_bytes,
            cached_key,
            expiry_observer,
            SharedHlsMediaSpanIndex::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_all_observers(
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        epoch: HlsEpochPlan,
        maximum_key_resource_bytes: NonZeroUsize,
        cached_key: SharedHlsKeyCache,
        expiry_observer: Option<Arc<dyn HlsResourceExpiryObserver>>,
        media_spans: SharedHlsMediaSpanIndex,
    ) -> Self {
        Self {
            http,
            generation,
            resources: epoch.resources.into_iter(),
            next_sequence: 0,
            next_byte_position: 0,
            maximum_key_resource_bytes,
            cached_key,
            expiry_observer,
            media_spans,
            seek_cancellation: DemuxSeekCancellationToken::new(),
            resource_attempt_observer: HlsResourceAttemptObserver::disabled(),
            active_read: HlsSourceActiveReadLifecycle::Disabled,
            stream_state: HlsResourceStreamState::Ready,
        }
    }

    /// Привязывает source к lifecycle конкретного worker-receipted seek-а.
    #[must_use]
    pub(crate) fn with_seek_cancellation(
        mut self,
        seek_cancellation: DemuxSeekCancellationToken,
    ) -> Self {
        self.seek_cancellation = seek_cancellation;
        self
    }

    /// Привязывает source к attempt-local typed body failure observer-у.
    #[must_use]
    pub(crate) fn with_resource_attempt_observer(
        mut self,
        resource_attempt_observer: HlsResourceAttemptObserver,
    ) -> Self {
        self.resource_attempt_observer = resource_attempt_observer;
        self
    }

    /// Привязывает static TS source к offside/committed active-read lifecycle-у.
    #[must_use]
    pub(crate) fn with_active_read_lifecycle(
        mut self,
        active_read: HlsEpochActiveReadLifecycle,
    ) -> Self {
        self.active_read = HlsSourceActiveReadLifecycle::Restartable(active_read);
        self
    }

    /// Выбирает streaming boundary только для container factory с доказанной поддержкой.
    #[must_use]
    pub(crate) fn into_demux_input(self, container: HlsRequiredContainer) -> DemuxInput {
        match container {
            HlsRequiredContainer::TransportStream => {
                DemuxInput::ordered_resource_stream(Box::new(self))
            }
            HlsRequiredContainer::FragmentedMp4 => DemuxInput::ordered_segments(Box::new(self)),
        }
    }

    fn observe_refreshable_expiry(
        &self,
        error: &AdaptiveTransportError,
        resource_kind: HlsRefreshableResourceKind,
    ) {
        let Some(reason) = error
            .http_status_code()
            .and_then(HlsEndpointRefreshReason::from_http_status)
        else {
            return;
        };
        if let Some(observer) = self.expiry_observer.as_ref() {
            observer.observe_refreshable_expiry(reason, resource_kind);
        }
    }

    fn fetch_resource(
        &self,
        resource: &PlannedResource,
    ) -> Result<Vec<u8>, AdaptiveTransportError> {
        let purpose = match resource.kind {
            demux_api::OrderedSegmentKind::Initialization => {
                AdaptiveResourcePurpose::Initialization
            }
            demux_api::OrderedSegmentKind::Media => AdaptiveResourcePurpose::MediaSegment,
        };
        let maximum_body_bytes = self.http.maximum_resource_bytes(purpose);
        let request = match resource.byte_range {
            Some(byte_range) => AdaptiveResourceFetchRequest::range(
                self.generation,
                resource.target.clone(),
                byte_range,
                maximum_body_bytes,
                purpose,
                AdaptiveResourceQueryApplication::MergeScopedAddition,
            ),
            None => AdaptiveResourceFetchRequest::full(
                self.generation,
                resource.target.clone(),
                maximum_body_bytes,
                purpose,
                AdaptiveResourceQueryApplication::MergeScopedAddition,
            ),
        }
        .with_secret_forwarding(self.http.resource_secret_forwarding_for(&resource.target));
        self.http
            .fetch_resource_blocking(request)
            .inspect_err(|error| {
                self.observe_refreshable_expiry(
                    error,
                    HlsRefreshableResourceKind::MediaOrInitialization,
                );
            })
            .map(web_media_adaptive::AdaptiveFetchedResource::into_bytes)
    }

    fn key_for(
        &mut self,
        encryption: &PlannedEncryption,
    ) -> Result<SecretAes128Key, HlsSegmentSourceError> {
        if let Some(cached) = self
            .cached_key
            .cached
            .lock()
            .map_err(|_| HlsSegmentSourceError::KeyCachePoisoned)?
            .as_ref()
            .filter(|cached| cached.identity == encryption.key_identity)
        {
            return Ok(cached.key.clone());
        }
        let fetched_key = match &encryption.key {
            PlannedKeySource::Inline(key) => key.clone(),
            PlannedKeySource::ManifestTarget(target) => self.fetch_key_target(
                target,
                AdaptiveResourceQueryApplication::MergeScopedAddition,
            )?,
            PlannedKeySource::ExtractorReplacement(target) => {
                self.fetch_key_target(target, AdaptiveResourceQueryApplication::BypassScopedQuery)?
            }
        };
        *self
            .cached_key
            .cached
            .lock()
            .map_err(|_| HlsSegmentSourceError::KeyCachePoisoned)? = Some(CachedKey {
            identity: encryption.key_identity,
            key: fetched_key.clone(),
        });
        Ok(fetched_key)
    }

    fn fetch_key_target(
        &self,
        target: &source_core::HttpRequestTarget,
        query_application: AdaptiveResourceQueryApplication,
    ) -> Result<SecretAes128Key, HlsSegmentSourceError> {
        let fetched = self
            .http
            .fetch_resource_blocking(
                AdaptiveResourceFetchRequest::full(
                    self.generation,
                    target.clone(),
                    self.maximum_key_resource_bytes,
                    AdaptiveResourcePurpose::EncryptionKey,
                    query_application,
                )
                .with_secret_forwarding(self.http.resource_secret_forwarding_for(target)),
            )
            .inspect_err(|error| {
                self.observe_refreshable_expiry(error, HlsRefreshableResourceKind::EncryptionKey);
            })?;
        let key_bytes = Zeroizing::new(fetched.into_bytes());
        Ok(SecretAes128Key::from_key_file_bytes(&key_bytes)?)
    }

    fn decrypt(
        &mut self,
        ciphertext: &[u8],
        encryption: &PlannedEncryption,
    ) -> Result<Bytes, HlsSegmentSourceError> {
        let key = self.key_for(encryption)?;
        let plaintext = decrypt_aes128_cbc_pkcs7(ciphertext, &key, encryption.iv)?;
        Ok(Bytes::copy_from_slice(plaintext.expose_for_demux()))
    }
}

impl OrderedSegmentSource for HlsEpochSegmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() || self.http.cancellation().is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        let Some(resource) = self.resources.next() else {
            return Ok(None);
        };
        if resource.encryption.is_none() {
            self.cached_key.clear().map_err(map_runtime_source_error)?;
        }
        let fetched = self
            .fetch_resource(&resource)
            .map_err(map_runtime_source_error)?;
        let bytes = match &resource.encryption {
            Some(encryption) => self
                .decrypt(&fetched, encryption)
                .map_err(map_runtime_source_error)?,
            None => Bytes::from(fetched),
        };
        let resource_start = self.next_byte_position;
        let resource_end = resource_start
            .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                map_runtime_source_error(HlsSegmentSourceError::BytePositionOverflow)
            })?)
            .ok_or_else(|| map_runtime_source_error(HlsSegmentSourceError::BytePositionOverflow))?;
        if let Some(restart_segment) = resource.restart_segment {
            self.media_spans
                .observe_media_resource(resource_start, resource_end, restart_segment)
                .map_err(map_runtime_source_error)?;
        }
        self.next_byte_position = resource_end;
        let sequence = OrderedSegmentSequence::new(self.next_sequence);
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(Some(OrderedSegment {
            sequence,
            kind: resource.kind,
            discontinuity: resource.discontinuity,
            bytes,
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HlsSegmentSourceError {
    #[error("transport")]
    Transport(#[from] AdaptiveTransportError),
    #[error("key")]
    Key(#[from] crate::HlsKeyStateError),
    #[error("decrypt")]
    Decrypt(#[from] crate::Aes128CbcDecryptError),
    #[error("key cache poisoned")]
    KeyCachePoisoned,
    #[error("media span index poisoned")]
    MediaSpanIndexPoisoned,
    #[error("ordered input byte position overflow")]
    BytePositionOverflow,
    #[error("active read lifecycle")]
    ActiveRead(#[from] HlsActiveReadError),
}

fn map_runtime_source_error(error: impl Into<HlsSegmentSourceError>) -> OrderedSegmentReadError {
    match error.into() {
        HlsSegmentSourceError::Transport(
            AdaptiveTransportError::Cancelled
            | AdaptiveTransportError::Source(SourceError::Cancelled),
        ) => OrderedSegmentReadError::Cancelled,
        HlsSegmentSourceError::Transport(_) => OrderedSegmentReadError::Failed {
            reason: "hls-resource-fetch".to_owned(),
        },
        HlsSegmentSourceError::Key(_) => OrderedSegmentReadError::Failed {
            reason: "hls-invalid-aes-key".to_owned(),
        },
        HlsSegmentSourceError::Decrypt(_) => OrderedSegmentReadError::Failed {
            reason: "hls-invalid-aes-ciphertext".to_owned(),
        },
        HlsSegmentSourceError::KeyCachePoisoned => OrderedSegmentReadError::Failed {
            reason: "hls-key-cache-poisoned".to_owned(),
        },
        HlsSegmentSourceError::MediaSpanIndexPoisoned => OrderedSegmentReadError::Failed {
            reason: "hls-media-span-index-poisoned".to_owned(),
        },
        HlsSegmentSourceError::BytePositionOverflow => OrderedSegmentReadError::Failed {
            reason: "hls-byte-position-overflow".to_owned(),
        },
        HlsSegmentSourceError::ActiveRead(_) => OrderedSegmentReadError::Failed {
            reason: "hls-active-read-lifecycle".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::SharedHlsMediaSpanIndex;
    use crate::plan::HlsSegmentRestartCoordinate;

    #[test]
    fn media_span_lookup_uses_half_open_exact_boundaries() {
        let spans = SharedHlsMediaSpanIndex::default();
        let first = HlsSegmentRestartCoordinate { segment_index: 0 };
        let second = HlsSegmentRestartCoordinate { segment_index: 1 };
        spans
            .observe_media_resource(4, 10, first)
            .expect("record first media span");
        spans
            .observe_media_resource(10, 20, second)
            .expect("record second media span");

        assert_eq!(
            spans
                .restart_segment_for_byte_position(9)
                .expect("lookup first span"),
            Some(first)
        );
        assert_eq!(
            spans
                .restart_segment_for_byte_position(10)
                .expect("lookup second span"),
            Some(second)
        );
        assert_eq!(
            spans
                .restart_segment_for_byte_position(20)
                .expect("lookup exclusive end"),
            None
        );
        assert_eq!(
            spans
                .restart_segment_for_byte_position(3)
                .expect("lookup initialization bytes"),
            None
        );
    }
}
