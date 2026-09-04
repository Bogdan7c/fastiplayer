//! Seekable bounded Range source поверх единой S31 HTTP policy.

use std::collections::VecDeque;
use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};

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
    /// Максимальный размер одного wire Range read-а и bounded read-ahead страницы.
    maximum_read_bytes: NonZeroUsize,
    /// Маленькая первая страница после open либо далёкого seek для минимальной latency.
    latency_first_read_bytes: NonZeroUsize,
    /// Число одновременно удерживаемых страниц, включая init/index и media window.
    maximum_cached_pages: NonZeroUsize,
    /// Способ применения provider query material.
    query_application: AdaptiveResourceQueryApplication,
    /// Optional consumer-visible prefix; physical response identity всё равно проверяется целиком.
    exposed_content_length: Option<NonZeroU64>,
}

/// Одна immutable Range-страница устраняет HTTP round-trip на каждый мелкий container packet.
struct AdaptiveRangeReadCache {
    /// Абсолютное начало страницы в representation.
    start: u64,
    /// Доказанные bytes из одного bounded `206 Partial Content` ответа.
    bytes: Vec<u8>,
}

impl AdaptiveRangeReadCache {
    /// Проверяет half-open принадлежность позиции cached странице.
    fn contains(&self, position: u64) -> bool {
        let Some(relative_position) = position.checked_sub(self.start) else {
            return false;
        };
        relative_position < u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    /// Копирует доступный suffix страницы, только если логический cursor попадает внутрь неё.
    fn copy_from(&self, position: u64, output: &mut [u8]) -> Option<usize> {
        let relative_position = position.checked_sub(self.start)?;
        let relative_position = usize::try_from(relative_position).ok()?;
        let cached_suffix = self.bytes.get(relative_position..)?;
        if cached_suffix.is_empty() {
            return None;
        }
        let copied_bytes = output.len().min(cached_suffix.len());
        output[..copied_bytes].copy_from_slice(&cached_suffix[..copied_bytes]);
        Some(copied_bytes)
    }
}

impl AdaptiveRangeSourceConfig {
    /// Создаёт policy без скрытых page-size defaults.
    pub fn new(
        maximum_read_bytes: NonZeroUsize,
        latency_first_read_bytes: NonZeroUsize,
        maximum_cached_pages: NonZeroUsize,
        query_application: AdaptiveResourceQueryApplication,
    ) -> SourceResult<Self> {
        if latency_first_read_bytes > maximum_read_bytes {
            return Err(SourceError::InvalidConfig {
                field: "adaptive_range_source.latency_first_read_bytes",
                message: "latency-first page exceeds maximum Range read".to_owned(),
            });
        }
        Ok(Self {
            maximum_read_bytes,
            latency_first_read_bytes,
            maximum_cached_pages,
            query_application,
            exposed_content_length: None,
        })
    }

    /// Ограничивает логическую длину source доказанным prefix без изменения wire identity.
    pub fn with_exposed_content_length(mut self, exposed_content_length: NonZeroU64) -> Self {
        self.exposed_content_length = Some(exposed_content_length);
        self
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
    /// Manifest-declared prefix не помещается в доказанную physical representation.
    #[error("adaptive Range exposed content length exceeds the physical representation")]
    ExposedContentLengthExceedsResource,
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
    /// Маленький bound первого read-а после cursor discontinuity.
    latency_first_read_bytes: NonZeroUsize,
    /// Жёсткий bound одновременно удерживаемых Range-страниц.
    maximum_cached_pages: NonZeroUsize,
    /// Provider-specific query application semantics.
    query_application: AdaptiveResourceQueryApplication,
    /// Доказанная полная длина representation.
    total_resource_bytes: u64,
    /// Consumer-visible длина; для обычного playback равна physical длине.
    exposed_content_bytes: u64,
    /// Exact validators initial probe-а, включая доказанное отсутствие.
    validators: SourceValidators,
    /// Текущий логический byte cursor.
    position: u64,
    /// Маленький FIFO страниц позволяет одновременно держать init/index и текущий media window.
    read_cache: VecDeque<AdaptiveRangeReadCache>,
    /// Следующий cache miss должен предпочесть latency, а не throughput.
    latency_first_read_pending: bool,
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
            .field("exposed_content_bytes", &self.exposed_content_bytes)
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
        let exposed_content_bytes = config
            .exposed_content_length
            .map(NonZeroU64::get)
            .unwrap_or(total_resource_bytes);
        if exposed_content_bytes > total_resource_bytes {
            return Err(AdaptiveRangeSourceOpenError::ExposedContentLengthExceedsResource);
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
            latency_first_read_bytes: config.latency_first_read_bytes,
            maximum_cached_pages: config.maximum_cached_pages,
            query_application: config.query_application,
            total_resource_bytes,
            exposed_content_bytes,
            validators,
            position: 0,
            read_cache: VecDeque::with_capacity(config.maximum_cached_pages.get()),
            latency_first_read_pending: true,
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

    /// Заполняет одну bounded страницу от текущего cursor-а до logical EOF.
    fn refill_read_cache(&mut self) -> SourceResult<()> {
        let requested_page_bytes = if self.latency_first_read_pending {
            self.latency_first_read_bytes
        } else {
            self.maximum_read_bytes
        };
        let requested_page_bytes_u64 =
            u64::try_from(requested_page_bytes.get()).map_err(|_| SourceError::InvalidConfig {
                field: "adaptive_range_source.page_bytes",
                message: "Range page length does not fit u64".to_owned(),
            })?;
        let page_start = self
            .position
            .checked_div(requested_page_bytes_u64)
            .and_then(|page_index| page_index.checked_mul(requested_page_bytes_u64))
            .ok_or_else(|| SourceError::InvalidConfig {
                field: "adaptive_range_source.page_start",
                message: "aligned Range page start overflows u64".to_owned(),
            })?;
        let remaining_bytes = self.exposed_content_bytes.saturating_sub(page_start);
        let page_bytes = requested_page_bytes
            .get()
            .min(usize::try_from(remaining_bytes).unwrap_or(usize::MAX));
        let page_bytes = NonZeroUsize::new(page_bytes).ok_or(SourceError::UnexpectedEof {
            offset: page_start,
            expected_bytes: 1,
            actual_bytes: 0,
        })?;
        let bytes = self.fetch_range(page_start, page_bytes)?;
        if bytes.is_empty() {
            return Err(SourceError::UnexpectedEof {
                offset: page_start,
                expected_bytes: page_bytes.get(),
                actual_bytes: 0,
            });
        }
        while self.read_cache.len() >= self.maximum_cached_pages.get() {
            self.read_cache.pop_front();
        }
        self.read_cache.push_back(AdaptiveRangeReadCache {
            start: page_start,
            bytes,
        });
        self.latency_first_read_pending = false;
        Ok(())
    }

    /// Продвигает cursor после уже проверенного cached read-а.
    fn advance_position(&mut self, copied_bytes: usize) -> SourceResult<()> {
        self.position = self
            .position
            .checked_add(
                u64::try_from(copied_bytes).map_err(|_| SourceError::InvalidConfig {
                    field: "adaptive_range_source.read_length",
                    message: "read length does not fit u64".to_owned(),
                })?,
            )
            .ok_or_else(|| SourceError::InvalidConfig {
                field: "adaptive_range_source.position",
                message: "source position overflows u64".to_owned(),
            })?;
        Ok(())
    }
}

impl ByteSource for AdaptiveRangeByteSource {
    /// Возвращает не больше caller buffer-а, переиспользуя одну bounded Range-страницу.
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if cancellation.is_cancelled() || self.context.cancellation().is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let remaining_bytes = self.exposed_content_bytes.saturating_sub(self.position);
        if remaining_bytes == 0 {
            return Ok(0);
        }
        let cached_bytes = self
            .read_cache
            .iter()
            .find_map(|cache| cache.copy_from(self.position, output));
        let copied_bytes = match cached_bytes {
            Some(copied_bytes) => copied_bytes,
            None => {
                self.refill_read_cache()?;
                self.read_cache
                    .iter()
                    .find_map(|cache| cache.copy_from(self.position, output))
                    .ok_or(SourceError::UnexpectedEof {
                        offset: self.position,
                        expected_bytes: 1,
                        actual_bytes: 0,
                    })?
            }
        };
        self.advance_position(copied_bytes)?;
        Ok(copied_bytes)
    }

    /// Переставляет логический cursor; EOF обрабатывается последующим `read`.
    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        if !self.read_cache.iter().any(|cache| cache.contains(offset)) {
            self.latency_first_read_pending = true;
        }
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

    /// Возвращает consumer-visible длину; physical identity остаётся проверенной отдельно.
    fn content_length(&self) -> Option<u64> {
        Some(self.exposed_content_bytes)
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
        // Range source никогда не прикрепляет restartable streaming attempt; если future
        // owner всё же расширит этот path, interruption остаётся terminal, а не retryable I/O.
        AdaptiveTransportError::RestartableReadInterrupted => SourceError::Cancelled,
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
        AdaptiveTransportError::InvalidResourcePolicy { .. } => {
            SourceError::HttpRequestPolicyRejected {
                reason: HttpRequestPolicyFailure::ResourcePolicyRejected,
            }
        }
    }
}
