//! Seekable bounded Range source поверх единой S31 HTTP policy.

use std::fmt;
use std::num::NonZeroUsize;

use source_core::{
    ByteSource, CancellationToken, HttpRepresentationChange, HttpRequestPolicyFailure,
    HttpRequestTarget, Seekability, SourceError, SourceFingerprint, SourceResult, SourceValidators,
};
use thiserror::Error;
use web_media_transport_api::SourceGeneration;

use crate::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};

/// Explicit policy одного seekable adaptive representation source-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveRangeSourceConfig {
    /// Максимальный размер одного wire Range read-а.
    maximum_read_bytes: NonZeroUsize,
    /// Способ применения provider query material.
    query_application: AdaptiveResourceQueryApplication,
}

impl AdaptiveRangeSourceConfig {
    /// Создаёт policy без скрытого page-size default-а.
    #[must_use]
    pub const fn new(
        maximum_read_bytes: NonZeroUsize,
        query_application: AdaptiveResourceQueryApplication,
    ) -> Self {
        Self {
            maximum_read_bytes,
            query_application,
        }
    }
}

/// Ошибка initial Range proof до публикации seekable source-а.
#[derive(Debug, Error)]
pub enum AdaptiveRangeSourceOpenError {
    /// S31 transport/policy отказали при probe.
    #[error("adaptive Range source probe failed: {0}")]
    Transport(#[from] AdaptiveTransportError),
    /// Внутренний wire boundary не вернул metadata exact Range response-а.
    #[error("adaptive Range source probe returned no Range metadata")]
    MissingRangeMetadata,
    /// `Content-Range` не содержит обязательную полную длину representation.
    #[error("adaptive Range source requires an exact total resource length")]
    MissingTotalLength,
    /// Seekable representation не может быть пустой.
    #[error("adaptive Range source rejected an empty representation")]
    EmptyRepresentation,
}

/// Seekable `ByteSource`, который не создаёт второй HTTP client и не читает full resource.
pub struct AdaptiveRangeByteSource {
    /// Immutable S31 session/secret/redirect/retry/generation policy.
    context: AdaptiveHttpContext,
    /// Исходный target; каждый read заново проходит тот же manual redirect policy.
    target: HttpRequestTarget,
    /// Exact source generation всех Range requests.
    generation: SourceGeneration,
    /// Caller-owned bound одного wire read-а.
    maximum_read_bytes: NonZeroUsize,
    /// Provider-specific query application semantics.
    query_application: AdaptiveResourceQueryApplication,
    /// Доказанная полная длина representation.
    total_resource_bytes: u64,
    /// Exact validators initial probe-а, включая доказанное отсутствие.
    validators: SourceValidators,
    /// Текущий логический byte cursor.
    position: u64,
    /// Opaque cache/source identity без raw target-а.
    fingerprint: SourceFingerprint,
}

impl fmt::Debug for AdaptiveRangeByteSource {
    /// Не раскрывает target, validators либо scoped request material.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveRangeByteSource")
            .field("generation", &self.generation)
            .field("maximum_read_bytes", &self.maximum_read_bytes)
            .field("total_resource_bytes", &self.total_resource_bytes)
            .field("position", &self.position)
            .finish()
    }
}

impl AdaptiveRangeByteSource {
    /// Делает однобайтовый `206` probe и публикует source только после exact total proof.
    pub fn open(
        context: AdaptiveHttpContext,
        target: HttpRequestTarget,
        generation: SourceGeneration,
        config: AdaptiveRangeSourceConfig,
    ) -> Result<Self, AdaptiveRangeSourceOpenError> {
        let probe = context.fetch_resource_blocking(AdaptiveResourceFetchRequest::range(
            generation,
            target.clone(),
            source_core::HttpBoundedByteRange::new(0, NonZeroUsize::MIN)
                .expect("single-byte Range cannot overflow"),
            NonZeroUsize::MIN,
            AdaptiveResourcePurpose::MediaSegment,
            config.query_application,
        ))?;
        let metadata = probe
            .range_metadata()
            .ok_or(AdaptiveRangeSourceOpenError::MissingRangeMetadata)?;
        let total_resource_bytes = metadata
            .total_resource_bytes()
            .ok_or(AdaptiveRangeSourceOpenError::MissingTotalLength)?;
        if total_resource_bytes == 0 {
            return Err(AdaptiveRangeSourceOpenError::EmptyRepresentation);
        }
        let validators = metadata.validators();
        let fingerprint = SourceFingerprint::new(format!(
            "adaptive-range:{:016x}:{}:{total_resource_bytes}",
            target.stable_identity_hash(),
            generation.value(),
        ));

        Ok(Self {
            context,
            target,
            generation,
            maximum_read_bytes: config.maximum_read_bytes,
            query_application: config.query_application,
            total_resource_bytes,
            validators,
            position: 0,
            fingerprint,
        })
    }

    /// Загружает один exact bounded range и проверяет static representation identity.
    fn fetch_range(&self, offset: u64, length: NonZeroUsize) -> SourceResult<Vec<u8>> {
        let byte_range = source_core::HttpBoundedByteRange::new(offset, length)?;
        let fetched = self
            .context
            .fetch_resource_blocking(AdaptiveResourceFetchRequest::range(
                self.generation,
                self.target.clone(),
                byte_range,
                length,
                AdaptiveResourcePurpose::MediaSegment,
                self.query_application,
            ))
            .map_err(map_adaptive_read_error)?;
        let metadata = fetched
            .range_metadata()
            .ok_or(SourceError::HttpRequestPolicyRejected {
                reason: HttpRequestPolicyFailure::WorkerStopped,
            })?;
        if metadata.total_resource_bytes() != Some(self.total_resource_bytes) {
            return Err(SourceError::HttpRepresentationChanged {
                reason: HttpRepresentationChange::TotalLength,
            });
        }
        if metadata.validators() != self.validators {
            return Err(SourceError::HttpRepresentationChanged {
                reason: HttpRepresentationChange::Validators,
            });
        }
        Ok(fetched.into_bytes())
    }
}

impl ByteSource for AdaptiveRangeByteSource {
    /// Читает не больше caller page bound-а и никогда не выполняет full GET.
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if cancellation.is_cancelled() || self.context.cancellation().is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let remaining_bytes = self.total_resource_bytes.saturating_sub(self.position);
        if remaining_bytes == 0 {
            return Ok(0);
        }
        let bounded_length = output
            .len()
            .min(self.maximum_read_bytes.get())
            .min(usize::try_from(remaining_bytes).unwrap_or(usize::MAX));
        let length = NonZeroUsize::new(bounded_length).ok_or(SourceError::UnexpectedEof {
            offset: self.position,
            expected_bytes: 1,
            actual_bytes: 0,
        })?;
        let bytes = self.fetch_range(self.position, length)?;
        output[..bytes.len()].copy_from_slice(&bytes);
        self.position = self
            .position
            .checked_add(
                u64::try_from(bytes.len()).map_err(|_| SourceError::InvalidConfig {
                    field: "adaptive_range_source.read_length",
                    message: "read length does not fit u64".to_owned(),
                })?,
            )
            .ok_or_else(|| SourceError::InvalidConfig {
                field: "adaptive_range_source.position",
                message: "source position overflows u64".to_owned(),
            })?;
        Ok(bytes.len())
    }

    /// Переставляет логический cursor; EOF обрабатывается последующим `read`.
    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.position = offset;
        Ok(())
    }

    /// Возвращает текущий логический cursor.
    fn position(&self) -> u64 {
        self.position
    }

    /// Initial `206` probe доказал seekability до публикации source-а.
    fn seekability(&self) -> Seekability {
        Seekability::Seekable
    }

    /// Возвращает exact validators initial representation probe-а.
    fn validators(&self) -> SourceValidators {
        self.validators.clone()
    }

    /// Возвращает обязательную доказанную полную длину representation.
    fn content_length(&self) -> Option<u64> {
        Some(self.total_resource_bytes)
    }

    /// Возвращает opaque identity без locator/header material.
    fn fingerprint(&self) -> SourceFingerprint {
        self.fingerprint.clone()
    }
}

/// Переводит S31 policy failure в neutral `ByteSource` error без raw material.
fn map_adaptive_read_error(error: AdaptiveTransportError) -> SourceError {
    match error {
        AdaptiveTransportError::Cancelled => SourceError::Cancelled,
        AdaptiveTransportError::Source(source) => source,
        AdaptiveTransportError::Target(_) => SourceError::HttpRequestPolicyRejected {
            reason: HttpRequestPolicyFailure::TargetResolution,
        },
        AdaptiveTransportError::Redirect(_) => SourceError::HttpRequestPolicyRejected {
            reason: HttpRequestPolicyFailure::RedirectRejected,
        },
        AdaptiveTransportError::SecretScopeRejected => SourceError::HttpRequestPolicyRejected {
            reason: HttpRequestPolicyFailure::SecretScopeRejected,
        },
        AdaptiveTransportError::ExplicitCookieHeader => SourceError::HttpRequestPolicyRejected {
            reason: HttpRequestPolicyFailure::ExplicitCookieHeader,
        },
        AdaptiveTransportError::WorkerStopped => SourceError::HttpRequestPolicyRejected {
            reason: HttpRequestPolicyFailure::WorkerStopped,
        },
        AdaptiveTransportError::StaleGeneration { .. } => SourceError::HttpRequestPolicyRejected {
            reason: HttpRequestPolicyFailure::StaleGeneration,
        },
        AdaptiveTransportError::ResourceBoundExceeded { .. } => {
            SourceError::HttpRequestPolicyRejected {
                reason: HttpRequestPolicyFailure::ResourceBoundExceeded,
            }
        }
    }
}
