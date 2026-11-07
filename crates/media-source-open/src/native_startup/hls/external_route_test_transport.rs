//! Поддельный внешний транспорт в памяти для тестов внешнего маршрута native HLS.
//!
//! Он ведёт себя как мост в браузер: каждый ресурс отдаётся по адресу-номерку
//! `https://resource.fastiplayer-bridge.invalid/<N>`, которого нет в реальной сети
//! (домен `.invalid` не резолвится по RFC 6761). Если бы хоть один запрос ушёл в
//! обычную сеть, он упал бы на DNS — поэтому успешный тест сам доказывает, что
//! обычная сеть не тронута, а журнал вызовов доказывает, что всё шло через «розетку».
//!
//! Контракт розетки соблюдается буквально: редиректов не бывает, повторов транспорт
//! не делает, диапазон отдаётся ровно запрошенным куском, лимит тела проверяется
//! тем же `HttpBoundedResponse::from_external_body`, что у настоящего моста.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use bytes::Bytes;
use source_core::{
    CancellationToken, HttpBoundedFetchRequest, HttpBoundedResponse, HttpExternalTransportFailure,
    HttpRangeResponseMetadata, HttpRequestAttemptId, HttpRequestTarget, HttpRetryAfter,
    HttpStatusCode, SourceError, SourceResult,
};
use web_media_adaptive::{
    AdaptiveBufferedHop, AdaptiveResourceTransport, AdaptiveStreamingBody, AdaptiveStreamingHop,
    AdaptiveTransportFuture,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExactSelectionIdentity, ExtractionGeneration,
    SemanticIdentity,
};
use web_media_transport_api::{MediaPresentation, SourceGeneration, TransportOpenRequest};

use crate::native_web_source::hls::NativeHlsUrl;
use crate::safe_media_label::SafeMediaLabel;

/// Адрес ресурса по номерку, как его переписывает экстеншен.
pub(super) fn bridge_address(handle: u32) -> String {
    format!("https://resource.fastiplayer-bridge.invalid/{handle}")
}

/// Источник native HLS, корень которого — номерок `root_handle`.
pub(super) fn bridge_source(root_handle: u32) -> NativeHlsUrl {
    NativeHlsUrl::new(
        HttpRequestTarget::parse_exact(bridge_address(root_handle)).expect("bridge root target"),
        SafeMediaLabel::from_service_safe_label("Bridge native HLS"),
    )
}

/// Что транспорт отдаёт по одному адресу.
#[derive(Clone)]
pub(super) enum InMemoryResource {
    /// Неизменное тело (сегмент, init, ключ, VOD-плейлист).
    Body(Vec<u8>),
    /// Последовательные снимки (live-плейлист): N-й запрос получает N-й снимок,
    /// после последнего повторяется последний.
    Snapshots(Vec<Vec<u8>>),
    /// Сервер ответил этим статусом (через `HttpExternalTransportFailure::Status`).
    Status(u16),
}

/// Какой из трёх входов розетки вызвал context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransportOperation {
    BufferedBlocking,
    BufferedAbortable,
    StreamingOpen,
}

/// Один записанный вызов транспорта.
#[derive(Debug, Clone)]
struct ObservedCall {
    target: String,
    operation: TransportOperation,
}

/// Поддельный мост: карта «адрес → ответ» и журнал всех вызовов.
pub(super) struct InMemoryHlsTransport {
    resources: HashMap<String, InMemoryResource>,
    calls: Mutex<Vec<ObservedCall>>,
}

impl fmt::Debug for InMemoryHlsTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InMemoryHlsTransport")
            .field("resources", &self.resources.len())
            .finish_non_exhaustive()
    }
}

impl InMemoryHlsTransport {
    /// Транспорт без ресурсов: любой запрос получит 404.
    pub(super) fn new() -> Arc<Self> {
        Self::with_resources(Vec::new())
    }

    /// Транспорт с ресурсами по номеркам.
    pub(super) fn with_resources(
        resources: impl IntoIterator<Item = (u32, InMemoryResource)>,
    ) -> Arc<Self> {
        Arc::new(Self {
            resources: resources
                .into_iter()
                .map(|(handle, resource)| (bridge_address(handle), resource))
                .collect(),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Сколько раз запрашивали номерок (все входы розетки, включая повторы context-а).
    pub(super) fn requests_for(&self, handle: u32) -> usize {
        let address = bridge_address(handle);
        self.lock_calls()
            .iter()
            .filter(|call| call.target == address)
            .count()
    }

    /// Все вызовы розетки за время теста.
    pub(super) fn total_requests(&self) -> usize {
        self.lock_calls().len()
    }

    /// Какими входами розетки пользовались (по порядку вызовов).
    pub(super) fn operations(&self) -> Vec<TransportOperation> {
        self.lock_calls()
            .iter()
            .map(|call| call.operation)
            .collect()
    }

    /// Все запрошенные адреса принадлежат мосту — ничего мимо номерков.
    pub(super) fn every_request_used_bridge_addresses(&self) -> bool {
        self.lock_calls().iter().all(|call| {
            call.target
                .starts_with("https://resource.fastiplayer-bridge.invalid/")
        })
    }

    fn lock_calls(&self) -> MutexGuard<'_, Vec<ObservedCall>> {
        self.calls.lock().expect("transport call journal mutex")
    }

    /// Записывает вызов и выдаёт ответ с уже проверенными диапазоном и лимитом.
    fn respond(
        &self,
        operation: TransportOperation,
        request: &HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<HttpBoundedResponse> {
        let target = request.target().expose_secret_for_request().to_owned();
        // Номер этого запроса к адресу считается ДО записи: первый запрос — 0.
        let previous_requests = {
            let mut calls = self.lock_calls();
            let previous = calls.iter().filter(|call| call.target == target).count();
            calls.push(ObservedCall {
                target: target.clone(),
                operation,
            });
            previous
        };
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let whole_body = match self.resources.get(&target) {
            Some(InMemoryResource::Body(body)) => body.clone(),
            Some(InMemoryResource::Snapshots(snapshots)) => snapshots
                .get(previous_requests)
                .or_else(|| snapshots.last())
                .cloned()
                .unwrap_or_default(),
            Some(InMemoryResource::Status(status)) => return Err(status_failure(request, *status)),
            None => return Err(status_failure(request, 404)),
        };
        let body = match request.byte_range() {
            // Ровно запрошенный кусок; выход за конец даст короткое тело, и
            // `from_external_body` честно ответит `UnexpectedEof`.
            Some(range) => {
                let start = usize::try_from(range.start()).unwrap_or(usize::MAX);
                let end = start.saturating_add(range.length().get());
                whole_body
                    .get(start.min(whole_body.len())..end.min(whole_body.len()))
                    .unwrap_or_default()
                    .to_vec()
            }
            None => whole_body,
        };
        HttpBoundedResponse::from_external_body(request, body)
    }

    fn buffered(
        &self,
        operation: TransportOperation,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<AdaptiveBufferedHop> {
        self.respond(operation, &request, cancellation)
            .map(AdaptiveBufferedHop::Complete)
    }
}

/// Статус сервера в общем словаре внешнего транспорта.
fn status_failure(request: &HttpBoundedFetchRequest, status: u16) -> SourceError {
    let status = HttpStatusCode::from_u16(status).expect("valid fixture HTTP status");
    request
        .external_failure_context()
        .failure(HttpExternalTransportFailure::Status {
            status,
            retry_after: HttpRetryAfter::Unavailable,
        })
}

impl AdaptiveResourceTransport for InMemoryHlsTransport {
    fn fetch_buffered_hop_blocking(
        &self,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<AdaptiveBufferedHop> {
        self.buffered(TransportOperation::BufferedBlocking, request, cancellation)
    }

    fn fetch_buffered_hop_abortable<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveBufferedHop>> {
        Box::pin(async move {
            self.buffered(TransportOperation::BufferedAbortable, request, cancellation)
        })
    }

    fn open_streaming_hop<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveStreamingHop>> {
        Box::pin(async move {
            let response =
                self.respond(TransportOperation::StreamingOpen, &request, cancellation)?;
            Ok(AdaptiveStreamingHop::Body(Box::new(
                InMemoryStreamingBody::split_in_two(response.into_bytes()),
            )))
        })
    }
}

/// Потоковое тело: два куска, как будто ответ пришёл не одним пакетом.
#[derive(Debug)]
struct InMemoryStreamingBody {
    pending_chunks: VecDeque<Bytes>,
    received_body_bytes: usize,
}

impl InMemoryStreamingBody {
    fn split_in_two(body: Vec<u8>) -> Self {
        let whole = Bytes::from(body);
        let middle = whole.len() / 2;
        Self {
            pending_chunks: [whole.slice(..middle), whole.slice(middle..)]
                .into_iter()
                .filter(|chunk| !chunk.is_empty())
                .collect(),
            received_body_bytes: 0,
        }
    }
}

impl AdaptiveStreamingBody for InMemoryStreamingBody {
    fn next_chunk<'operation>(
        &'operation mut self,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<Option<Bytes>>> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let chunk = self.pending_chunks.pop_front();
            if let Some(chunk) = &chunk {
                self.received_body_bytes += chunk.len();
            }
            Ok(chunk)
        })
    }

    fn received_body_bytes(&self) -> usize {
        self.received_body_bytes
    }

    fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        None
    }

    fn request_attempt_id(&self) -> Option<HttpRequestAttemptId> {
        None
    }
}

/// Exact parent identity для прямых тестов портов (без production snapshot счётчика).
pub(super) fn test_parent_identity(source: &NativeHlsUrl) -> ExactSelectionIdentity {
    let source_identity = source.source_identity();
    ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source_identity,
            ExtractionGeneration::new(1),
            CandidateFormatIdentity::new("native-hls-vod").expect("format identity"),
        ),
        SemanticIdentity::new(source_identity, "native-hls-vod").expect("semantic identity"),
    )
    .expect("exact parent identity")
}

/// Production-форма VOD transport request-а к корню моста.
pub(super) fn vod_transport_request() -> TransportOpenRequest {
    let source = bridge_source(1);
    super::native_transport_request(
        &test_parent_identity(&source),
        &source,
        MediaPresentation::Vod,
        SourceGeneration::new(1),
        CancellationToken::new(),
    )
    .expect("native VOD transport request")
}

/// Декодирует base64-фикстуру (стандартный алфавит, переносы строк, `=` в конце).
///
/// Своя крошечная реализация вместо новой зависимости: фикстуры app-egui хранятся
/// в base64, а тестам здесь нужны их сырые байты.
pub(super) fn decode_base64(encoded: &str) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(encoded.len() * 3 / 4);
    // Накопитель битов: каждый символ даёт 6 бит, каждый полный байт уходит наружу.
    let mut accumulator = 0_u32;
    let mut accumulated_bits = 0_u8;
    for encoded_byte in encoded.bytes() {
        let value = match encoded_byte {
            // Фикстуры разбиты на строки: переносы не несут данных.
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            b'A'..=b'Z' => encoded_byte - b'A',
            b'a'..=b'z' => encoded_byte - b'a' + 26,
            b'0'..=b'9' => encoded_byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            other => panic!("invalid base64 fixture byte {other:#04x}"),
        };
        accumulator = (accumulator << 6) | u32::from(value);
        accumulated_bits += 6;
        if accumulated_bits >= 8 {
            accumulated_bits -= 8;
            decoded.push(u8::try_from(accumulator >> accumulated_bits).expect("one byte"));
            accumulator &= (1_u32 << accumulated_bits) - 1;
        }
    }
    decoded
}
