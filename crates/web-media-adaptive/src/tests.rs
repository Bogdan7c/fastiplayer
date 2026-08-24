//! Hermetic local-server evidence S31 adaptive foundation.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroU32, NonZeroUsize};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use demux_api::{
    OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentSequence,
    ProgressiveDemuxBufferLimits,
};
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, Demuxer, MediaMetadata, TrackInfo,
};
use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpHeader, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig,
    ValidatedHttpHeaders,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_transport_api::{
    EndpointExpiryObserver, EndpointExpiryReason, EndpointExpiryResourceKind, EndpointExpirySignal,
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretQueryOverride, SecretRequestContext, SecretRequestScope,
    SourceGeneration, TransportOpenRequest, TransportProviderId,
};

use super::*;

mod blocking_resource_fetch;
mod manifest_cancellation;
mod ordered_segment_read_ahead;
mod range_source;
mod retry_after;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
struct ObservedRequest {
    request_line: String,
    headers: String,
}

struct LocalServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<ObservedRequest>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl LocalServer {
    fn start(handler: impl Fn(usize, &ObservedRequest) -> Vec<u8> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        listener
            .set_nonblocking(true)
            .expect("set test listener nonblocking");
        let address = listener.local_addr().expect("local server address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_requests = Arc::clone(&requests);
        let handler = Arc::new(handler);
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Drop будит nonblocking listener пустым соединением; это не HTTP request.
                        if worker_stop.load(Ordering::Acquire) {
                            break;
                        }
                        let request = read_request(&mut stream);
                        let request_index = {
                            let mut observed = worker_requests.lock().expect("requests mutex");
                            let request_index = observed.len();
                            observed.push(request.clone());
                            request_index
                        };
                        let response = handler(request_index, &request);
                        stream.write_all(&response).expect("write response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("valid local target")
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("requests mutex").len()
    }

    fn requests(&self) -> Vec<ObservedRequest> {
        self.requests.lock().expect("requests mutex").clone()
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("local server thread");
        }
    }
}

fn read_request(stream: &mut TcpStream) -> ObservedRequest {
    stream
        .set_read_timeout(Some(TEST_TIMEOUT))
        .expect("request read timeout");
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).expect("read HTTP request");
        assert!(read > 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8(bytes).expect("ASCII HTTP request");
    let mut lines = text.lines();
    ObservedRequest {
        request_line: lines.next().unwrap_or_default().to_owned(),
        headers: lines.collect::<Vec<_>>().join("\n"),
    }
}

fn response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    )
    .into_bytes();
    for (name, value) in headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(body);
    response
}

fn context(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    redirects: RedirectPolicy,
    authorization: Option<&str>,
    segment_query: Option<&str>,
) -> AdaptiveHttpContext {
    context_with_queries(
        target,
        cancellation,
        redirects,
        authorization,
        segment_query,
        None,
        MediaPresentation::Vod,
    )
}

fn context_with_presentation(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    redirects: RedirectPolicy,
    authorization: Option<&str>,
    segment_query: Option<&str>,
    presentation: MediaPresentation,
) -> AdaptiveHttpContext {
    context_with_queries(
        target,
        cancellation,
        redirects,
        authorization,
        segment_query,
        None,
        presentation,
    )
}

fn context_with_queries(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    redirects: RedirectPolicy,
    authorization: Option<&str>,
    segment_query: Option<&str>,
    key_query: Option<&str>,
    presentation: MediaPresentation,
) -> AdaptiveHttpContext {
    context_with_options(
        target,
        cancellation,
        redirects,
        TestContextOptions {
            authorization,
            segment_query,
            key_query,
            presentation,
            endpoint_expiry_observer: None,
        },
    )
}

/// Named fixture options не превращают test boundary в набор positional `Option`-ов.
struct TestContextOptions<'value> {
    authorization: Option<&'value str>,
    segment_query: Option<&'value str>,
    key_query: Option<&'value str>,
    presentation: MediaPresentation,
    endpoint_expiry_observer: Option<Arc<dyn EndpointExpiryObserver>>,
}

/// Общий fixture builder прикрепляет optional production observer до создания HTTP context-а.
fn context_with_options(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    redirects: RedirectPolicy,
    options: TestContextOptions<'_>,
) -> AdaptiveHttpContext {
    let source = SourceIdentity::new(71);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("adaptive-test").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "adaptive-test").expect("semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("component identity");
    let scope =
        SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("root scope"));
    let headers = options
        .authorization
        .map(|value| vec![HttpHeader::new("authorization", value)])
        .unwrap_or_default();
    let mut secrets = SecretRequestContext::builder(scope)
        .with_headers(ValidatedHttpHeaders::new(headers).expect("headers"));
    if let Some(query) = options.segment_query {
        secrets = secrets
            .with_segment_query_override(SecretQueryOverride::new(query).expect("query override"));
    }
    if let Some(query) = options.key_query {
        secrets = secrets
            .with_key_query_override(SecretQueryOverride::new(query).expect("key query override"));
    }
    let mut request = TransportOpenRequest::new(
        TransportProviderId::new("adaptive-test").expect("provider id"),
        component,
        target.clone(),
        options.presentation,
        SourceGeneration::new(1),
        secrets.build(),
        redirects,
        cancellation,
    )
    .expect("transport request");
    if let Some(observer) = options.endpoint_expiry_observer {
        request = request.with_endpoint_expiry_observer(observer);
    }
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    AdaptiveHttpContext::new(
        request,
        &source_config,
        AdaptiveTransportLimits::new(
            NonZeroUsize::new(1024).expect("manifest bound"),
            NonZeroUsize::new(1024).expect("segment bound"),
            NonZeroUsize::new(8).expect("descriptor bound"),
        ),
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("attempts"),
            Duration::from_millis(5),
            Duration::from_millis(20),
            Duration::from_millis(20),
        )
        .expect("retry policy"),
    )
    .expect("adaptive context")
}

/// Observer fixture проверяет полный transport -> logical signal boundary.
#[derive(Default)]
struct RecordingExpiryObserver {
    signals: Mutex<Vec<EndpointExpirySignal>>,
}

impl EndpointExpiryObserver for RecordingExpiryObserver {
    fn observe_endpoint_expiry(&self, signal: EndpointExpirySignal) {
        self.signals
            .lock()
            .expect("expiry signal mutex")
            .push(signal);
    }
}

impl RecordingExpiryObserver {
    fn recorded_signals(&self) -> Vec<EndpointExpirySignal> {
        self.signals.lock().expect("expiry signal mutex").clone()
    }
}

fn same_origin_redirects() -> RedirectPolicy {
    RedirectPolicy::same_origin(RedirectHopLimit::new(4).expect("redirect limit"))
}

fn poll_manifest_ready(fetcher: &mut AdaptiveManifestFetcher) -> ManifestResource {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match fetcher.poll(Instant::now()) {
            ManifestPoll::Ready(resource) => return resource,
            ManifestPoll::TemporarilyUnavailable { retry_after } => {
                assert!(Instant::now() < deadline, "manifest poll timed out");
                thread::sleep(retry_after.min(Duration::from_millis(5)));
            }
            other => panic!("unexpected manifest result: {other:?}"),
        }
    }
}

fn poll_segment(source: &mut AdaptiveOrderedSegmentSource) -> SegmentPoll {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match source.poll_next(Instant::now()) {
            SegmentPoll::TemporarilyUnavailable { retry_after } => {
                assert!(Instant::now() < deadline, "segment poll timed out");
                thread::sleep(retry_after.min(Duration::from_millis(5)));
            }
            result => return result,
        }
    }
}

fn clock(units: u32, origin: i64) -> ComponentClockMetadata {
    ComponentClockMetadata::new(NonZeroU32::new(units).expect("clock units"), origin)
}

fn snapshot(
    generation: u64,
    descriptor: AdaptiveSegmentDescriptor,
    presentation: AdaptivePresentation,
    component_clock: ComponentClockMetadata,
    completion: AdaptiveSegmentCompletion,
) -> AdaptiveSegmentSnapshot {
    AdaptiveSegmentSnapshot::new(
        SourceGeneration::new(generation),
        presentation,
        component_clock,
        vec![descriptor],
        completion,
    )
    .expect("valid snapshot")
}

#[test]
fn manifest_redirect_uses_effective_base_uri() {
    let server = LocalServer::start(|index, _| match index {
        0 => response(
            "302 Found",
            &[("Location", "/nested/live/manifest.m3u8".to_owned())],
            b"",
        ),
        _ => response("200 OK", &[], b"#EXTM3U"),
    });
    let target = server.target("/entry");
    let mut fetcher = AdaptiveManifestFetcher::new(context_with_presentation(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
        MediaPresentation::Live,
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
    let segment_target = resource
        .base_uri()
        .resolve("../segment-7.ts")
        .expect("relative segment");
    assert!(
        segment_target
            .expose_secret_for_request()
            .ends_with("/nested/segment-7.ts")
    );
}

#[test]
fn live_manifest_refresh_fences_slow_stale_generation() {
    let server = LocalServer::start(|index, _| {
        if index == 0 {
            thread::sleep(Duration::from_millis(40));
            response("200 OK", &[], b"stale")
        } else {
            response("200 OK", &[], b"current")
        }
    });
    let target = server.target("/refresh");
    let mut fetcher = AdaptiveManifestFetcher::new(context_with_presentation(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
        MediaPresentation::Live,
    ))
    .expect("manifest fetcher");
    fetcher
        .request(
            ManifestFetchRequest::new(target.clone(), SourceGeneration::new(1)),
            Instant::now(),
        )
        .expect("initial request");
    assert!(matches!(
        fetcher.poll(Instant::now()),
        ManifestPoll::TemporarilyUnavailable { .. }
    ));
    let request_deadline = Instant::now() + TEST_TIMEOUT;
    while server.request_count() == 0 {
        assert!(
            Instant::now() < request_deadline,
            "initial request was not admitted"
        );
        thread::sleep(Duration::from_millis(1));
    }
    fetcher
        .request(
            ManifestFetchRequest::new(target.clone(), SourceGeneration::new(2)),
            Instant::now(),
        )
        .expect("new generation refresh");
    let stale_request = fetcher.request(
        ManifestFetchRequest::new(target, SourceGeneration::new(1)),
        Instant::now(),
    );
    assert!(matches!(
        stale_request,
        Err(AdaptiveTransportError::StaleGeneration { .. })
    ));
    let resource = poll_manifest_ready(&mut fetcher);
    assert_eq!(resource.generation(), SourceGeneration::new(2));
    assert_eq!(resource.bytes().as_ref(), b"current");
    assert_eq!(server.request_count(), 2);
}

#[test]
fn completed_manifest_generation_cannot_repeat_network_side_effect() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"manifest"));
    let target = server.target("/same-generation");
    let mut fetcher = AdaptiveManifestFetcher::new(context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("manifest fetcher");
    fetcher
        .request(
            ManifestFetchRequest::new(target.clone(), SourceGeneration::new(1)),
            Instant::now(),
        )
        .expect("initial generation");
    assert_eq!(
        poll_manifest_ready(&mut fetcher).bytes().as_ref(),
        b"manifest"
    );

    let repeated = fetcher.request(
        ManifestFetchRequest::new(target, SourceGeneration::new(1)),
        Instant::now(),
    );
    assert!(matches!(
        repeated,
        Err(AdaptiveTransportError::StaleGeneration { .. })
    ));
    assert_eq!(server.request_count(), 1);
}

#[test]
fn manifest_body_bound_fails_without_retaining_oversized_payload() {
    let oversized_body = vec![b'x'; 1025];
    let server = LocalServer::start(move |_, _| response("200 OK", &[], &oversized_body));
    let target = server.target("/oversized");
    let mut fetcher = AdaptiveManifestFetcher::new(context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("manifest fetcher");
    fetcher
        .request(
            ManifestFetchRequest::new(target, SourceGeneration::new(1)),
            Instant::now(),
        )
        .expect("manifest request");
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match fetcher.poll(Instant::now()) {
            ManifestPoll::TemporarilyUnavailable { retry_after } => {
                assert!(Instant::now() < deadline, "bounded manifest timed out");
                thread::sleep(retry_after.min(Duration::from_millis(5)));
            }
            ManifestPoll::Failed(AdaptiveTransportError::Source(
                source_core::SourceError::HttpBodyTooLarge { maximum_bytes, .. },
            )) => {
                assert_eq!(maximum_bytes, 1024);
                break;
            }
            other => panic!("oversized manifest must fail typed, got {other:?}"),
        }
    }
}

#[test]
fn exact_range_fetch_preserves_segment_boundary() {
    let server = LocalServer::start(|_, request| {
        assert!(
            request.headers.contains("range: bytes=2-5")
                || request.headers.contains("Range: bytes=2-5")
        );
        response(
            "206 Partial Content",
            &[("Content-Range", "bytes 2-5/8".to_owned())],
            b"cdef",
        )
    });
    let target = server.target("/media.bin");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    let descriptor = AdaptiveSegmentDescriptor::range(
        OrderedSegmentSequence::new(1),
        OrderedSegmentKind::Media,
        OrderedSegmentDiscontinuity::Continuous,
        target,
        SegmentByteRange::new(2, NonZeroUsize::new(4).expect("range length")).expect("range"),
    );
    source
        .install_snapshot(snapshot(
            1,
            descriptor,
            AdaptivePresentation::Vod { duration: None },
            clock(90_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");
    let SegmentPoll::Segment(segment) = poll_segment(&mut source) else {
        panic!("segment expected");
    };
    assert_eq!(segment.bytes.as_ref(), b"cdef");
    assert!(matches!(
        source.poll_next(Instant::now()),
        SegmentPoll::EndOfStream
    ));
}

#[test]
fn adaptive_media_410_publishes_generation_fenced_expiry_signal() {
    let server = LocalServer::start(|_, _| response("410 Gone", &[], &[]));
    let target = server.target("/expired-segment.m4s");
    let observer = Arc::new(RecordingExpiryObserver::default());
    let context = context_with_options(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        TestContextOptions {
            authorization: None,
            segment_query: None,
            key_query: None,
            presentation: MediaPresentation::Vod,
            endpoint_expiry_observer: Some(observer.clone()),
        },
    );
    let request = AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target,
        NonZeroUsize::new(64).expect("body bound"),
        AdaptiveResourcePurpose::MediaSegment,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    );

    let error = context
        .fetch_resource_blocking(request)
        .expect_err("expired adaptive endpoint must preserve source error");

    assert!(matches!(
        error,
        AdaptiveTransportError::Source(source_core::SourceError::HttpStatus { status, .. })
            if status.as_u16() == 410
    ));
    let signals = observer.recorded_signals();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].source_generation(), SourceGeneration::new(1));
    assert_eq!(
        signals[0].resource_kind(),
        EndpointExpiryResourceKind::MediaSegment
    );
    assert_eq!(signals[0].reason(), EndpointExpiryReason::ResourceExpired);
}

#[test]
fn exact_range_retries_transient_http_status() {
    let server = LocalServer::start(|index, _| {
        if index == 0 {
            response("503 Service Unavailable", &[], b"")
        } else {
            response(
                "206 Partial Content",
                &[("Content-Range", "bytes 2-5/8".to_owned())],
                b"cdef",
            )
        }
    });
    let target = server.target("/range-retry");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::range(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target,
                SegmentByteRange::new(2, NonZeroUsize::new(4).expect("range length"))
                    .expect("range"),
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(90_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");

    let SegmentPoll::Segment(segment) = poll_segment(&mut source) else {
        panic!("Range retry must recover");
    };
    assert_eq!(segment.bytes.as_ref(), b"cdef");
    assert_eq!(server.request_count(), 2);
}

#[test]
fn retry_reports_temporary_unavailable_then_recovers() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = Arc::clone(&attempts);
    let server = LocalServer::start(move |_, _| {
        if server_attempts.fetch_add(1, Ordering::AcqRel) == 0 {
            response("503 Service Unavailable", &[], b"")
        } else {
            response("200 OK", &[], b"segment")
        }
    });
    let target = server.target("/retry");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target,
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");
    assert!(matches!(
        source.poll_next(Instant::now()),
        SegmentPoll::TemporarilyUnavailable { .. }
    ));
    let SegmentPoll::Segment(segment) = poll_segment(&mut source) else {
        panic!("retry must recover");
    };
    assert_eq!(segment.bytes.as_ref(), b"segment");
    assert_eq!(attempts.load(Ordering::Acquire), 2);
}

#[test]
fn retry_budget_is_bounded_for_persistent_failure() {
    let server = LocalServer::start(|_, _| response("503 Service Unavailable", &[], b""));
    let target = server.target("/persistent-failure");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target,
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");
    let SegmentPoll::Failed(AdaptiveTransportError::Source(source_core::SourceError::HttpStatus {
        status,
        ..
    })) = poll_segment(&mut source)
    else {
        panic!("persistent failure must exhaust retry budget");
    };
    assert_eq!(status.as_u16(), 503);
    assert_eq!(server.request_count(), 3);
}

#[test]
fn cancellation_is_terminal_without_network_request() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"unused"));
    let target = server.target("/cancel");
    let cancellation = CancellationToken::new();
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        cancellation.clone(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target,
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");
    cancellation.cancel();
    assert!(matches!(
        source.poll_next(Instant::now()),
        SegmentPoll::Cancelled
    ));
    thread::sleep(Duration::from_millis(20));
    assert_eq!(server.request_count(), 0);
}

#[test]
fn cancellation_during_retry_backoff_prevents_follow_up_request() {
    let server = LocalServer::start(|_, _| {
        response(
            "503 Service Unavailable",
            &[("Retry-After", "1".to_owned())],
            b"retryable",
        )
    });
    let target = server.target("/cancel-backoff");
    let cancellation = CancellationToken::new();
    let mut adaptive_context = context(
        &target,
        cancellation.clone(),
        same_origin_redirects(),
        None,
        None,
    );
    adaptive_context.retry = AdaptiveRetryPolicy::new(
        NonZeroU8::new(2).expect("attempt budget"),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(2),
    )
    .expect("retry policy");
    let mut source = AdaptiveOrderedSegmentSource::new(adaptive_context).expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target,
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");
    assert!(matches!(
        source.poll_next(Instant::now()),
        SegmentPoll::TemporarilyUnavailable { .. }
    ));

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match source.poll_next(Instant::now()) {
            SegmentPoll::TemporarilyUnavailable { retry_after }
                if server.request_count() == 1 && retry_after >= Duration::from_millis(900) =>
            {
                break;
            }
            SegmentPoll::TemporarilyUnavailable { .. } => {
                assert!(Instant::now() < deadline, "retry backoff was not reached");
                thread::sleep(Duration::from_millis(1));
            }
            other => panic!("unexpected retry state before cancellation: {other:?}"),
        }
    }

    cancellation.cancel();
    assert!(matches!(
        source.poll_next(Instant::now()),
        SegmentPoll::Cancelled
    ));
    thread::sleep(Duration::from_millis(30));
    assert_eq!(server.request_count(), 1);
}

#[test]
fn stale_generation_is_rejected_and_live_refresh_replaces_metadata() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"x"));
    let target = server.target("/live");
    let mut source = AdaptiveOrderedSegmentSource::new(context_with_presentation(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
        MediaPresentation::Live,
    ))
    .expect("segment source");
    let first_window =
        DvrWindow::new(Duration::from_secs(10), Duration::from_secs(20)).expect("DVR");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target.clone(),
            ),
            AdaptivePresentation::Live {
                edge: LiveEdge::new(Duration::from_secs(20)),
                dvr: Some(first_window),
            },
            clock(90_000, 7),
            AdaptiveSegmentCompletion::AwaitRefresh,
        ))
        .expect("initial live snapshot");
    let stale = source.install_snapshot(snapshot(
        1,
        AdaptiveSegmentDescriptor::full(
            OrderedSegmentSequence::new(2),
            OrderedSegmentKind::Media,
            OrderedSegmentDiscontinuity::Continuous,
            target.clone(),
        ),
        AdaptivePresentation::Live {
            edge: LiveEdge::new(Duration::from_secs(21)),
            dvr: None,
        },
        clock(90_000, 8),
        AdaptiveSegmentCompletion::AwaitRefresh,
    ));
    assert_eq!(
        stale.expect_err("same generation is stale"),
        AdaptiveSegmentSnapshotError::NonAdvancingGeneration
    );
    let refreshed_window =
        DvrWindow::new(Duration::from_secs(12), Duration::from_secs(24)).expect("DVR");
    source
        .install_snapshot(snapshot(
            2,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(3),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::StartsNewTimeline,
                target,
            ),
            AdaptivePresentation::Live {
                edge: LiveEdge::new(Duration::from_secs(24)),
                dvr: Some(refreshed_window),
            },
            clock(48_000, 11),
            AdaptiveSegmentCompletion::AwaitRefresh,
        ))
        .expect("new generation refresh");
    assert_eq!(
        source.component_clock(),
        Some(clock(48_000, 11)),
        "audio/video components keep independent clocks"
    );
    assert_eq!(
        source.presentation(),
        Some(AdaptivePresentation::Live {
            edge: LiveEdge::new(Duration::from_secs(24)),
            dvr: Some(refreshed_window),
        })
    );
}

#[test]
fn overlapping_live_refresh_never_redelivers_completed_segment() {
    let server = LocalServer::start(|index, _| {
        if index == 0 {
            response("200 OK", &[], b"first")
        } else {
            response("200 OK", &[], b"second")
        }
    });
    let target = server.target("/overlap");
    let mut source = AdaptiveOrderedSegmentSource::new(context_with_presentation(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
        MediaPresentation::Live,
    ))
    .expect("segment source");
    let live = AdaptivePresentation::Live {
        edge: LiveEdge::new(Duration::from_secs(10)),
        dvr: None,
    };
    let first_descriptor = AdaptiveSegmentDescriptor::full(
        OrderedSegmentSequence::new(10),
        OrderedSegmentKind::Media,
        OrderedSegmentDiscontinuity::Continuous,
        target.clone(),
    );
    source
        .install_snapshot(snapshot(
            1,
            first_descriptor,
            live,
            clock(1_000, 0),
            AdaptiveSegmentCompletion::AwaitRefresh,
        ))
        .expect("initial snapshot");
    let SegmentPoll::Segment(first) = poll_segment(&mut source) else {
        panic!("first segment expected");
    };
    assert_eq!(first.sequence, OrderedSegmentSequence::new(10));

    let refreshed = AdaptiveSegmentSnapshot::new(
        SourceGeneration::new(2),
        live,
        clock(1_000, 0),
        vec![
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(10),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target.clone(),
            ),
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(11),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target,
            ),
        ],
        AdaptiveSegmentCompletion::EndAfterSnapshot,
    )
    .expect("overlapping refresh");
    source.install_snapshot(refreshed).expect("new generation");

    let SegmentPoll::Segment(second) = poll_segment(&mut source) else {
        panic!("second segment expected");
    };
    assert_eq!(second.sequence, OrderedSegmentSequence::new(11));
    assert_eq!(second.bytes.as_ref(), b"second");
    assert_eq!(server.request_count(), 2);
    assert!(matches!(
        source.poll_next(Instant::now()),
        SegmentPoll::EndOfStream
    ));
}

#[test]
fn cross_origin_segment_redirect_strips_header_and_query_secret() {
    let final_server = LocalServer::start(|_, _| response("200 OK", &[], b"safe"));
    let final_target = final_server.target("/final");
    let redirect_location = final_target.expose_secret_for_request().to_owned();
    let initial_server = LocalServer::start(move |_, _| {
        response("302 Found", &[("Location", redirect_location.clone())], b"")
    });
    let initial_target = initial_server.target("/segment");
    let redirects = RedirectPolicy::cross_origin_without_secrets(
        RedirectHopLimit::new(4).expect("redirect limit"),
    );
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &initial_target,
        CancellationToken::new(),
        redirects,
        Some("Bearer very-secret"),
        Some("token=very-secret"),
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                initial_target,
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");
    assert!(matches!(poll_segment(&mut source), SegmentPoll::Segment(_)));
    let initial = initial_server.requests();
    assert!(initial[0].headers.contains("very-secret"));
    assert!(initial[0].request_line.contains("token=very-secret"));
    let final_requests = final_server.requests();
    assert!(!final_requests[0].headers.contains("very-secret"));
    assert!(!final_requests[0].request_line.contains("very-secret"));
}

#[test]
fn out_of_scope_cdn_segment_uses_suppress_and_omits_secrets() {
    let scoped_server = LocalServer::start(|_, _| response("200 OK", &[], b"unused"));
    let foreign_server = LocalServer::start(|_, _| response("200 OK", &[], b"cdn-segment"));
    let scoped_target = scoped_server.target("/playlist.m3u8");
    let foreign_target = foreign_server.target("/segment1.ts");
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &scoped_target,
        CancellationToken::new(),
        RedirectPolicy::cross_origin_without_secrets(
            RedirectHopLimit::new(4).expect("redirect limit"),
        ),
        Some("Bearer segment-secret"),
        Some("token=segment-secret"),
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                foreign_target,
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");

    let SegmentPoll::Segment(segment) = poll_segment(&mut source) else {
        panic!("CDN segment expected via Suppress");
    };
    assert_eq!(segment.bytes.as_ref(), b"cdn-segment");
    let requests = foreign_server.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains("segment-secret"));
    assert!(!requests[0].request_line.contains("segment-secret"));
}

struct EndDemuxer;

impl Demuxer for EndDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        None
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, _timestamp: Duration) -> Result<DemuxSeekResult> {
        unreachable!("test demuxer is not seekable")
    }
}

#[test]
fn deferred_adapter_keeps_registry_prefetch_off_player_owner() {
    let server = LocalServer::start(|_, _| {
        thread::sleep(Duration::from_millis(80));
        response("200 OK", &[], b"initial-segment")
    });
    let target = server.target("/slow");
    let cancellation = CancellationToken::new();
    let mut source = AdaptiveOrderedSegmentSource::new(context(
        &target,
        cancellation.clone(),
        same_origin_redirects(),
        None,
        None,
    ))
    .expect("segment source");
    source
        .install_snapshot(snapshot(
            1,
            AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(1),
                OrderedSegmentKind::Media,
                OrderedSegmentDiscontinuity::Continuous,
                target,
            ),
            AdaptivePresentation::Vod { duration: None },
            clock(1_000, 0),
            AdaptiveSegmentCompletion::EndAfterSnapshot,
        ))
        .expect("snapshot");
    let mut demuxer = BlockingOrderedSegmentAdapter::open_deferred(
        source,
        cancellation,
        ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(4).expect("events"),
            NonZeroUsize::new(1024).expect("bytes"),
        ),
        DemuxRetryHint::new(Duration::from_millis(2)).expect("retry hint"),
        |mut ordered_source| {
            let initial = ordered_source
                .next_segment(&CancellationToken::new())?
                .expect("initial segment");
            assert_eq!(initial.bytes.as_ref(), b"initial-segment");
            Ok(Box::new(EndDemuxer))
        },
    )
    .expect("deferred demuxer");
    assert!(matches!(
        demuxer.next_event().expect("poll"),
        DemuxReadEvent::TemporarilyUnavailable(_)
    ));
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event().expect("poll deferred demuxer") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "deferred open timed out");
                thread::sleep(Duration::from_millis(3));
            }
            DemuxReadEvent::TracksChanged(_) => break,
            other => panic!("tracks must be first ready event, got {other:?}"),
        }
    }
}
