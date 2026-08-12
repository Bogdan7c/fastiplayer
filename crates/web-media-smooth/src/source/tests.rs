//! Focused P3B tests на checked-in canonical Smooth PIFF corpus.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bounded_xml_reader::XmlBudgets;
use demux_api::{
    OrderedSegmentKind, OrderedSegmentReadError, OrderedSegmentSource,
    PresentationWindowOrderedSegment, PresentationWindowOrderedSegmentReadOutcome,
    PresentationWindowOrderedSegmentSource,
};
use media_core::PacketPresentationWindow;
use rustiplayer_config::NetworkConfig;
use smooth_streaming_fmp4::{
    SmoothFragmentIndex, SmoothFragmentPlanRequest, SmoothFragmentReconstructionRequest,
    SmoothReconstructedFragment, SmoothStreamOrdinal, SmoothTrackMappingRequest,
    SmoothTrackSelection, map_smooth_track, plan_smooth_fragment, reconstruct_smooth_fragment,
};
use smooth_streaming_manifest_core::{SmoothQualityLevel, SmoothStreamKind};
use source_core::{
    CancellationToken, HttpHeader, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig,
    ValidatedHttpHeaders,
};
use symphonia_format_isomp4::{
    FragmentInitializationLimits, FragmentInspectionLimits, FragmentWriteLimits,
};
use web_media_adaptive::{AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalog,
    ComponentVariantCatalogEntries, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogLimit, ComponentVariantSelectionRequest, ExtractionGeneration,
    PreferredHeightPolicy, SemanticIdentity, SourceIdentity,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

use crate::{
    AggregateInitializationByteLimit, SmoothFragmentSourceBuildError, SmoothFragmentSourcePolicy,
    SmoothPreparationPolicy, SmoothPrepareRequest,
};

/// Полный canonical manifest используется напрямую из единственного checked-in corpus.
const MANIFEST: &str =
    include_str!("../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/tears-of-steel.ismc");
/// Первый low-video fragment canonical corpus.
const VIDEO_LOW_FIRST: &[u8] = include_bytes!(
    "../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-401000-0.bin"
);
/// Первый high-video fragment canonical corpus.
const VIDEO_HIGH_FIRST: &[u8] = include_bytes!(
    "../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-1501000-0.bin"
);
/// Второй high-video fragment canonical corpus.
const VIDEO_HIGH_SECOND: &[u8] = include_bytes!(
    "../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-1501000-40000000.bin"
);
/// Первый audio fragment canonical corpus.
const AUDIO_FIRST: &[u8] =
    include_bytes!("../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-0.bin");
/// Второй audio fragment canonical corpus.
const AUDIO_SECOND: &[u8] = include_bytes!(
    "../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-39680000.bin"
);
/// Canonical high-video first request target.
pub(crate) const VIDEO_HIGH_FIRST_PATH: &str =
    "/media/QualityLevels(1501000)/Fragments(video_eng=0)";
/// Canonical audio second request target.
const AUDIO_SECOND_PATH: &str = "/media/QualityLevels(64008)/Fragments(audio_eng=39680000)";

/// Test-only exact response replacement для одного canonical path.
struct FixtureResponseOverride {
    request_target: &'static str,
    body: Vec<u8>,
}

/// Управляемый loopback origin с журналом запросов.
pub(crate) struct FixtureOrigin {
    target: HttpRequestTarget,
    exact_target: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    worker: Option<thread::JoinHandle<()>>,
}

impl FixtureOrigin {
    /// Запускает origin, который отдаёт canonical manifest и доступные corpus fragments.
    pub(crate) fn start() -> Self {
        Self::start_with_override(None)
    }

    /// Запускает тот же origin с одним explicit fragment override.
    pub(crate) fn start_with_fragment(request_target: &'static str, body: Vec<u8>) -> Self {
        Self::start_with_override(Some(FixtureResponseOverride {
            request_target,
            body,
        }))
    }

    /// Общий constructor loopback origin.
    fn start_with_override(response_override: Option<FixtureResponseOverride>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener
            .set_nonblocking(true)
            .expect("fixture listener nonblocking");
        let address = listener.local_addr().expect("fixture address");
        let exact_target = format!("http://{address}/media/tears-of-steel.ismc");
        let target =
            HttpRequestTarget::parse_exact(exact_target.clone()).expect("fixture manifest target");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_requests = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream);
                        worker_requests
                            .lock()
                            .expect("request journal")
                            .push(request.clone());
                        let request_target = request_target(&request);
                        let response = response_override
                            .as_ref()
                            .filter(|replacement| replacement.request_target == request_target)
                            .map_or_else(
                                || response_for(&request),
                                |replacement| (200, replacement.body.as_slice()),
                            );
                        write_response(&mut stream, response);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fixture origin timeout");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        });
        Self {
            target,
            exact_target,
            requests,
            stop,
            address,
            worker: Some(worker),
        }
    }

    /// Возвращает manifest target.
    pub(crate) fn target(&self) -> &HttpRequestTarget {
        &self.target
    }

    /// Возвращает exact loopback URL только внутри test transport.
    fn exact_target(&self) -> &str {
        &self.exact_target
    }

    /// Возвращает число принятых HTTP requests.
    pub(crate) fn request_count(&self) -> usize {
        self.requests.lock().expect("request journal").len()
    }

    /// Возвращает request targets в порядке приёма.
    pub(crate) fn request_targets(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("request journal")
            .iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .expect("HTTP request line")
                    .split_whitespace()
                    .nth(1)
                    .expect("HTTP request target")
                    .to_owned()
            })
            .collect()
    }
}

impl Drop for FixtureOrigin {
    /// Останавливает worker через explicit flag и loopback wake-up.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("fixture worker");
        }
    }
}

/// Читает только HTTP headers с bounded test buffer.
fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("fixture read timeout");
    let mut request = Vec::with_capacity(1_024);
    let mut chunk = [0_u8; 512];
    while request.len() < 16 * 1_024 {
        let bytes_read = stream.read(&mut chunk).expect("read fixture request");
        if bytes_read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..bytes_read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).expect("ASCII fixture request")
}

/// Выбирает canonical response по exact path.
fn response_for(request: &str) -> (u16, &'static [u8]) {
    match request_target(request) {
        "/media/tears-of-steel.ismc" => (200, MANIFEST.as_bytes()),
        "/media/QualityLevels(401000)/Fragments(video_eng=0)" => (200, VIDEO_LOW_FIRST),
        VIDEO_HIGH_FIRST_PATH => (200, VIDEO_HIGH_FIRST),
        "/media/QualityLevels(1501000)/Fragments(video_eng=40000000)" => (200, VIDEO_HIGH_SECOND),
        "/media/QualityLevels(64008)/Fragments(audio_eng=0)" => (200, AUDIO_FIRST),
        AUDIO_SECOND_PATH => (200, AUDIO_SECOND),
        _ => (404, b"missing canonical fixture"),
    }
}

/// Извлекает exact request target из bounded test request.
fn request_target(request: &str) -> &str {
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("HTTP request target")
}

/// Пишет конечный HTTP/1.1 response без keep-alive.
fn write_response(stream: &mut TcpStream, response: (u16, &[u8])) {
    let (status, body) = response;
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("write fixture headers");
    stream.write_all(body).expect("write fixture body");
}

/// Собирает transport request без секретов и скрытых redirect semantics.
pub(crate) fn transport_request(target: &HttpRequestTarget) -> TransportOpenRequest {
    transport_request_with_security(
        target,
        RedirectPolicy::same_origin(RedirectHopLimit::new(2).expect("redirect budget")),
        false,
    )
}

/// Собирает transport request с explicit redirect и secret policy.
fn transport_request_with_security(
    target: &HttpRequestTarget,
    redirects: RedirectPolicy,
    include_secret: bool,
) -> TransportOpenRequest {
    let source = SourceIdentity::new(91);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(3),
        CandidateFormatIdentity::new("smooth-p3b").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "smooth-p3b").expect("semantic identity");
    let component =
        MediaComponentIdentity::new(exact, semantic, MediaComponentRole::PresentationManifest)
            .expect("presentation manifest component");
    let scope =
        SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("root path scope"));
    let mut secrets = SecretRequestContext::builder(scope);
    if include_secret {
        secrets = secrets.with_headers(
            ValidatedHttpHeaders::new(vec![HttpHeader::new(
                "authorization",
                "Bearer p3b-do-not-leak",
            )])
            .expect("secret header"),
        );
        secrets = secrets
            .with_serialized_cookies("p3b_session=p3b-cookie-secret")
            .expect("serialized secret cookie");
    }
    TransportOpenRequest::new(
        TransportProviderId::new("smooth-p3b-fixture").expect("provider id"),
        component,
        target.clone(),
        MediaPresentation::Vod,
        SourceGeneration::new(17),
        secrets.build(),
        redirects,
        CancellationToken::new(),
    )
    .expect("transport request")
}

/// Обслуживает один initial manifest redirect и возвращает raw request.
fn serve_redirect_once(listener: TcpListener, location: String) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("redirect accept");
        let request = read_request(&mut stream);
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(response.as_bytes())
            .expect("write redirect response");
        request
    })
}

/// Caller-owned preparation budgets, достаточные для полного canonical manifest.
pub(crate) fn preparation_policy() -> SmoothPreparationPolicy {
    preparation_policy_with_segment_limit(128 * 1_024)
}

/// Preparation policy с caller-controlled retained media body bound.
fn preparation_policy_with_segment_limit(maximum_segment_bytes: usize) -> SmoothPreparationPolicy {
    SmoothPreparationPolicy::new(
        AdaptiveTransportLimits::new(
            NonZeroUsize::new(32 * 1_024).expect("manifest bytes"),
            NonZeroUsize::new(maximum_segment_bytes).expect("segment bytes"),
            NonZeroUsize::new(64).expect("snapshot bytes"),
        ),
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(1).expect("retry attempts"),
            Duration::from_millis(1),
            Duration::from_millis(2),
        )
        .expect("retry policy"),
        XmlBudgets::builder()
            .maximum_document_bytes(32 * 1_024)
            .maximum_depth(16)
            .maximum_tokens(8_192)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(4_096)
            .maximum_attribute_bytes(64 * 1_024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(32)
            .maximum_namespace_bytes(4 * 1_024)
            .maximum_text_bytes(32 * 1_024)
            .build()
            .expect("XML budgets"),
        smooth_streaming_manifest_core::SmoothManifestLimits::builder()
            .maximum_streams(8)
            .maximum_qualities_per_stream(16)
            .maximum_total_qualities(32)
            .maximum_timeline_entries_per_stream(256)
            .maximum_total_timeline_entries(512)
            .maximum_fragments_per_stream(1_024)
            .maximum_total_fragments(2_048)
            .maximum_template_bytes(512)
            .maximum_string_bytes(256)
            .maximum_codec_bytes(4_096)
            .maximum_custom_attributes_per_quality(8)
            .maximum_total_custom_attributes(32)
            .maximum_custom_attribute_name_bytes(64)
            .maximum_custom_attribute_value_bytes(128)
            .build()
            .expect("manifest budgets"),
        FragmentInitializationLimits::builder()
            .maximum_output_bytes(32 * 1_024)
            .maximum_codec_configuration_bytes(8 * 1_024)
            .build()
            .expect("initialization budgets"),
        AggregateInitializationByteLimit::new(
            NonZeroUsize::new(256 * 1_024).expect("aggregate init bytes"),
        ),
        ComponentVariantCatalogLimit::new(64).expect("catalog budget"),
        web_media_core::ComponentVariantEdgeLimit::new(1_024).expect("compatibility edge budget"),
    )
}

/// Mandatory reconstruction budgets.
pub(crate) fn fragment_policy() -> SmoothFragmentSourcePolicy {
    fragment_policy_with_limits(128 * 1_024, 128 * 1_024)
}

/// Reconstruction policy с explicit input budget для limit tests.
fn fragment_policy_with_max_input(max_input_bytes: usize) -> SmoothFragmentSourcePolicy {
    fragment_policy_with_limits(max_input_bytes, 128 * 1_024)
}

/// Reconstruction policy с independent inspection и write bounds.
fn fragment_policy_with_limits(
    max_input_bytes: usize,
    maximum_output_bytes: usize,
) -> SmoothFragmentSourcePolicy {
    let inspection = FragmentInspectionLimits::builder()
        .max_input_bytes(max_input_bytes)
        .max_box_count(128)
        .max_box_depth(8)
        .max_traf_count(1)
        .max_trun_count(8)
        .max_samples(4_096)
        .max_sample_table_bytes(64 * 1_024)
        .max_box_payload_bytes(128 * 1_024)
        .build()
        .expect("inspection budgets");
    let write =
        FragmentWriteLimits::try_new(maximum_output_bytes).expect("non-zero fragment write budget");
    SmoothFragmentSourcePolicy::new(inspection, write)
}

/// Готовит catalog единственным manifest fetch.
pub(crate) fn prepare(origin: &FixtureOrigin) -> crate::SmoothPreparedCatalog {
    prepare_with_generation(origin, 44)
}

/// Готовит catalog с caller-controlled generation для stale-selection tests.
fn prepare_with_generation(
    origin: &FixtureOrigin,
    catalog_generation: u64,
) -> crate::SmoothPreparedCatalog {
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    crate::prepare::prepare_smooth_vod_all_for_test(SmoothPrepareRequest::new(
        transport_request(origin.target()),
        &source_config,
        ComponentVariantCatalogGeneration::new(catalog_generation),
        PreferredHeightPolicy::NoPreference,
        preparation_policy(),
    ))
    .expect("canonical Smooth preparation")
}

/// Выбирает exact VideoAndAudio pair по descriptor bitrate, а не storage order.
pub(crate) fn selection(
    prepared: &crate::SmoothPreparedCatalog,
    video_bitrate: u64,
) -> web_media_core::ComponentVariantSelection {
    let video = prepared
        .catalog()
        .required_video_variants()
        .expect("video axis")
        .iter()
        .find(|variant| {
            variant
                .track()
                .bitrate()
                .is_some_and(|bitrate| bitrate.bits_per_second() == video_bitrate)
        })
        .expect("video bitrate")
        .exact_identity()
        .clone();
    let audio = prepared
        .catalog()
        .required_audio_variants()
        .expect("audio axis")
        .iter()
        .find(|variant| {
            variant
                .track()
                .bitrate()
                .is_some_and(|bitrate| bitrate.bits_per_second() == 64_008)
        })
        .expect("64 kbps audio variant")
        .exact_identity()
        .clone();
    prepared
        .catalog()
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio { video, audio })
        .expect("exact selection")
}

mod cases;
