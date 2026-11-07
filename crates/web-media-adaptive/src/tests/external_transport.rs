//! «Розетка» транспорта: вся сеть context-а идёт через подставленный транспорт,
//! а повторы, редиректы, кэш, секреты и отмена остаются над ним. Сети нет.

use std::collections::VecDeque;
use std::fmt;
use std::io;

use bytes::Bytes;
use media_core::DemuxSeekCancellationToken;
use source_core::{
    HttpBoundedFetchRequest, HttpBoundedResponse, HttpExternalTransportFailure,
    HttpRangeResponseMetadata, HttpRequestAttemptId, HttpRetryAfter, HttpStatusCode, SourceError,
    SourceResult,
};

use super::*;

/// Какой из трёх входов розетки вызвал context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportOperation {
    BufferedBlocking,
    BufferedAbortable,
    StreamingOpen,
}

/// Что поддельный транспорт увидел в одном вызове.
#[derive(Debug, Clone)]
struct ObservedTransportCall {
    operation: TransportOperation,
    exposed_target: String,
    header_names: Vec<String>,
}

/// Заранее заданный ответ на один вызов.
enum ScriptedHop {
    /// Тело ресурса: буферный путь отдаёт его целиком, потоковый — двумя кусками.
    Body(&'static [u8]),
    /// Редирект на абсолютную цель.
    Redirect(HttpRequestTarget),
    /// Сбой внешнего транспорта в общем словаре source-core.
    Failure(fn() -> HttpExternalTransportFailure),
}

/// Поддельный внешний транспорт: отвечает по сценарию и записывает каждый вызов.
#[derive(Default)]
struct ScriptedTransport {
    script: Mutex<VecDeque<ScriptedHop>>,
    calls: Mutex<Vec<ObservedTransportCall>>,
}

impl fmt::Debug for ScriptedTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ScriptedTransport")
    }
}

impl ScriptedTransport {
    fn scripted(hops: impl IntoIterator<Item = ScriptedHop>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(hops.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<ObservedTransportCall> {
        self.calls.lock().expect("calls mutex").clone()
    }

    /// Записывает вызов и выдаёт следующий шаг сценария.
    ///
    /// Пустой сценарий — отдельная неповторяемая ошибка: лишний вызов не зависнет,
    /// а упадёт на проверке числа вызовов.
    fn next_hop(
        &self,
        operation: TransportOperation,
        request: &HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<ScriptedHop> {
        // Вызов записывается ДО проверки отмены: тест отмены доказывает, что context
        // не дошёл до розетки, а не то, что подделка сама отказалась.
        self.calls
            .lock()
            .expect("calls mutex")
            .push(ObservedTransportCall {
                operation,
                exposed_target: request.target().expose_secret_for_request().to_owned(),
                header_names: request
                    .headers()
                    .iter()
                    .map(|header| header.name.to_ascii_lowercase())
                    .collect(),
            });
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        self.script.lock().expect("script mutex").pop_front().ok_or(
            SourceError::HttpRequestPolicyRejected {
                reason: source_core::HttpRequestPolicyFailure::WorkerStopped,
            },
        )
    }

    fn buffered(
        &self,
        operation: TransportOperation,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<AdaptiveBufferedHop> {
        match self.next_hop(operation, &request, cancellation)? {
            ScriptedHop::Body(body) => Ok(AdaptiveBufferedHop::Complete(
                HttpBoundedResponse::from_external_body(&request, body.to_vec())?,
            )),
            ScriptedHop::Redirect(next_target) => Ok(AdaptiveBufferedHop::Redirect { next_target }),
            ScriptedHop::Failure(failure) => {
                Err(request.external_failure_context().failure(failure()))
            }
        }
    }
}

impl AdaptiveResourceTransport for ScriptedTransport {
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
            match self.next_hop(TransportOperation::StreamingOpen, &request, cancellation)? {
                ScriptedHop::Body(body) => {
                    let split = body.len() / 2;
                    Ok(AdaptiveStreamingHop::Body(Box::new(
                        ScriptedStreamingBody {
                            pending_chunks: [&body[..split], &body[split..]]
                                .into_iter()
                                .filter(|chunk| !chunk.is_empty())
                                .map(Bytes::from_static)
                                .collect(),
                            received_body_bytes: 0,
                        },
                    )))
                }
                ScriptedHop::Redirect(next_target) => {
                    Ok(AdaptiveStreamingHop::Redirect { next_target })
                }
                ScriptedHop::Failure(failure) => {
                    Err(request.external_failure_context().failure(failure()))
                }
            }
        })
    }
}

/// Потоковое тело поддельного транспорта без Range-метаданных и без id запроса.
#[derive(Debug)]
struct ScriptedStreamingBody {
    pending_chunks: VecDeque<Bytes>,
    received_body_bytes: usize,
}

impl AdaptiveStreamingBody for ScriptedStreamingBody {
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

/// Настройки context-а над поддельным транспортом.
struct TransportContextOptions {
    redirects: RedirectPolicy,
    presentation: MediaPresentation,
    authorization: Option<&'static str>,
    endpoint_expiry_observer: Option<Arc<dyn EndpointExpiryObserver>>,
}

impl TransportContextOptions {
    fn vod() -> Self {
        Self {
            redirects: same_origin_redirects(),
            presentation: MediaPresentation::Vod,
            authorization: None,
            endpoint_expiry_observer: None,
        }
    }
}

fn open_request(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    options: &TransportContextOptions,
    secrets: SecretRequestContext,
) -> TransportOpenRequest {
    let source = SourceIdentity::new(72);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("adaptive-transport-test").expect("format identity"),
    );
    let semantic =
        SemanticIdentity::new(source, "adaptive-transport-test").expect("semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("component identity");
    let mut request = TransportOpenRequest::new(
        TransportProviderId::new("adaptive-transport-test").expect("provider id"),
        component,
        target.clone(),
        options.presentation,
        SourceGeneration::new(1),
        secrets,
        options.redirects,
        cancellation,
    )
    .expect("transport request");
    if let Some(observer) = options.endpoint_expiry_observer.clone() {
        request = request.with_endpoint_expiry_observer(observer);
    }
    request
}

fn root_scope(target: &HttpRequestTarget) -> SecretRequestScope {
    SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("root scope"))
}

fn source_config() -> SourceRuntimeConfig {
    SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config")
}

fn limits() -> AdaptiveTransportLimits {
    AdaptiveTransportLimits::new(
        NonZeroUsize::new(1024).expect("manifest bound"),
        NonZeroUsize::new(1024).expect("segment bound"),
        NonZeroUsize::new(8).expect("descriptor bound"),
    )
}

/// Три попытки с короткими паузами: тест повторов не ждёт секундами.
fn retry_policy() -> AdaptiveRetryPolicy {
    AdaptiveRetryPolicy::new(
        NonZeroU8::new(3).expect("attempts"),
        Duration::from_millis(1),
        Duration::from_millis(2),
        Duration::from_millis(2),
    )
    .expect("retry policy")
}

fn transport_context(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    transport: Arc<ScriptedTransport>,
    options: TransportContextOptions,
) -> AdaptiveHttpContext {
    let headers = options
        .authorization
        .map(|value| vec![HttpHeader::new("authorization", value)])
        .unwrap_or_default();
    let secrets = SecretRequestContext::builder(root_scope(target))
        .with_headers(ValidatedHttpHeaders::new(headers).expect("headers"))
        .build();
    AdaptiveHttpContext::with_transport(
        open_request(target, cancellation, &options, secrets),
        transport,
        &source_config(),
        limits(),
        retry_policy(),
    )
    .expect("context over external transport")
}

fn local_target(path: &str) -> HttpRequestTarget {
    HttpRequestTarget::parse_exact(format!("https://media.example.test{path}"))
        .expect("valid target")
}

fn segment_request(target: &HttpRequestTarget) -> AdaptiveResourceFetchRequest {
    AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target.clone(),
        NonZeroUsize::new(64).expect("body bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    )
}

fn read_stream_to_end(resource: &mut AdaptiveStreamingResource) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = resource.next_chunk().expect("stream chunk") {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

fn interrupted() -> HttpExternalTransportFailure {
    HttpExternalTransportFailure::Interrupted(io::Error::new(
        io::ErrorKind::ConnectionReset,
        "bridge dropped",
    ))
}

fn timed_out() -> HttpExternalTransportFailure {
    HttpExternalTransportFailure::TimedOut
}

fn not_found() -> HttpExternalTransportFailure {
    HttpExternalTransportFailure::Status {
        status: HttpStatusCode::NOT_FOUND,
        retry_after: HttpRetryAfter::Unavailable,
    }
}

fn gone() -> HttpExternalTransportFailure {
    HttpExternalTransportFailure::Status {
        status: HttpStatusCode::GONE,
        retry_after: HttpRetryAfter::Unavailable,
    }
}

fn too_large() -> HttpExternalTransportFailure {
    HttpExternalTransportFailure::BodyTooLarge
}

#[test]
fn buffered_fetch_goes_through_injected_transport_and_returns_its_body() {
    let target = local_target("/vod/seg-1.ts");
    let transport = ScriptedTransport::scripted([ScriptedHop::Body(b"segment-bytes")]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions {
            authorization: Some("Bearer scoped"),
            ..TransportContextOptions::vod()
        },
    );

    let fetched = context
        .fetch_resource_blocking(segment_request(&target))
        .expect("fetch over external transport");

    assert_eq!(fetched.bytes(), b"segment-bytes");
    assert!(fetched.range_metadata().is_none());
    assert_eq!(
        fetched.final_target().expose_secret_for_request(),
        target.expose_secret_for_request()
    );
    let calls = transport.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].operation, TransportOperation::BufferedBlocking);
    assert_eq!(calls[0].exposed_target, target.expose_secret_for_request());
    // Слой секретов работает над розеткой: scoped-заголовок доходит до транспорта.
    assert_eq!(calls[0].header_names, vec!["authorization".to_owned()]);
}

#[test]
fn streaming_open_goes_through_injected_transport() {
    let target = local_target("/vod/seg-2.ts");
    let transport = ScriptedTransport::scripted([ScriptedHop::Body(b"0123456789")]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions::vod(),
    );

    let mut resource = context
        .open_resource_streaming_blocking(
            segment_request(&target),
            DemuxSeekCancellationToken::new(),
        )
        .expect("stream over external transport");

    assert_eq!(read_stream_to_end(&mut resource), b"0123456789");
    assert_eq!(resource.received_body_bytes(), 10);
    assert!(resource.range_metadata().is_none());
    assert_eq!(resource.network_request_attempt_id(), None);
    let calls = transport.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].operation, TransportOperation::StreamingOpen);
}

#[test]
fn abortable_manifest_fetch_goes_through_injected_transport() {
    let target = local_target("/live/index.m3u8");
    let transport = ScriptedTransport::scripted([ScriptedHop::Body(b"#EXTM3U")]);
    let mut fetcher = AdaptiveManifestFetcher::new(transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions {
            presentation: MediaPresentation::Live,
            ..TransportContextOptions::vod()
        },
    ))
    .expect("manifest fetcher");

    fetcher
        .request(
            ManifestFetchRequest::new(target, SourceGeneration::new(1)),
            Instant::now(),
        )
        .expect("manifest request");
    let resource = poll_manifest_ready(&mut fetcher);

    assert_eq!(resource.bytes().as_ref(), b"#EXTM3U");
    let calls = transport.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].operation, TransportOperation::BufferedAbortable);
}

#[test]
fn retryable_failures_are_retried_until_transport_succeeds() {
    let target = local_target("/vod/seg-3.ts");
    let transport = ScriptedTransport::scripted([
        ScriptedHop::Failure(interrupted),
        ScriptedHop::Failure(timed_out),
        ScriptedHop::Body(b"third-time"),
    ]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions::vod(),
    );

    let fetched = context
        .fetch_resource_blocking(segment_request(&target))
        .expect("third attempt succeeds");

    assert_eq!(fetched.bytes(), b"third-time");
    assert_eq!(transport.calls().len(), 3, "one transport call per attempt");
}

#[test]
fn retry_budget_bounds_streaming_calls_for_persistent_retryable_failure() {
    let target = local_target("/vod/seg-4.ts");
    let transport = ScriptedTransport::scripted([
        ScriptedHop::Failure(interrupted),
        ScriptedHop::Failure(interrupted),
        ScriptedHop::Failure(interrupted),
        ScriptedHop::Body(b"never-reached"),
    ]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions::vod(),
    );

    let error = context
        .open_resource_streaming_blocking(
            segment_request(&target),
            DemuxSeekCancellationToken::new(),
        )
        .expect_err("budget exhausted");

    assert!(matches!(
        error,
        AdaptiveTransportError::Source(SourceError::HttpBodyRead { .. })
    ));
    assert_eq!(transport.calls().len(), 3, "exactly maximum_attempts calls");
}

#[test]
fn non_retryable_failures_reach_caller_after_single_transport_call() {
    for (failure, expected_status) in [
        (not_found as fn() -> HttpExternalTransportFailure, Some(404)),
        (too_large, None),
    ] {
        let target = local_target("/vod/seg-5.ts");
        let transport = ScriptedTransport::scripted([
            ScriptedHop::Failure(failure),
            ScriptedHop::Body(b"must-not-be-fetched"),
        ]);
        let context = transport_context(
            &target,
            CancellationToken::new(),
            transport.clone(),
            TransportContextOptions::vod(),
        );

        let error = context
            .fetch_resource_blocking(segment_request(&target))
            .expect_err("non-retryable failure");

        assert_eq!(error.http_status_code(), expected_status);
        assert_eq!(transport.calls().len(), 1, "no retry for {error}");
    }
}

#[test]
fn allowed_redirect_continues_with_new_target_through_same_transport() {
    let target = local_target("/vod/entry.ts");
    let next_target = local_target("/cdn/seg-6.ts");
    let transport = ScriptedTransport::scripted([
        ScriptedHop::Redirect(next_target.clone()),
        ScriptedHop::Body(b"after-redirect"),
    ]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions::vod(),
    );

    let fetched = context
        .fetch_resource_blocking(segment_request(&target))
        .expect("same-origin redirect allowed");

    assert_eq!(fetched.bytes(), b"after-redirect");
    assert_eq!(
        fetched.final_target().expose_secret_for_request(),
        next_target.expose_secret_for_request()
    );
    let calls = transport.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].exposed_target,
        next_target.expose_secret_for_request()
    );
}

#[test]
fn rejected_redirect_fails_without_second_transport_call() {
    let target = local_target("/vod/entry.ts");
    let foreign_target =
        HttpRequestTarget::parse_exact("https://other.example.test/seg.ts").expect("target");
    let transport = ScriptedTransport::scripted([
        ScriptedHop::Redirect(foreign_target.clone()),
        ScriptedHop::Body(b"must-not-be-fetched"),
    ]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions::vod(),
    );

    let buffered_error = context
        .fetch_resource_blocking(segment_request(&target))
        .expect_err("cross-origin redirect rejected");
    assert!(matches!(
        buffered_error,
        AdaptiveTransportError::Redirect(_)
    ));
    assert_eq!(transport.calls().len(), 1);

    let streaming_transport = ScriptedTransport::scripted([
        ScriptedHop::Redirect(foreign_target),
        ScriptedHop::Body(b"must-not-be-fetched"),
    ]);
    let streaming_context = transport_context(
        &target,
        CancellationToken::new(),
        streaming_transport.clone(),
        TransportContextOptions::vod(),
    );
    let streaming_error = streaming_context
        .open_resource_streaming_blocking(
            segment_request(&target),
            DemuxSeekCancellationToken::new(),
        )
        .expect_err("cross-origin redirect rejected");
    assert!(matches!(
        streaming_error,
        AdaptiveTransportError::Redirect(_)
    ));
    assert_eq!(streaming_transport.calls().len(), 1);
}

#[test]
fn cancellation_before_request_never_calls_transport() {
    let target = local_target("/vod/seg-7.ts");
    let transport = ScriptedTransport::scripted([
        ScriptedHop::Body(b"must-not-be-fetched"),
        ScriptedHop::Body(b"must-not-be-fetched"),
    ]);
    let cancellation = CancellationToken::new();
    let context = transport_context(
        &target,
        cancellation.clone(),
        transport.clone(),
        TransportContextOptions::vod(),
    );
    cancellation.cancel();

    let buffered = context.fetch_resource_blocking(segment_request(&target));
    let streaming = context.open_resource_streaming_blocking(
        segment_request(&target),
        DemuxSeekCancellationToken::new(),
    );

    assert!(matches!(buffered, Err(AdaptiveTransportError::Cancelled)));
    assert!(matches!(streaming, Err(AdaptiveTransportError::Cancelled)));
    assert!(transport.calls().is_empty());
}

#[test]
fn completed_vod_resource_is_replayed_without_second_transport_call() {
    let target = local_target("/vod/seg-8.ts");
    let transport = ScriptedTransport::scripted([
        ScriptedHop::Body(b"cached-once"),
        ScriptedHop::Body(b"must-not-be-fetched"),
    ]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions::vod(),
    );

    let mut streamed = context
        .open_resource_streaming_blocking(
            segment_request(&target),
            DemuxSeekCancellationToken::new(),
        )
        .expect("first stream");
    assert_eq!(read_stream_to_end(&mut streamed), b"cached-once");
    let buffered = context
        .fetch_resource_blocking(segment_request(&target))
        .expect("buffered replay");
    let mut replayed = context
        .open_resource_streaming_blocking(
            segment_request(&target),
            DemuxSeekCancellationToken::new(),
        )
        .expect("streaming replay");

    assert_eq!(buffered.bytes(), b"cached-once");
    assert_eq!(read_stream_to_end(&mut replayed), b"cached-once");
    assert_eq!(
        transport.calls().len(),
        1,
        "completed VOD bytes come from cache"
    );
}

#[test]
fn auxiliary_view_shares_transport_but_not_completed_cache() {
    let target = local_target("/vod/seg-9.ts");
    let transport = ScriptedTransport::scripted([
        ScriptedHop::Body(b"video-bytes"),
        ScriptedHop::Body(b"auxiliary-bytes"),
    ]);
    let video_context = transport_context(
        &target,
        CancellationToken::new(),
        transport.clone(),
        TransportContextOptions::vod(),
    );
    let auxiliary = video_context.for_auxiliary_resources();

    let video = video_context
        .fetch_resource_blocking(segment_request(&target))
        .expect("video fetch");
    let auxiliary_fetch = auxiliary
        .fetch_resource_blocking(segment_request(&target))
        .expect("auxiliary fetch");
    let video_again = video_context
        .fetch_resource_blocking(segment_request(&target))
        .expect("video replay");

    assert_eq!(video.bytes(), b"video-bytes");
    // Свежий кэш вспомогательного вида не видит байты видео — новый вызов транспорта.
    assert_eq!(auxiliary_fetch.bytes(), b"auxiliary-bytes");
    // Кэш видео не тронут вспомогательным запросом.
    assert_eq!(video_again.bytes(), b"video-bytes");
    assert_eq!(
        transport.calls().len(),
        2,
        "one shared transport, two caches"
    );
}

#[test]
fn endpoint_expiry_observer_sees_external_transport_status() {
    let target = local_target("/vod/seg-10.ts");
    let observer = Arc::new(RecordingExpiryObserver::default());
    let transport = ScriptedTransport::scripted([ScriptedHop::Failure(gone)]);
    let context = transport_context(
        &target,
        CancellationToken::new(),
        transport,
        TransportContextOptions {
            endpoint_expiry_observer: Some(observer.clone()),
            ..TransportContextOptions::vod()
        },
    );

    let error = context
        .fetch_resource_blocking(segment_request(&target))
        .expect_err("410 stays an error");

    assert_eq!(error.http_status_code(), Some(410));
    let signals = observer.recorded_signals();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].reason(), EndpointExpiryReason::ResourceExpired);
    assert_eq!(
        signals[0].resource_kind(),
        EndpointExpiryResourceKind::MediaSegment
    );
}

/// Внешний транспорт не видит scoped cookie jar: cookies отклоняются, а не теряются молча.
#[test]
fn with_transport_rejects_cookie_secrets_it_cannot_forward() {
    let target = local_target("/vod/index.m3u8");
    let options = TransportContextOptions::vod();
    let cookie_secrets = SecretRequestContext::builder(root_scope(&target))
        .with_serialized_cookies("session=abc")
        .expect("cookies")
        .build();
    let cookie_result = AdaptiveHttpContext::with_transport(
        open_request(&target, CancellationToken::new(), &options, cookie_secrets),
        ScriptedTransport::scripted([]),
        &source_config(),
        limits(),
        retry_policy(),
    );
    assert!(matches!(
        cookie_result,
        Err(AdaptiveTransportError::SecretScopeRejected)
    ));
}
