use std::io::{Read, Write};
use std::net::TcpListener;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashContainer, DashMediaKind, DashMpdLimits, DashMpdParseRequest, parse_dash_mpd,
};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxRegistry, DemuxSniffBudget, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};
use media_core::{DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, Demuxer};
use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig, ValidatedHttpHeaders,
};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveRetryPolicy, AdaptiveTransportError,
    AdaptiveTransportLimits,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit, ComponentVariantEdgeLimit,
    ExactSelectionIdentity, ExtractionGeneration, PreferredHeightPolicy, SemanticIdentity,
    SourceIdentity,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

use crate::plan::{
    DashPeriodInputPlan, DashPlanError, DashPresentationPlan, build_manifest_plan,
    build_serialized_plan,
};
use crate::{
    DashFetchedManifestInput, DashManifestInput, DashPresentationSelection,
    DashRepresentationCapabilityProbe, DashRepresentationCapabilityRejection,
    DashRepresentationEvidence, DashRepresentationSelectionError, DashResourceReference,
    DashSerializedComponent, DashSerializedFragment, DashSerializedFragmentKind,
    DashSerializedPresentation, DashVideoDimensions, DashVodCatalogDiscoveryRequest,
    DashVodHttpContext, DashVodInput, DashVodOpenError, DashVodOpenPolicy, DashVodOpenRequest,
    NativeDashVodCatalogDiscoveryRequest, discover_dash_vod_catalog,
    discover_native_dash_vod_catalog, prepare_dash_vod, prepare_discovered_dash_vod,
    prepare_discovered_dash_vod_semantic,
};

struct AcceptAllDashCapabilities;

impl DashRepresentationCapabilityProbe for AcceptAllDashCapabilities {
    fn check_video(
        &self,
        _video: &media_core::TrackInfo,
    ) -> Result<(), DashRepresentationCapabilityRejection> {
        Ok(())
    }

    fn check_audio(
        &self,
        _audio: &media_core::TrackInfo,
    ) -> Result<(), DashRepresentationCapabilityRejection> {
        Ok(())
    }

    fn check_muxed(
        &self,
        _video: &media_core::TrackInfo,
        _audio: &media_core::TrackInfo,
    ) -> Result<(), DashRepresentationCapabilityRejection> {
        Ok(())
    }
}

fn parse(document: &str) -> dash_mpd_core::DashMpd {
    parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(64 * 1024)
            .maximum_depth(32)
            .maximum_tokens(1_024)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(512)
            .maximum_attribute_bytes(32 * 1024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(32)
            .maximum_namespace_bytes(4 * 1024)
            .maximum_text_bytes(32 * 1024)
            .build()
            .expect("complete XML budgets"),
        limits: DashMpdLimits {
            maximum_periods: 8,
            maximum_adaptation_sets_per_period: 8,
            maximum_representations_per_adaptation_set: 16,
            maximum_segments_per_list: 64,
            maximum_timeline_entries: 64,
            maximum_schema_string_bytes: 4 * 1024,
        },
    })
    .expect("valid test MPD")
}

fn evidence(
    media_kind: DashMediaKind,
    container: DashContainer,
    representation_id: Option<&str>,
) -> DashRepresentationEvidence {
    DashRepresentationEvidence {
        media_kind,
        container,
        representation_id: representation_id.map(str::to_owned),
        codecs: None,
        bandwidth: None,
        dimensions: None,
    }
}

fn base() -> HttpRequestTarget {
    HttpRequestTarget::parse_exact("https://media.example/root/manifest.mpd")
        .expect("valid test target")
}

struct FixtureServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    manifest_requests: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}

/// HTTP fixture с независимой обработкой соединений, чтобы тест видел именно
/// provider concurrency, а не случайную последовательность accept loop-а.
struct ParallelCatalogFixtureServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    maximum_active_initializations: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}

/// HTTP fixture обслуживает MPD и только bounded Range reads media resource-а.
struct RangeFixtureServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RangeFixtureServer {
    fn start(manifest: String, media_resource: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Range fixture server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking Range fixture server");
        let address = listener.local_addr().expect("Range fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request_bytes = [0_u8; 8 * 1024];
                        let read = stream
                            .read(&mut request_bytes)
                            .expect("read Range fixture request");
                        let request = String::from_utf8_lossy(&request_bytes[..read]);
                        if request.starts_with("GET /manifest.mpd ") {
                            let mut response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                manifest.len()
                            )
                            .into_bytes();
                            response.extend_from_slice(manifest.as_bytes());
                            stream
                                .write_all(&response)
                                .expect("write manifest response");
                            continue;
                        }
                        assert!(
                            request.starts_with("GET /video.mp4 "),
                            "unexpected Range fixture request"
                        );
                        let range_line = request
                            .lines()
                            .find(|line| line.to_ascii_lowercase().starts_with("range: bytes="))
                            .expect("media request must be bounded Range");
                        let range = range_line
                            .split_once('=')
                            .map(|(_, range)| range)
                            .expect("Range separator");
                        let (start, requested_end) = range.split_once('-').expect("closed Range");
                        let start = start.parse::<usize>().expect("Range start");
                        let requested_end = requested_end.parse::<usize>().expect("Range end");
                        let end = requested_end.min(media_resource.len().saturating_sub(1));
                        let body = &media_resource[start..=end];
                        let mut response = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: \"dash-range-v1\"\r\nConnection: close\r\n\r\n",
                            body.len(),
                            media_resource.len()
                        )
                        .into_bytes();
                        response.extend_from_slice(body);
                        stream.write_all(&response).expect("write Range response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("Range fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }

    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("Range fixture target")
    }
}

impl Drop for RangeFixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("Range fixture server joins");
        }
    }
}

impl FixtureServer {
    fn start(initialization: Vec<u8>, first: Vec<u8>, second: Vec<u8>) -> Self {
        Self::start_internal(None, initialization, first, second)
    }

    fn start_with_manifest(
        manifest: String,
        initialization: Vec<u8>,
        first: Vec<u8>,
        second: Vec<u8>,
    ) -> Self {
        Self::start_internal(Some(manifest), initialization, first, second)
    }

    fn start_internal(
        manifest: Option<String>,
        initialization: Vec<u8>,
        first: Vec<u8>,
        second: Vec<u8>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fixture server");
        let address = listener.local_addr().expect("fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let manifest_requests = Arc::new(AtomicUsize::new(0));
        let worker_manifest_requests = Arc::clone(&manifest_requests);
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 4 * 1024];
                        let read = stream.read(&mut request).expect("read fixture request");
                        let request = String::from_utf8_lossy(&request[..read]);
                        let body = if request.starts_with("GET /manifest.mpd ") {
                            worker_manifest_requests.fetch_add(1, Ordering::Relaxed);
                            manifest
                                .as_ref()
                                .expect("manifest fixture configured")
                                .as_bytes()
                        } else if request.starts_with("GET /init.mp4 ")
                            || request.starts_with("GET /init.webm ")
                        {
                            &initialization
                        } else if request.starts_with("GET /one.m4s ")
                            || request.starts_with("GET /one.webm ")
                        {
                            &first
                        } else if request.starts_with("GET /two.m4s ")
                            || request.starts_with("GET /two.webm ")
                        {
                            &second
                        } else {
                            panic!("unexpected fixture request line")
                        };
                        let mut response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        response.extend_from_slice(body);
                        stream.write_all(&response).expect("write fixture response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            manifest_requests,
            thread: Some(thread),
        }
    }

    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("fixture target")
    }

    fn manifest_request_count(&self) -> usize {
        self.manifest_requests.load(Ordering::Relaxed)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::net::TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fixture server joins");
        }
    }
}

impl ParallelCatalogFixtureServer {
    /// Поднимает manifest с четырьмя fully playable muxed lanes и задерживает
    /// каждый независимый initialization response на одинаковое время.
    fn start(
        manifest: String,
        initialization: Vec<u8>,
        first: Vec<u8>,
        second: Vec<u8>,
        initialization_delay: Duration,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind parallel DASH fixture");
        listener
            .set_nonblocking(true)
            .expect("nonblocking parallel DASH fixture");
        let address = listener.local_addr().expect("parallel fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let maximum_active_initializations = Arc::new(AtomicUsize::new(0));
        let worker_maximum_active = Arc::clone(&maximum_active_initializations);
        let manifest = Arc::new(manifest.into_bytes());
        let initialization = Arc::new(initialization);
        let first = Arc::new(first);
        let second = Arc::new(second);
        let thread = thread::spawn(move || {
            let active_initializations = Arc::new(AtomicUsize::new(0));
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let connection_manifest = Arc::clone(&manifest);
                        let connection_initialization = Arc::clone(&initialization);
                        let connection_first = Arc::clone(&first);
                        let connection_second = Arc::clone(&second);
                        let connection_active = Arc::clone(&active_initializations);
                        let connection_maximum_active = Arc::clone(&worker_maximum_active);
                        thread::spawn(move || {
                            let mut request_bytes = [0_u8; 4 * 1_024];
                            let read = stream
                                .read(&mut request_bytes)
                                .expect("read parallel DASH fixture request");
                            let request = String::from_utf8_lossy(&request_bytes[..read]);
                            let is_initialization = request.starts_with("GET /init-");
                            let body = if request.starts_with("GET /manifest.mpd ") {
                                connection_manifest
                            } else if is_initialization {
                                connection_initialization
                            } else if request.starts_with("GET /one-") {
                                connection_first
                            } else if request.starts_with("GET /two-") {
                                connection_second
                            } else {
                                panic!("unexpected parallel DASH fixture request line")
                            };
                            if is_initialization {
                                let active = connection_active.fetch_add(1, Ordering::AcqRel) + 1;
                                connection_maximum_active.fetch_max(active, Ordering::AcqRel);
                                thread::sleep(initialization_delay);
                                connection_active.fetch_sub(1, Ordering::AcqRel);
                            }
                            let mut response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            response.extend_from_slice(&body);
                            stream
                                .write_all(&response)
                                .expect("write parallel DASH fixture response");
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("parallel DASH fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            maximum_active_initializations,
            thread: Some(thread),
        }
    }

    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("parallel DASH fixture target")
    }

    fn maximum_active_initializations(&self) -> usize {
        self.maximum_active_initializations.load(Ordering::Acquire)
    }
}

impl Drop for ParallelCatalogFixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("parallel DASH fixture stops");
        }
    }
}

fn adaptive_context(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    generation: SourceGeneration,
) -> AdaptiveHttpContext {
    let source = SourceIdentity::new(91);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(generation.value()),
        CandidateFormatIdentity::new("dash-runtime-test").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "dash-runtime-test").expect("semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("component identity");
    let scope =
        SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("root path scope"));
    let secrets = SecretRequestContext::builder(scope)
        .with_headers(ValidatedHttpHeaders::new(Vec::new()).expect("empty headers"))
        .build();
    let request = TransportOpenRequest::new(
        TransportProviderId::new("dash-runtime-test").expect("provider id"),
        component,
        target.clone(),
        MediaPresentation::Vod,
        generation,
        secrets,
        RedirectPolicy::same_origin(RedirectHopLimit::new(4).expect("redirect hop limit")),
        cancellation,
    )
    .expect("transport request");
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    AdaptiveHttpContext::new(
        request,
        &source_config,
        AdaptiveTransportLimits::new(
            NonZeroUsize::new(64 * 1_024).expect("manifest bound"),
            NonZeroUsize::new(256 * 1_024).expect("resource bound"),
            NonZeroUsize::new(64).expect("descriptor bound"),
        ),
        AdaptiveRetryPolicy::new(
            std::num::NonZeroU8::new(2).expect("attempts"),
            Duration::from_millis(5),
            Duration::from_millis(10),
            Duration::from_millis(10),
        )
        .expect("retry policy"),
    )
    .expect("adaptive DASH context")
}

fn demux_registry() -> Arc<DemuxRegistry> {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("Symphonia factory"),
        ))
        .expect("register Symphonia");
    Arc::new(registry)
}

fn open_policy() -> DashVodOpenPolicy {
    DashVodOpenPolicy {
        maximum_manifest_bytes: NonZeroUsize::new(64 * 1_024).expect("manifest bytes"),
        maximum_fragment_bytes: NonZeroUsize::new(256 * 1_024).expect("fragment bytes"),
        maximum_range_read_bytes: NonZeroUsize::new(16 * 1_024).expect("Range bytes"),
        maximum_cached_range_pages: NonZeroUsize::new(2).expect("cached Range pages"),
        maximum_planned_segments: NonZeroUsize::new(64).expect("segments"),
        maximum_parallel_catalog_probes: NonZeroUsize::new(4).expect("parallel catalog probes"),
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(8 * 1_024).expect("sniff bytes"),
            NonZeroUsize::new(4).expect("sniff segments"),
            Duration::from_secs(1),
        )
        .expect("sniff budget"),
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(64).expect("events"),
            NonZeroUsize::new(512 * 1_024).expect("packets"),
        ),
        asynchronous_seek_limits: ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(4).expect("seek receipts"),
        ),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(5)).expect("retry hint"),
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(2),
            NonZeroUsize::new(256 * 1_024).expect("composite bytes"),
        )
        .expect("lead policy"),
        maximum_seek_scan_events: NonZeroUsize::new(4_096).expect("scan events"),
        maximum_seek_scan_bytes: NonZeroUsize::new(8 * 1_024 * 1_024).expect("scan bytes"),
    }
}

fn manifest_input(target: HttpRequestTarget) -> DashManifestInput {
    DashManifestInput {
        target,
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(64 * 1024)
            .maximum_depth(32)
            .maximum_tokens(1_024)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(512)
            .maximum_attribute_bytes(32 * 1024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(32)
            .maximum_namespace_bytes(4 * 1024)
            .maximum_text_bytes(32 * 1024)
            .build()
            .expect("complete XML budgets"),
        mpd_limits: DashMpdLimits {
            maximum_periods: 8,
            maximum_adaptation_sets_per_period: 8,
            maximum_representations_per_adaptation_set: 16,
            maximum_segments_per_list: 64,
            maximum_timeline_entries: 64,
            maximum_schema_string_bytes: 4 * 1024,
        },
    }
}

fn catalog_identity(generation: u64) -> ComponentVariantCatalogIdentity {
    let source = SourceIdentity::new(91);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("dash-runtime-test").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "dash-runtime-test").expect("semantic identity");
    ComponentVariantCatalogIdentity::new(
        ExactSelectionIdentity::new(exact, semantic).expect("same source identity"),
        ComponentVariantCatalogGeneration::new(generation),
    )
}

fn discovered_vod_request(
    server: &FixtureServer,
    generation: SourceGeneration,
    catalog_generation: u64,
) -> DashVodCatalogDiscoveryRequest<'static> {
    let manifest_target = server.target("/manifest.mpd");
    DashVodCatalogDiscoveryRequest {
        open: DashVodOpenRequest {
            http: DashVodHttpContext::Manifest(Box::new(adaptive_context(
                &manifest_target,
                CancellationToken::new(),
                generation,
            ))),
            generation,
            input: DashVodInput::Manifest(manifest_input(manifest_target)),
            selection: DashPresentationSelection::Single {
                main: evidence(DashMediaKind::Muxed, DashContainer::IsoBmff, Some("muxed")),
            },
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        catalog_identity: catalog_identity(catalog_generation),
        catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
        compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
        capability_probe: &AcceptAllDashCapabilities,
    }
}

fn muxed_fmp4() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let bytes = decode_base64(include_str!(
        "../../web-media-hls/tests/fixtures/muxed-fmp4.base64"
    ));
    let initialization = bytes[..1_248].to_vec();
    let media_fragment = bytes[1_248..2_314].to_vec();
    (initialization, media_fragment.clone(), media_fragment)
}

fn audio_webm() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    (
        decode_base64(include_str!("../tests/fixtures/audio-webm-init.base64")),
        decode_base64(include_str!("../tests/fixtures/audio-webm-one.base64")),
        decode_base64(include_str!("../tests/fixtures/audio-webm-two.base64")),
    )
}

fn decode_base64(encoded: &str) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(encoded.len() * 3 / 4);
    let mut accumulator = 0_u32;
    let mut accumulated_bits = 0_u8;
    for encoded_byte in encoded.trim().bytes() {
        let value = match encoded_byte {
            b'A'..=b'Z' => encoded_byte - b'A',
            b'a'..=b'z' => encoded_byte - b'a' + 26,
            b'0'..=b'9' => encoded_byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            whitespace if whitespace.is_ascii_whitespace() => continue,
            _ => panic!("invalid generated base64 fixture"),
        };
        accumulator = (accumulator << 6) | u32::from(value);
        accumulated_bits += 6;
        if accumulated_bits >= 8 {
            accumulated_bits -= 8;
            decoded.push((accumulator >> accumulated_bits) as u8);
            accumulator &= (1_u32 << accumulated_bits) - 1;
        }
    }
    decoded
}

#[test]
fn template_timeline_applies_every_base_url_level_and_init() {
    let mpd = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
            mediaPresentationDuration="PT4S">
          <BaseURL>mpd/</BaseURL>
          <Period duration="PT4S"><BaseURL>period/</BaseURL>
            <AdaptationSet mimeType="video/mp4" contentType="video" codecs="avc1.4d401f">
              <BaseURL>adaptation/</BaseURL>
              <SegmentTemplate timescale="1000" presentationTimeOffset="9000"
                  initialization="init-$RepresentationID$.mp4"
                  media="chunk-$Time$.m4s">
                <SegmentTimeline><S t="9000" d="2000" r="1"/></SegmentTimeline>
              </SegmentTemplate>
              <Representation id="v1" bandwidth="10"><BaseURL>representation/</BaseURL></Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#,
    );
    let plan = build_manifest_plan(
        &mpd,
        &base(),
        &DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Video, DashContainer::IsoBmff, Some("v1")),
        },
        NonZeroUsize::new(8).expect("bound"),
    )
    .expect("template plan");
    let DashPresentationPlan::Single(component) = plan else {
        panic!("single plan")
    };
    let DashPeriodInputPlan::Ordered { resources, .. } = &component.periods[0].input else {
        panic!("ordered template")
    };
    assert_eq!(resources.len(), 3);
    assert_eq!(
        resources[0].target.expose_secret_for_request(),
        "https://media.example/root/mpd/period/adaptation/representation/init-v1.mp4"
    );
    assert_eq!(
        resources[2].target.expose_secret_for_request(),
        "https://media.example/root/mpd/period/adaptation/representation/chunk-11000.m4s"
    );
    assert_eq!(resources[2].timeline_start, Some(Duration::from_secs(2)));
}

#[test]
fn duration_template_and_segment_list_ranges_are_finite_and_bounded() {
    let duration_template = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT6S">
          <Period duration="PT6S"><AdaptationSet mimeType="audio/webm" codecs="opus">
            <Representation id="a"><SegmentTemplate timescale="1" duration="2"
              initialization="init.webm" media="$Number$.webm"/></Representation>
          </AdaptationSet></Period>
        </MPD>"#,
    );
    let plan = build_manifest_plan(
        &duration_template,
        &base(),
        &DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Audio, DashContainer::WebM, Some("a")),
        },
        NonZeroUsize::new(3).expect("bound"),
    )
    .expect("duration template");
    let DashPresentationPlan::Single(component) = plan else {
        panic!("single")
    };
    let DashPeriodInputPlan::Ordered { resources, .. } = &component.periods[0].input else {
        panic!("ordered")
    };
    assert_eq!(resources.len(), 4);
    assert_eq!(resources[3].duration, Some(Duration::from_secs(2)));

    let list = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT4S">
          <Period duration="PT4S"><AdaptationSet mimeType="audio/webm" codecs="opus">
            <Representation id="a"><SegmentList timescale="1" duration="2">
              <Initialization sourceURL="init.webm" range="0-9"/>
              <SegmentURL media="one.webm" mediaRange="10-19"/>
              <SegmentURL media="two.webm"/>
            </SegmentList></Representation>
          </AdaptationSet></Period>
        </MPD>"#,
    );
    let plan = build_manifest_plan(
        &list,
        &base(),
        &DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Audio, DashContainer::WebM, Some("a")),
        },
        NonZeroUsize::new(8).expect("bound"),
    )
    .expect("list plan");
    let DashPresentationPlan::Single(component) = plan else {
        panic!("single")
    };
    let DashPeriodInputPlan::Ordered { resources, .. } = &component.periods[0].input else {
        panic!("ordered")
    };
    assert_eq!(
        resources[0].byte_range.expect("init range").length().get(),
        10
    );
    assert_eq!(resources[1].byte_range.expect("media range").start(), 10);
}

#[test]
fn segment_base_is_range_backed_and_selection_never_picks_first_on_tie() {
    let base_mpd = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT4S">
          <Period duration="PT4S"><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
            <Representation id="v"><BaseURL>video.mp4</BaseURL>
              <SegmentBase indexRange="100-199"><Initialization range="0-99"/></SegmentBase>
            </Representation>
          </AdaptationSet></Period>
        </MPD>"#,
    );
    let plan = build_manifest_plan(
        &base_mpd,
        &base(),
        &DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Video, DashContainer::IsoBmff, Some("v")),
        },
        NonZeroUsize::new(8).expect("bound"),
    )
    .expect("base plan");
    let DashPresentationPlan::Single(component) = plan else {
        panic!("single")
    };
    let DashPeriodInputPlan::Range {
        target,
        catalog_probe_content_length,
        ..
    } = &component.periods[0].input
    else {
        panic!("Range-backed")
    };
    assert_eq!(
        target.expose_secret_for_request(),
        "https://media.example/root/video.mp4"
    );
    assert_eq!(
        catalog_probe_content_length.map(|length| length.get()),
        Some(100)
    );

    let tie = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
          <Period duration="PT2S"><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
            <SegmentTemplate timescale="1" duration="2" initialization="i.mp4" media="m.m4s"/>
            <Representation id="one"/><Representation id="two"/>
          </AdaptationSet></Period>
        </MPD>"#,
    );
    let error = build_manifest_plan(
        &tie,
        &base(),
        &DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Video, DashContainer::IsoBmff, None),
        },
        NonZeroUsize::new(8).expect("bound"),
    )
    .err()
    .expect("tie must remain ambiguous");
    assert!(matches!(
        error,
        DashPlanError::Selection(DashRepresentationSelectionError::Ambiguous)
    ));
}

#[test]
fn exact_dimensions_disambiguate_representations_without_guessing() {
    let mpd = parse(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
          <Period duration="PT2S"><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
            <SegmentTemplate timescale="1" duration="2" initialization="i.mp4"
              media="$RepresentationID$.m4s"/>
            <Representation id="720" width="1280" height="720"/>
            <Representation id="1080" width="1920" height="1080"/>
          </AdaptationSet></Period>
        </MPD>"#,
    );
    let mut exact = evidence(DashMediaKind::Video, DashContainer::IsoBmff, None);
    exact.dimensions = Some(DashVideoDimensions {
        width: 1_920,
        height: 1_080,
    });
    let plan = build_manifest_plan(
        &mpd,
        &base(),
        &DashPresentationSelection::Single { main: exact },
        NonZeroUsize::new(8).expect("bound"),
    )
    .expect("exact dimensions select one Representation");
    let DashPresentationPlan::Single(component) = plan else {
        panic!("single")
    };

    let DashPeriodInputPlan::Ordered { resources, .. } = &component.periods[0].input else {
        panic!("ordered template")
    };
    assert_eq!(
        resources[1].target.expose_secret_for_request(),
        "https://media.example/root/1080.m4s"
    );
}

#[test]
fn serialized_relative_fragments_require_exact_component_alignment() {
    let base = base();
    let component = |second_duration| DashSerializedComponent {
        container: DashContainer::IsoBmff,
        media_kind: DashMediaKind::Video,
        fragments: vec![
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Initialization,
                target: DashResourceReference::relative(base.clone(), "init.mp4"),
                byte_range: None,
                duration: None,
            },
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Media,
                target: DashResourceReference::relative(base.clone(), "one.m4s"),
                byte_range: None,
                duration: Some(Duration::from_secs(2)),
            },
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Media,
                target: DashResourceReference::relative(base.clone(), "two.m4s"),
                byte_range: None,
                duration: Some(second_duration),
            },
        ],
        query_application: AdaptiveResourceQueryApplication::MergeScopedAddition,
    };
    let mut audio = component(Duration::from_secs(2));
    audio.media_kind = DashMediaKind::Audio;
    let presentation = DashSerializedPresentation::Separate {
        video: component(Duration::from_secs(2)),
        audio: audio.clone(),
    };
    build_serialized_plan(
        &presentation,
        &DashPresentationSelection::Separate {
            video: evidence(DashMediaKind::Video, DashContainer::IsoBmff, None),
            audio: evidence(DashMediaKind::Audio, DashContainer::IsoBmff, None),
        },
        NonZeroUsize::new(8).expect("bound"),
    )
    .expect("aligned components");

    audio.fragments[2].duration = Some(Duration::from_secs(3));
    let error = build_serialized_plan(
        &DashSerializedPresentation::Separate {
            video: component(Duration::from_secs(2)),
            audio,
        },
        &DashPresentationSelection::Separate {
            video: evidence(DashMediaKind::Video, DashContainer::IsoBmff, None),
            audio: evidence(DashMediaKind::Audio, DashContainer::IsoBmff, None),
        },
        NonZeroUsize::new(8).expect("bound"),
    )
    .err()
    .expect("misaligned components rejected");
    assert!(matches!(error, DashPlanError::ComponentAlignmentMismatch));
}

#[test]
fn serialized_audio_webm_reaches_actual_ordered_runtime_readiness() {
    let (initialization, first, second) = audio_webm();
    let server = FixtureServer::start(initialization, first, second);
    let base = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(1);
    let component = DashSerializedComponent {
        container: DashContainer::WebM,
        media_kind: DashMediaKind::Audio,
        fragments: vec![
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Initialization,
                target: DashResourceReference::relative(base.clone(), "init.webm"),
                byte_range: None,
                duration: None,
            },
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Media,
                target: DashResourceReference::relative(base.clone(), "one.webm"),
                byte_range: None,
                duration: Some(Duration::from_millis(200)),
            },
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Media,
                target: DashResourceReference::relative(base.clone(), "two.webm"),
                byte_range: None,
                duration: Some(Duration::from_millis(200)),
            },
        ],
        query_application: AdaptiveResourceQueryApplication::MergeScopedAddition,
    };
    let result = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::SerializedSingle(Box::new(adaptive_context(
            &base,
            CancellationToken::new(),
            generation,
        ))),
        generation,
        input: DashVodInput::Serialized(DashSerializedPresentation::Single(component)),
        selection: DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Audio, DashContainer::WebM, None),
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    })
    .expect("serialized WebM prepares through production ordered runtime");

    assert_eq!(result.duration(), Duration::from_millis(400));
    let mut demuxer = result.into_demuxer();
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == media_core::TrackKind::Audio && track.codec_id == "A_OPUS")
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match demuxer.next_event().expect("ordered WebM runtime event") {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.track_id, demuxer.tracks()[0].id);
                break;
            }
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            DemuxReadEvent::EndOfStream => panic!("ordered WebM ended before first packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("ordered WebM readiness timeout"),
        }
    }
}

#[test]
fn serialized_muxed_fmp4_is_ready_before_publish_and_seek_reopens_fragment() {
    let (initialization, first, second) = muxed_fmp4();
    let server = FixtureServer::start(initialization, first, second);
    let base = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(1);
    let cancellation = CancellationToken::new();
    let component = DashSerializedComponent {
        container: DashContainer::IsoBmff,
        media_kind: DashMediaKind::Muxed,
        fragments: vec![
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Initialization,
                target: DashResourceReference::relative(base.clone(), "init.mp4"),
                byte_range: None,
                duration: None,
            },
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Media,
                target: DashResourceReference::relative(base.clone(), "one.m4s"),
                byte_range: None,
                duration: Some(Duration::from_secs(1)),
            },
            DashSerializedFragment {
                kind: DashSerializedFragmentKind::Media,
                target: DashResourceReference::relative(base.clone(), "two.m4s"),
                byte_range: None,
                duration: Some(Duration::from_secs(1)),
            },
        ],
        query_application: AdaptiveResourceQueryApplication::MergeScopedAddition,
    };
    let selection = DashPresentationSelection::Single {
        main: evidence(DashMediaKind::Muxed, DashContainer::IsoBmff, None),
    };
    let result = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::SerializedSingle(Box::new(adaptive_context(
            &base,
            cancellation,
            generation,
        ))),
        generation,
        input: DashVodInput::Serialized(DashSerializedPresentation::Single(component)),
        selection,
        demux_registry: demux_registry(),
        policy: open_policy(),
    })
    .expect("serialized fMP4 prepares");
    assert_eq!(result.duration(), Duration::from_secs(2));
    let seek_handle = result.async_seek_handle();
    let mut demuxer = result.into_demuxer();
    assert_eq!(
        demuxer
            .tracks()
            .iter()
            .filter(|track| track.kind == media_core::TrackKind::Video)
            .count(),
        1
    );
    assert_eq!(
        demuxer
            .tracks()
            .iter()
            .filter(|track| track.kind == media_core::TrackKind::Audio)
            .count(),
        1
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            fence,
            DemuxSeekRequest::accurate(Duration::from_millis(1_200)),
        )
        .expect("fragment-boundary seek accepted");
    let seek = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            assert_eq!(receipt.fence, fence);
            let ProgressiveAsyncSeekOutcome::Succeeded(seek) = receipt.outcome else {
                panic!("fragment-boundary seek failed: {:?}", receipt.outcome);
            };
            break seek;
        }
        assert!(std::time::Instant::now() < deadline, "seek receipt timeout");
        thread::sleep(Duration::from_millis(1));
    };
    assert!(
        seek.actual_position.as_duration() <= Duration::from_millis(1_200),
        "authoritative anchor не должен быть позже requested target"
    );
    loop {
        match demuxer.next_event().expect("runtime event") {
            DemuxReadEvent::Packet(packet) if packet.pts >= Duration::from_millis(1_200) => {
                break;
            }
            DemuxReadEvent::Packet(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("seeked runtime ended before packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("runtime readiness timeout"),
        }
    }
}

#[test]
fn manifest_segment_base_uses_range_source_and_receipted_seek() {
    let (initialization, media_fragment, _) = muxed_fmp4();
    let media_resource = [initialization, media_fragment].concat();
    let manifest = format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT1S">
          <Period duration="PT1S"><AdaptationSet contentType="application"
            mimeType="application/mp4" codecs="avc1.4d401f,mp4a.40.2">
            <Representation id="muxed"><BaseURL>video.mp4</BaseURL>
              <SegmentBase indexRange="1248-{}"><Initialization range="0-1247"/></SegmentBase>
            </Representation>
          </AdaptationSet></Period>
        </MPD>"#,
        media_resource.len().saturating_sub(1)
    );
    let server = RangeFixtureServer::start(manifest, media_resource);
    let manifest_target = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(1);
    let result = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(adaptive_context(
            &manifest_target,
            CancellationToken::new(),
            generation,
        ))),
        generation,
        input: DashVodInput::Manifest(manifest_input(manifest_target)),
        selection: DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Muxed, DashContainer::IsoBmff, Some("muxed")),
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    })
    .expect("SegmentBase Range runtime prepares");
    let seek_handle = result.async_seek_handle();
    let _demuxer = result.into_demuxer();
    let fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            fence,
            DemuxSeekRequest::accurate(Duration::from_millis(500)),
        )
        .expect("SegmentBase seek accepted");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            assert_eq!(receipt.fence, fence);
            assert!(
                matches!(receipt.outcome, ProgressiveAsyncSeekOutcome::Succeeded(_)),
                "SegmentBase seek must return authoritative result: {:?}",
                receipt.outcome
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "SegmentBase seek receipt timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn manifest_multi_period_transition_keeps_tracks_and_timeline_stable() {
    let (initialization, first, second) = muxed_fmp4();
    let manifest = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
        mediaPresentationDuration="PT2S">
      <Period id="p0" duration="PT1S">
        <AdaptationSet contentType="application" mimeType="application/mp4"
          codecs="avc1.4d401f,mp4a.40.2">
          <Representation id="muxed">
            <SegmentList timescale="1" duration="1">
              <Initialization sourceURL="init.mp4"/>
              <SegmentURL media="one.m4s"/>
            </SegmentList>
          </Representation>
        </AdaptationSet>
      </Period>
      <Period id="p1" duration="PT1S">
        <AdaptationSet contentType="application" mimeType="application/mp4"
          codecs="avc1.4d401f,mp4a.40.2">
          <Representation id="muxed">
            <SegmentList timescale="1" duration="1">
              <Initialization sourceURL="init.mp4"/>
              <SegmentURL media="two.m4s"/>
            </SegmentList>
          </Representation>
        </AdaptationSet>
      </Period>
    </MPD>"#
        .to_owned();
    let server = FixtureServer::start_with_manifest(manifest, initialization, first, second);
    let manifest_target = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(1);
    let result = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(adaptive_context(
            &manifest_target,
            CancellationToken::new(),
            generation,
        ))),
        generation,
        input: DashVodInput::Manifest(manifest_input(manifest_target)),
        selection: DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Muxed, DashContainer::IsoBmff, Some("muxed")),
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    })
    .expect("multi-period DASH prepares");
    let mut demuxer = result.into_demuxer();
    let stable_track_ids = demuxer
        .tracks()
        .iter()
        .map(|track| track.id)
        .collect::<Vec<_>>();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut saw_transition = false;
    let mut saw_second_period_packet = false;
    while std::time::Instant::now() < deadline && !saw_second_period_packet {
        match demuxer.next_event().expect("multi-period runtime event") {
            DemuxReadEvent::TracksChanged(update) => {
                assert_eq!(
                    update
                        .tracks
                        .iter()
                        .map(|track| track.id)
                        .collect::<Vec<_>>(),
                    stable_track_ids
                );
                saw_transition = true;
            }
            DemuxReadEvent::Packet(packet) if packet.pts >= Duration::from_secs(1) => {
                assert!(
                    saw_transition,
                    "transition event precedes new Period packet"
                );
                saw_second_period_packet = true;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                thread::sleep(Duration::from_millis(1));
            }
            DemuxReadEvent::EndOfStream => break,
        }
    }
    assert!(saw_transition, "bounded Period transition must be explicit");
    assert!(
        saw_second_period_packet,
        "second Period packet must use global timeline"
    );
}

#[test]
fn discovered_multi_period_vod_opens_exact_and_semantic_selection() {
    let (initialization, first, second) = muxed_fmp4();
    let manifest = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
        mediaPresentationDuration="PT2S">
      <Period id="p0" duration="PT1S">
        <AdaptationSet contentType="application" mimeType="application/mp4"
          codecs="avc1.4d401f,mp4a.40.2">
          <Representation id="muxed">
            <SegmentList timescale="1" duration="1">
              <Initialization sourceURL="init.mp4"/>
              <SegmentURL media="one.m4s"/>
            </SegmentList>
          </Representation>
        </AdaptationSet>
      </Period>
      <Period id="p1" duration="PT1S">
        <AdaptationSet contentType="application" mimeType="application/mp4"
          codecs="avc1.4d401f,mp4a.40.2">
          <Representation id="muxed">
            <SegmentList timescale="1" duration="1">
              <Initialization sourceURL="init.mp4"/>
              <SegmentURL media="two.m4s"/>
            </SegmentList>
          </Representation>
        </AdaptationSet>
      </Period>
    </MPD>"#
        .to_owned();
    let server = FixtureServer::start_with_manifest(manifest, initialization, first, second);

    let exact_catalog =
        discover_dash_vod_catalog(discovered_vod_request(&server, SourceGeneration::new(1), 1))
            .expect("exact discovery");
    assert_eq!(exact_catalog.catalog().stored_variant_count(), 1);
    let exact = exact_catalog.provider_default().clone();
    let exact_open =
        prepare_discovered_dash_vod(exact_catalog, exact).expect("exact discovered open");
    assert_eq!(exact_open.duration(), Duration::from_secs(2));

    let semantic_catalog =
        discover_dash_vod_catalog(discovered_vod_request(&server, SourceGeneration::new(2), 2))
            .expect("semantic discovery");
    let semantic = semantic_catalog
        .provider_default()
        .semantic_rematch_request();
    let semantic_open = prepare_discovered_dash_vod_semantic(semantic_catalog, semantic)
        .expect("semantic discovered open");
    assert_eq!(semantic_open.duration(), Duration::from_secs(2));
}

#[test]
fn native_fetched_manifest_discovery_reuses_single_root_response() {
    let (initialization, first, second) = muxed_fmp4();
    let manifest = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
        mediaPresentationDuration="PT2S">
      <Period duration="PT2S">
        <AdaptationSet contentType="application" mimeType="application/mp4"
          codecs="avc1.4d401f,mp4a.40.2">
          <Representation id="muxed" width="16" height="16">
            <SegmentList timescale="1" duration="1">
              <Initialization sourceURL="init.mp4"/>
              <SegmentURL media="one.m4s"/>
              <SegmentURL media="two.m4s"/>
            </SegmentList>
          </Representation>
        </AdaptationSet>
      </Period>
    </MPD>"#
        .to_owned();
    let server = FixtureServer::start_with_manifest(manifest, initialization, first, second);
    let generation = SourceGeneration::new(7);
    let manifest_target = server.target("/manifest.mpd");
    let http = adaptive_context(&manifest_target, CancellationToken::new(), generation);
    let policy = open_policy();
    let fetched = http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            generation,
            manifest_target.clone(),
            policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))
        .expect("single authoritative root fetch");
    let parser_input = manifest_input(manifest_target.clone());
    let fetched_input = DashFetchedManifestInput::new(
        manifest_target,
        fetched,
        &http,
        parser_input.xml_budgets,
        parser_input.mpd_limits,
    );
    let discovered = discover_native_dash_vod_catalog(NativeDashVodCatalogDiscoveryRequest {
        http: Box::new(http),
        generation,
        manifest: fetched_input,
        demux_registry: demux_registry(),
        policy,
        catalog_identity: catalog_identity(7),
        catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
        compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
        capability_probe: &AcceptAllDashCapabilities,
        preferred_height: PreferredHeightPolicy::NoPreference,
    })
    .expect("native fetched discovery");
    let selected = discovered.provider_default().clone();
    let opened =
        prepare_discovered_dash_vod(discovered, selected).expect("open discovered native row");
    assert_eq!(opened.duration(), Duration::from_secs(2));
    assert_eq!(server.manifest_request_count(), 1);
}

#[test]
fn native_catalog_proves_slow_initializations_concurrently_and_opens_selected_packet() {
    let (initialization, first, second) = muxed_fmp4();
    let representation_rows = (0..4)
        .map(|ordinal| {
            format!(
                r#"<Representation id="lane-{ordinal}" bandwidth="{}" width="16" height="16">
                     <SegmentList timescale="1" duration="1">
                       <Initialization sourceURL="init-{ordinal}.mp4"/>
                       <SegmentURL media="one-{ordinal}.m4s"/>
                       <SegmentURL media="two-{ordinal}.m4s"/>
                     </SegmentList>
                   </Representation>"#,
                100_000 + ordinal * 10_000
            )
        })
        .collect::<String>();
    let manifest = format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
             <Period duration="PT2S">
               <AdaptationSet contentType="application" mimeType="application/mp4"
                 codecs="avc1.4d401f,mp4a.40.2">
                 {representation_rows}
               </AdaptationSet>
             </Period>
           </MPD>"#
    );
    let manifest_for_handoff = manifest.clone();
    let initialization_delay = Duration::from_millis(200);
    let server = ParallelCatalogFixtureServer::start(
        manifest,
        initialization,
        first,
        second,
        initialization_delay,
    );
    let generation = SourceGeneration::new(8);
    let manifest_target = server.target("/manifest.mpd");
    let http = adaptive_context(&manifest_target, CancellationToken::new(), generation);
    let policy = open_policy();
    let fetched_manifest = http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            generation,
            manifest_target.clone(),
            policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))
        .expect("parallel fixture root fetch");
    assert_eq!(fetched_manifest.bytes(), manifest_for_handoff.as_bytes());
    let parser_input = manifest_input(manifest_target.clone());
    let fetched_input = DashFetchedManifestInput::new(
        manifest_target,
        fetched_manifest,
        &http,
        parser_input.xml_budgets,
        parser_input.mpd_limits,
    );

    let discovery_started = Instant::now();
    let discovered = discover_native_dash_vod_catalog(NativeDashVodCatalogDiscoveryRequest {
        http: Box::new(http),
        generation,
        manifest: fetched_input,
        demux_registry: demux_registry(),
        policy,
        catalog_identity: catalog_identity(8),
        catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
        compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
        capability_probe: &AcceptAllDashCapabilities,
        preferred_height: PreferredHeightPolicy::NoPreference,
    })
    .expect("parallel native DASH discovery");
    let discovery_elapsed = discovery_started.elapsed();

    assert_eq!(discovered.catalog().stored_variant_count(), 4);
    assert!(
        server.maximum_active_initializations() >= 4,
        "all four initialization proof requests must overlap"
    );
    assert!(
        discovery_elapsed < initialization_delay * 3,
        "bounded parallel proof must not serialize four delayed requests: {discovery_elapsed:?}"
    );

    let selected = discovered.provider_default().clone();
    let opened = prepare_discovered_dash_vod(discovered, selected)
        .expect("selected DASH lane opens after parallel catalog proof");
    let mut demuxer = opened.into_demuxer();
    let packet_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match demuxer.next_event().expect("selected DASH runtime event") {
            DemuxReadEvent::Packet(_) => break,
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < packet_deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("selected DASH runtime packet readiness timeout")
            }
            DemuxReadEvent::EndOfStream => {
                panic!("selected DASH runtime ended before its first packet")
            }
        }
    }
}

#[test]
fn fetched_manifest_handoff_rejects_a_different_runtime_generation() {
    let server =
        FixtureServer::start_with_manifest("<MPD/>".to_owned(), Vec::new(), Vec::new(), Vec::new());
    let manifest_target = server.target("/manifest.mpd");
    let fetched_generation = SourceGeneration::new(41);
    let fetched_http = adaptive_context(
        &manifest_target,
        CancellationToken::new(),
        fetched_generation,
    );
    let policy = open_policy();
    let fetched = fetched_http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            fetched_generation,
            manifest_target.clone(),
            policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))
        .expect("fetch manifest with original generation");
    let parser_input = manifest_input(manifest_target.clone());
    let fetched_input = DashFetchedManifestInput::new(
        manifest_target.clone(),
        fetched,
        &fetched_http,
        parser_input.xml_budgets,
        parser_input.mpd_limits,
    );
    let current_generation = SourceGeneration::new(42);
    let current_http = adaptive_context(
        &manifest_target,
        CancellationToken::new(),
        current_generation,
    );
    let error = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(current_http)),
        generation: current_generation,
        input: DashVodInput::FetchedManifest(fetched_input),
        selection: DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Muxed, DashContainer::IsoBmff, None),
        },
        demux_registry: demux_registry(),
        policy,
    })
    .expect_err("cross-generation fetched handoff must fail closed");

    assert!(matches!(
        error,
        DashVodOpenError::FetchedManifestGenerationMismatch
    ));
    assert_eq!(server.manifest_request_count(), 1);
}

#[test]
fn fetched_manifest_handoff_rechecks_current_body_policy() {
    let server =
        FixtureServer::start_with_manifest("<MPD/>".to_owned(), Vec::new(), Vec::new(), Vec::new());
    let manifest_target = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(43);
    let http = adaptive_context(&manifest_target, CancellationToken::new(), generation);
    let mut policy = open_policy();
    let fetched = http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            generation,
            manifest_target.clone(),
            policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))
        .expect("fetch manifest under transport bound");
    let parser_input = manifest_input(manifest_target.clone());
    let fetched_input = DashFetchedManifestInput::new(
        manifest_target,
        fetched,
        &http,
        parser_input.xml_budgets,
        parser_input.mpd_limits,
    );
    policy.maximum_manifest_bytes = NonZeroUsize::new(1).expect("non-zero policy");
    let error = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(http)),
        generation,
        input: DashVodInput::FetchedManifest(fetched_input),
        selection: DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Muxed, DashContainer::IsoBmff, None),
        },
        demux_registry: demux_registry(),
        policy,
    })
    .expect_err("handoff must recheck the current open policy");

    assert!(matches!(
        error,
        DashVodOpenError::FetchedManifestExceedsPolicy
    ));
    assert_eq!(server.manifest_request_count(), 1);
}

#[test]
fn cancelled_manifest_open_stops_before_network_side_effect() {
    let manifest_target = HttpRequestTarget::parse_exact("http://127.0.0.1:9/never-requested.mpd")
        .expect("syntactically valid target");
    let generation = SourceGeneration::new(1);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(adaptive_context(
            &manifest_target,
            cancellation,
            generation,
        ))),
        generation,
        input: DashVodInput::Manifest(manifest_input(manifest_target)),
        selection: DashPresentationSelection::Single {
            main: evidence(DashMediaKind::Video, DashContainer::IsoBmff, None),
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    })
    .expect_err("cancelled open must fail before network");
    assert!(matches!(
        error,
        DashVodOpenError::Transport(AdaptiveTransportError::Cancelled)
    ));
}
