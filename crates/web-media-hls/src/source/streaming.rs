//! Pull-based lifecycle одного ordered HLS resource-а.

use std::num::NonZeroUsize;

use bytes::Bytes;
use demux_api::{
    OrderedResourceMetadata, OrderedResourceReadError, OrderedResourceReadOutcome,
    OrderedResourceStreamSource, OrderedSegmentSequence,
};
use source_core::{CancellationToken, SourceError};
use web_media_adaptive::{
    AdaptiveResourceFetchRequest, AdaptiveResourcePurpose, AdaptiveResourceQueryApplication,
    AdaptiveStreamingResource, AdaptiveTransportError,
};

use super::{
    HlsEpochSegmentSource, HlsRefreshableResourceKind, HlsSegmentSourceError,
    HlsSourceActiveReadLifecycle,
};
use crate::plan::PlannedResource;

/// Состояние ровно одного pull-based resource lifecycle-а.
///
/// Network cursor намеренно остаётся inline: дополнительный `Box` добавил бы allocation на каждый
/// resource и изменил бы latency/memory behavior уже работающего streaming path-а ради формы enum-а.
#[allow(
    clippy::large_enum_variant,
    reason = "single active resource cursor stays allocation-free; variants are never collected"
)]
pub(super) enum HlsResourceStreamState {
    /// Следующий вызов публикует `Begin` либо terminal `EndOfInput`.
    Ready,
    /// Metadata уже опубликована; HTTP open/decrypt начнётся только по body demand.
    Opening {
        /// Immutable resource plan текущего lifecycle-а.
        resource: PlannedResource,
        /// Exact virtual plaintext offset первого byte ресурса.
        resource_start: u64,
    },
    /// Не зашифрованный response читается из единственного network cursor-а.
    Streaming {
        /// Immutable resource plan нужен для provenance и EOF semantics.
        resource: PlannedResource,
        /// Exact virtual plaintext offset первого byte ресурса.
        resource_start: u64,
        /// Открытый bounded response и current-thread executor.
        body: AdaptiveStreamingResource,
        /// Остаток одного wire chunk-а, ещё не отданный demux caller-у.
        remainder: Bytes,
    },
    /// AES-CBC требует полного ciphertext; plaintext всё равно отдаётся bounded chunks.
    Buffered {
        /// Immutable resource plan нужен для provenance и EOF semantics.
        resource: PlannedResource,
        /// Exact virtual plaintext offset первого byte ресурса.
        resource_start: u64,
        /// Неразданный bounded plaintext без дополнительного копирования.
        remainder: Bytes,
    },
    /// Terminal state делает повторный `EndOfInput` идемпотентным.
    Finished,
}

impl HlsEpochSegmentSource {
    /// Полностью дочитывает bounded response через cancellable streaming transport.
    /// Redirect/status/range/max-body/secret policy уже зафиксированы в typed request-е.
    pub(super) fn fetch_cancellable_full_resource(
        &self,
        request: AdaptiveResourceFetchRequest,
        refreshable_kind: HlsRefreshableResourceKind,
        register_active_attempt: bool,
    ) -> Result<Vec<u8>, HlsSegmentSourceError> {
        let opened = if register_active_attempt {
            match &self.active_read {
                HlsSourceActiveReadLifecycle::Disabled => self
                    .http
                    .open_resource_streaming_blocking(request, self.seek_cancellation.clone())
                    .map_err(HlsSegmentSourceError::from),
                HlsSourceActiveReadLifecycle::Restartable(lifecycle) => {
                    let attempt = lifecycle.new_resource_attempt()?;
                    let body = self
                        .http
                        .open_resource_streaming_blocking_with_restartable_read_attempt(
                            request,
                            self.seek_cancellation.clone(),
                            attempt.clone(),
                        )
                        .map_err(HlsSegmentSourceError::from)?;
                    lifecycle.register_opened_attempt(attempt)?;
                    Ok(body)
                }
            }
        } else {
            self.http
                .open_resource_streaming_blocking(request, self.seek_cancellation.clone())
                .map_err(HlsSegmentSourceError::from)
        };
        let mut body = opened.inspect_err(|error| {
            if let HlsSegmentSourceError::Transport(error) = error {
                self.observe_refreshable_expiry(error, refreshable_kind);
            }
        })?;
        let mut resource_bytes = Vec::new();
        loop {
            let next_chunk = body.next_chunk();
            if let Err(error) = &next_chunk {
                self.resource_attempt_observer
                    .observe_transport_error(error);
                self.observe_refreshable_expiry(error, refreshable_kind);
            }
            match next_chunk? {
                Some(chunk) => resource_bytes.extend_from_slice(&chunk),
                None => return Ok(resource_bytes),
            }
        }
    }

    /// Открывает один unencrypted resource до HTTP EOF, сохраняя общую policy.
    fn open_streaming_resource(
        &self,
        resource: &PlannedResource,
    ) -> Result<AdaptiveStreamingResource, HlsSegmentSourceError> {
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
        let opened = match &self.active_read {
            HlsSourceActiveReadLifecycle::Disabled => self
                .http
                .open_resource_streaming_blocking(request, self.seek_cancellation.clone())
                .map_err(HlsSegmentSourceError::from),
            HlsSourceActiveReadLifecycle::Restartable(lifecycle) => {
                let attempt = lifecycle.new_resource_attempt()?;
                let body = self
                    .http
                    .open_resource_streaming_blocking_with_restartable_read_attempt(
                        request,
                        self.seek_cancellation.clone(),
                        attempt.clone(),
                    )
                    .map_err(HlsSegmentSourceError::from)?;
                lifecycle.register_opened_attempt(attempt)?;
                Ok(body)
            }
        };
        opened.inspect_err(|error| {
            if let HlsSegmentSourceError::Transport(error) = error {
                self.observe_refreshable_expiry(
                    error,
                    HlsRefreshableResourceKind::MediaOrInitialization,
                );
            }
        })
    }

    /// Учитывает только фактически переданный demux-у plaintext chunk.
    fn observe_streamed_plaintext(
        &mut self,
        resource: &PlannedResource,
        resource_start: u64,
        chunk_bytes: usize,
    ) -> Result<(), HlsSegmentSourceError> {
        let chunk_bytes =
            u64::try_from(chunk_bytes).map_err(|_| HlsSegmentSourceError::BytePositionOverflow)?;
        self.next_byte_position = self
            .next_byte_position
            .checked_add(chunk_bytes)
            .ok_or(HlsSegmentSourceError::BytePositionOverflow)?;
        if let Some(manifest_segment) = resource.manifest_segment {
            self.media_spans.observe_media_resource(
                resource_start,
                self.next_byte_position,
                manifest_segment,
            )?;
        }
        Ok(())
    }

    /// Отделяет caller-bounded prefix одного immutable chunk-а без копирования.
    fn take_bounded_chunk(remainder: &mut Bytes, maximum_chunk_bytes: NonZeroUsize) -> Bytes {
        if remainder.len() > maximum_chunk_bytes.get() {
            remainder.split_to(maximum_chunk_bytes.get())
        } else {
            std::mem::take(remainder)
        }
    }
}

impl OrderedResourceStreamSource for HlsEpochSegmentSource {
    fn next_event(
        &mut self,
        maximum_chunk_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<OrderedResourceReadOutcome, OrderedResourceReadError> {
        if cancellation.is_cancelled()
            || self.http.cancellation().is_cancelled()
            || self.seek_cancellation.is_cancelled()
        {
            return Err(OrderedResourceReadError::Cancelled);
        }
        loop {
            let state = std::mem::replace(&mut self.stream_state, HlsResourceStreamState::Finished);
            match state {
                HlsResourceStreamState::Ready => {
                    let Some(resource) = self.resources.next() else {
                        self.stream_state = HlsResourceStreamState::Finished;
                        return Ok(OrderedResourceReadOutcome::EndOfInput);
                    };
                    if resource.encryption.is_none() {
                        self.cached_key.clear().map_err(map_resource_stream_error)?;
                    }
                    if let (
                        HlsSourceActiveReadLifecycle::Restartable(lifecycle),
                        Some(restart_segment),
                    ) = (&self.active_read, resource.restart_segment)
                    {
                        lifecycle
                            .observe_media_restart(restart_segment)
                            .map_err(HlsSegmentSourceError::from)
                            .map_err(map_resource_stream_error)?;
                    }
                    let sequence = OrderedSegmentSequence::new(self.next_sequence);
                    self.next_sequence = self.next_sequence.saturating_add(1);
                    let metadata = OrderedResourceMetadata {
                        sequence,
                        kind: resource.kind,
                        discontinuity: resource.discontinuity,
                    };
                    self.stream_state = HlsResourceStreamState::Opening {
                        resource,
                        resource_start: self.next_byte_position,
                    };
                    return Ok(OrderedResourceReadOutcome::Begin(metadata));
                }
                HlsResourceStreamState::Opening {
                    resource,
                    resource_start,
                } => {
                    if let Some(encryption) = resource.encryption.as_ref() {
                        let ciphertext = self
                            .fetch_resource(&resource)
                            .map_err(map_resource_stream_error)?;
                        let plaintext = self
                            .decrypt(&ciphertext, encryption)
                            .map_err(map_resource_stream_error)?;
                        self.stream_state = HlsResourceStreamState::Buffered {
                            resource,
                            resource_start,
                            remainder: plaintext,
                        };
                    } else {
                        let body = self
                            .open_streaming_resource(&resource)
                            .map_err(map_resource_stream_error)?;
                        self.stream_state = HlsResourceStreamState::Streaming {
                            resource,
                            resource_start,
                            body,
                            remainder: Bytes::new(),
                        };
                    }
                }
                HlsResourceStreamState::Streaming {
                    resource,
                    resource_start,
                    mut body,
                    mut remainder,
                } => {
                    if remainder.is_empty() {
                        let next_chunk = body.next_chunk();
                        if let Err(error) = &next_chunk {
                            self.resource_attempt_observer
                                .observe_transport_error(error);
                        }
                        match next_chunk.map_err(map_resource_stream_error)? {
                            Some(chunk) if !chunk.is_empty() => remainder = chunk,
                            Some(_) => {
                                self.stream_state = HlsResourceStreamState::Streaming {
                                    resource,
                                    resource_start,
                                    body,
                                    remainder,
                                };
                                continue;
                            }
                            None => {
                                self.stream_state = HlsResourceStreamState::Ready;
                                return Ok(OrderedResourceReadOutcome::EndResource);
                            }
                        }
                    }
                    let chunk = Self::take_bounded_chunk(&mut remainder, maximum_chunk_bytes);
                    self.observe_streamed_plaintext(&resource, resource_start, chunk.len())
                        .map_err(map_resource_stream_error)?;
                    self.stream_state = HlsResourceStreamState::Streaming {
                        resource,
                        resource_start,
                        body,
                        remainder,
                    };
                    return Ok(OrderedResourceReadOutcome::Data(chunk));
                }
                HlsResourceStreamState::Buffered {
                    resource,
                    resource_start,
                    mut remainder,
                } => {
                    if remainder.is_empty() {
                        self.stream_state = HlsResourceStreamState::Ready;
                        return Ok(OrderedResourceReadOutcome::EndResource);
                    }
                    let chunk = Self::take_bounded_chunk(&mut remainder, maximum_chunk_bytes);
                    self.observe_streamed_plaintext(&resource, resource_start, chunk.len())
                        .map_err(map_resource_stream_error)?;
                    self.stream_state = HlsResourceStreamState::Buffered {
                        resource,
                        resource_start,
                        remainder,
                    };
                    return Ok(OrderedResourceReadOutcome::Data(chunk));
                }
                HlsResourceStreamState::Finished => {
                    self.stream_state = HlsResourceStreamState::Finished;
                    return Ok(OrderedResourceReadOutcome::EndOfInput);
                }
            }
        }
    }
}

/// Переводит HLS-private failure в neutral streaming boundary без ложного EOF.
fn map_resource_stream_error(error: impl Into<HlsSegmentSourceError>) -> OrderedResourceReadError {
    match error.into() {
        HlsSegmentSourceError::Transport(
            AdaptiveTransportError::Cancelled
            | AdaptiveTransportError::Source(SourceError::Cancelled),
        ) => OrderedResourceReadError::Cancelled,
        HlsSegmentSourceError::Transport(AdaptiveTransportError::RestartableReadInterrupted) => {
            OrderedResourceReadError::RestartableReadInterrupted
        }
        HlsSegmentSourceError::Transport(_) => OrderedResourceReadError::Failed {
            reason: "hls-resource-fetch".to_owned(),
        },
        HlsSegmentSourceError::Key(_) => OrderedResourceReadError::Failed {
            reason: "hls-invalid-aes-key".to_owned(),
        },
        HlsSegmentSourceError::Decrypt(_) => OrderedResourceReadError::Failed {
            reason: "hls-invalid-aes-ciphertext".to_owned(),
        },
        HlsSegmentSourceError::KeyCachePoisoned => OrderedResourceReadError::Failed {
            reason: "hls-key-cache-poisoned".to_owned(),
        },
        HlsSegmentSourceError::MediaSpanIndexPoisoned => OrderedResourceReadError::Failed {
            reason: "hls-media-span-index-poisoned".to_owned(),
        },
        HlsSegmentSourceError::BytePositionOverflow => OrderedResourceReadError::Failed {
            reason: "hls-byte-position-overflow".to_owned(),
        },
        HlsSegmentSourceError::ActiveRead(_) => OrderedResourceReadError::Failed {
            reason: "hls-active-read-lifecycle".to_owned(),
        },
    }
}
