//! Как один hop HLS-движка становится просьбой к странице и как её отказ становится
//! ошибкой, понятной слоям выше (повторы, VOD-кэш, ошибки демуксера).
//!
//! Здесь встречаются три словаря, и только здесь:
//! - запрос «розетки» `HttpBoundedFetchRequest` (цель, диапазон, лимит тела);
//! - договор протокола 1.1: адрес-заглушка номерка, пределы `fetch_resource`;
//! - нейтральная очередь `pushed-media-source` ([`ResourceFetchSpec`], [`ResourceFetchError`]).
//!
//! Главный инвариант (инвариант 2 моста): **плеер просит у страницы только номерки**.
//! Цель, которая не является адресом-заглушкой ровно одного вида
//! (`https://resource.fastiplayer-bridge.invalid/<N>`), отвергается здесь, ДО очереди:
//! запроса в протокол нет вовсе. Так же отвергается всё, чего протокол не может
//! записать (смещение больше 2⁵³, диапазон больше потолка `fetch_resource`), — иначе
//! неотправляемый запрос висел бы в очереди без ответа.

use std::io;
use std::num::NonZeroU64;

use browser_bridge_protocol::limits::{MAX_JSON_SAFE_INTEGER, MAX_RESOURCE_FETCH_BYTES};
use browser_bridge_protocol::parse_resource_address;
use pushed_media_source::{
    ExternalResourceHandle, ResourceByteRange, ResourceExtent, ResourceFetchError,
    ResourceFetchSpec,
};
use source_core::{
    HttpBoundedFetchRequest, HttpExternalFailureContext, HttpExternalTransportFailure,
    HttpRequestPolicyFailure, HttpRetryAfter, HttpStatusCode, SourceError,
};

/// Просьба к странице для одного hop-а и всё, что нужно, чтобы честно назвать её отказ.
#[derive(Debug)]
pub(super) struct BridgeFetchPlan {
    /// Что просим у очереди: номерок, весь ресурс или диапазон, потолок тела.
    spec: ResourceFetchSpec,
    /// Как назвать отказ этой просьбы.
    failures: BridgeFailureContext,
}

impl BridgeFetchPlan {
    /// Переводит запрос «розетки» в просьбу по номерку.
    ///
    /// # Errors
    /// - цель не адрес-заглушка номерка → `HttpRequestPolicyRejected{TargetResolution}`
    ///   (неповторяемо: плеер не попросит у страницы ничего, кроме номерков);
    /// - диапазон, которого протокол не записывает (смещение больше 2⁵³ или конец за
    ///   пределом `u64`) → `HttpRequestPolicyRejected{ResourceBoundExceeded}`;
    /// - диапазон длиннее потолка `fetch_resource` → `HttpBodyTooLarge` (так же, как
    ///   ответил бы честный сервер на слишком большой ресурс).
    pub(super) fn for_request(request: &HttpBoundedFetchRequest) -> Result<Self, SourceError> {
        let failures = BridgeFailureContext {
            external: request.external_failure_context(),
            range_offset: request.byte_range().map_or(0, |range| range.start()),
        };
        let handle = resource_handle_of(request)?;
        let max_bytes = capped_body_limit(request);
        let extent = match request.byte_range() {
            None => ResourceExtent::Whole,
            Some(range) => {
                let length = u64::try_from(range.length().get()).unwrap_or(u64::MAX);
                if length > MAX_RESOURCE_FETCH_BYTES {
                    return Err(failures
                        .external
                        .failure(HttpExternalTransportFailure::BodyTooLarge));
                }
                if range.start() > MAX_JSON_SAFE_INTEGER {
                    return Err(policy_rejection(
                        HttpRequestPolicyFailure::ResourceBoundExceeded,
                    ));
                }
                // Длина диапазона ненулевая по типу `HttpBoundedByteRange`.
                let length = NonZeroU64::new(length).ok_or_else(|| {
                    policy_rejection(HttpRequestPolicyFailure::ResourceBoundExceeded)
                })?;
                let range = ResourceByteRange::new(range.start(), length).map_err(|_overflow| {
                    policy_rejection(HttpRequestPolicyFailure::ResourceBoundExceeded)
                })?;
                ResourceExtent::Range(range)
            }
        };
        Ok(Self {
            spec: ResourceFetchSpec {
                handle,
                extent,
                max_bytes,
            },
            failures,
        })
    }

    /// Просьба для очереди.
    pub(super) const fn spec(&self) -> ResourceFetchSpec {
        self.spec
    }

    /// Как назвать отказ этой просьбы (переживает сам запрос: нужна потоковому телу).
    pub(super) fn into_failures(self) -> BridgeFailureContext {
        self.failures
    }

    /// Ошибка очереди → ошибка «розетки».
    pub(super) fn source_error(&self, error: ResourceFetchError) -> SourceError {
        self.failures.source_error(error)
    }
}

/// Secret-safe данные запроса для типизированных ошибок (адрес не печатается).
#[derive(Debug, Clone)]
pub(super) struct BridgeFailureContext {
    /// Контекст `source-core` для ошибок сети, статуса и размера.
    external: HttpExternalFailureContext,
    /// Начало диапазона (0 для всего ресурса) — для `UnexpectedEof` короткого ответа.
    range_offset: u64,
}

impl BridgeFailureContext {
    /// Переводит ошибку очереди в существующий вариант `SourceError`, сохраняя смысл
    /// «повторять ли» (`SourceError::is_transient_network_failure`), от которого
    /// зависят повторы adaptive-слоя:
    ///
    /// | очередь | `SourceError` | повтор |
    /// |---|---|---|
    /// | сеть, сторож не поднялся | `HttpBodyRead` | да |
    /// | страница молчала | `HttpTimeout` | да |
    /// | HTTP-статус | `HttpStatus` | 5xx/408/429 — да, 4xx — нет |
    /// | короткий диапазон | `UnexpectedEof` | да |
    /// | отказ фона | `HttpRequestPolicyRejected` | нет |
    /// | слишком большой | `HttpBodyTooLarge` | нет |
    /// | источник закрыт (печать) | `SourceClosed` | нет |
    /// | отмена | `Cancelled` | нет |
    /// | нарушение страницы, номера кончились | `HttpRequestPolicyRejected{WorkerStopped}` | нет |
    pub(super) fn source_error(&self, error: ResourceFetchError) -> SourceError {
        match error {
            ResourceFetchError::Cancelled => SourceError::Cancelled,
            ResourceFetchError::Network => self.interrupted("the page reported a network error"),
            ResourceFetchError::SilenceWatchdogUnavailable { .. } => {
                // Причину системы не копируем: для слоёв выше это обрыв, повтор уместен.
                self.interrupted("the resource silence watchdog did not start")
            }
            ResourceFetchError::PageSilent { .. } => self
                .external
                .failure(HttpExternalTransportFailure::TimedOut),
            ResourceFetchError::Http { status } => self.status(status),
            ResourceFetchError::Refused => {
                // Фон отказался качать ресурс: повтор того же номерка ничего не даст.
                policy_rejection(HttpRequestPolicyFailure::ResourcePolicyRejected)
            }
            ResourceFetchError::TooLarge | ResourceFetchError::ExceedsResourceCeiling { .. } => {
                self.external
                    .failure(HttpExternalTransportFailure::BodyTooLarge)
            }
            ResourceFetchError::ShortRange {
                requested,
                received,
            } => SourceError::UnexpectedEof {
                offset: self.range_offset,
                expected_bytes: usize::try_from(requested).unwrap_or(usize::MAX),
                actual_bytes: usize::try_from(received).unwrap_or(usize::MAX),
            },
            ResourceFetchError::SourceClosed => SourceError::SourceClosed,
            // Очередь мертва (сеанс закроет переводчик кодом 4003) или номера кончились:
            // транспорт больше не работает, повторять нечего.
            ResourceFetchError::PageViolation(_) | ResourceFetchError::RequestIdsExhausted => {
                policy_rejection(HttpRequestPolicyFailure::WorkerStopped)
            }
        }
    }

    /// Обрыв связи (повторяемо).
    fn interrupted(&self, reason: &'static str) -> SourceError {
        self.external
            .failure(HttpExternalTransportFailure::Interrupted(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                reason,
            )))
    }

    /// HTTP-статус сервера страницы; число вне словаря HTTP — обрыв (протокол такого
    /// не пропускает, но падать здесь не из-за чего).
    fn status(&self, status: u16) -> SourceError {
        match HttpStatusCode::from_u16(status) {
            Ok(status) => self.external.failure(HttpExternalTransportFailure::Status {
                status,
                retry_after: HttpRetryAfter::Unavailable,
            }),
            Err(_invalid) => self.interrupted("the page reported an invalid HTTP status"),
        }
    }
}

/// Номерок из цели запроса; всё, что не адрес-заглушка, — отказ без запроса.
fn resource_handle_of(
    request: &HttpBoundedFetchRequest,
) -> Result<ExternalResourceHandle, SourceError> {
    // Адрес раскрывается только здесь и никуда не пишется: в журнал идёт только факт.
    parse_resource_address(request.target().expose_secret_for_request())
        .map(|handle| ExternalResourceHandle::new(handle.0))
        .ok_or_else(|| {
            tracing::warn!("The HLS engine asked for a resource that is not a bridge handle");
            policy_rejection(HttpRequestPolicyFailure::TargetResolution)
        })
}

/// Потолок тела просьбы: лимит движка, но не больше потолка `fetch_resource`
/// протокола (128 МиБ). Ресурс больше — страница честно ответит `too_large`.
fn capped_body_limit(request: &HttpBoundedFetchRequest) -> NonZeroU64 {
    let engine_limit = u64::try_from(request.maximum_body_bytes().get()).unwrap_or(u64::MAX);
    let limit = engine_limit.min(MAX_RESOURCE_FETCH_BYTES);
    // Оба числа ненулевые: лимит движка — `NonZeroUsize`, потолок протокола — константа.
    NonZeroU64::new(limit).unwrap_or(NonZeroU64::MIN)
}

/// Отказ политики без раскрытия цели.
const fn policy_rejection(reason: HttpRequestPolicyFailure) -> SourceError {
    SourceError::HttpRequestPolicyRejected { reason }
}
