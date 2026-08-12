//! Hermetic S38 acceptance evidence: local F4M/bootstrap/F4F проходят production runtime.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bounded_xml_reader::XmlBudgets;
use demux_api::{
    DemuxRegistry, DemuxSniffBudget, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveAsyncSeekReceipt, ProgressiveDemuxBufferLimits,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use flv_demux::{FlvDemuxFactory, FlvDemuxOptions};
use hds_manifest_core::{F4mManifestLimits, HdsBootstrapLimits};
use media_core::{DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, Demuxer, TrackKind};
use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig, ValidatedHttpHeaders,
};
use web_media_adaptive::{AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantSelection, ExactSelectionIdentity,
    ExtractionGeneration, PreferredHeightPolicy, SemanticIdentity, SourceIdentity,
};
use web_media_hds::{
    HdsCatalogDiscoveryRequest, HdsNoPlayableRendition, HdsRenditionCapabilityProbe,
    HdsRenditionCapabilityRejection, HdsRenditionSelection, HdsVodOpenPolicy, HdsVodOpenRequest,
    discover_hds_renditions, prepare_discovered_hds_vod, prepare_hds_vod,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

/// Общий deadline ограничивает только ожидание worker-а в test thread-е.
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Минимальный локальный HTTP origin без внешней сети и скрытых fixture-файлов.
struct HermeticHttpServer {
    /// Адрес случайного loopback port-а.
    address: SocketAddr,
    /// Cooperative флаг завершения accept loop-а.
    stop: Arc<AtomicBool>,
    /// Запрошенные path-ы доказывают реальный transport traversal.
    requested_paths: Arc<Mutex<Vec<String>>>,
    /// Join handle не позволяет серверу пережить тест.
    worker: Option<thread::JoinHandle<()>>,
}

impl HermeticHttpServer {
    /// Запускает bounded origin с заранее известными immutable ответами.
    fn start(routes: HashMap<&'static str, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HDS fixture server");
        listener
            .set_nonblocking(true)
            .expect("set HDS fixture listener nonblocking");
        let address = listener.local_addr().expect("read HDS fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let requested_paths = Arc::new(Mutex::new(Vec::new()));
        let worker_requested_paths = Arc::clone(&requested_paths);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _peer)) => {
                        let request = read_http_request(&mut stream);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or_default()
                            .split('?')
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        worker_requested_paths
                            .lock()
                            .expect("lock HDS requested paths")
                            .push(path.clone());
                        let response = routes.get(path.as_str()).map_or_else(
                            || http_response("404 Not Found", b"missing fixture route"),
                            |body| http_response("200 OK", body),
                        );
                        stream
                            .write_all(&response)
                            .expect("write HDS fixture response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("HDS fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            requested_paths,
            worker: Some(worker),
        }
    }

    /// Возвращает exact HTTP target внутри собственного loopback origin-а.
    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("valid HDS fixture target")
    }

    /// Возвращает snapshot уже обслуженных path-ов без request headers/secrets.
    fn requested_paths(&self) -> Vec<String> {
        self.requested_paths
            .lock()
            .expect("lock HDS requested paths")
            .clone()
    }
}

impl Drop for HermeticHttpServer {
    /// Завершает accept loop и обязательно присоединяет fixture thread.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join HDS fixture server");
        }
    }
}

/// Читает только HTTP headers; test origin не принимает request body.
fn read_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(TEST_TIMEOUT))
        .expect("set HDS fixture read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream.read(&mut chunk).expect("read HDS fixture request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).expect("HDS fixture request is UTF-8 HTTP")
}

/// Формирует закрывающий соединение HTTP/1.1 response с exact body length.
fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut response = headers.into_bytes();
    response.extend_from_slice(body);
    response
}

/// Доказывает полный positive path вместо отдельного parser-only smoke.
#[test]
fn prepares_local_f4m_bootstrap_and_f4f_until_tracks_and_packet() {
    let first_fragment = f4f_fragment(0);
    let second_fragment = f4f_fragment(1_000);
    let server = HermeticHttpServer::start(HashMap::from([
        ("/manifest.f4m", vod_manifest()),
        ("/media/bootstrap.bin", vod_bootstrap()),
        ("/media/videoSeg1-Frag1", first_fragment),
        ("/media/videoSeg1-Frag2", second_fragment),
    ]));
    let cancellation = CancellationToken::new();
    let root_target = server.target("/manifest.f4m");
    let opened = prepare_hds_vod(HdsVodOpenRequest {
        transport_request: transport_request(&root_target, cancellation.clone()),
        source_config: source_config(),
        demux_registry: f4f_registry(),
        policy: open_policy(),
        selection: HdsRenditionSelection::BestByPreference(PreferredHeightPolicy::NoPreference),
    })
    .expect("production HDS VOD preparation succeeds");

    assert!(opened.catalog().is_none());
    assert_eq!(opened.presentation_window().start(), Duration::ZERO);
    assert_eq!(
        opened.presentation_window().end_exclusive(),
        Duration::from_secs(2)
    );

    let seek_handle = opened.async_seek_handle();
    let mut demuxer = opened.into_demuxer();
    let initial_event = next_ready_event(demuxer.as_mut());
    let DemuxReadEvent::TracksChanged(initial_tracks) = initial_event else {
        panic!("initial HDS event must publish discovered F4F tracks");
    };
    assert!(
        initial_tracks
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video && track.codec_id == "V_MPEG4/ISO/AVC")
    );

    let mut audio_track_seen = false;
    let mut media_packet_seen = false;
    for _ in 0..16 {
        match next_ready_event(demuxer.as_mut()) {
            DemuxReadEvent::TracksChanged(update) => {
                audio_track_seen |= update
                    .tracks
                    .iter()
                    .any(|track| track.kind == TrackKind::Audio && track.codec_id == "A_AAC");
            }
            DemuxReadEvent::Packet(packet) => {
                media_packet_seen = true;
                assert!(matches!(packet.kind, TrackKind::Video | TrackKind::Audio));
            }
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                unreachable!("next_ready_event filters readiness events")
            }
        }
        if audio_track_seen && media_packet_seen {
            break;
        }
    }
    assert!(audio_track_seen, "F4F FLV tags must publish the AAC track");
    assert!(
        media_packet_seen,
        "F4F FLV tags must yield an encoded packet"
    );

    let seek_fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            seek_fence,
            DemuxSeekRequest::accurate(Duration::from_millis(1_500)),
        )
        .expect("HDS transactional seek is accepted");
    let seek_receipt = wait_for_seek_receipt(&seek_handle);
    assert_eq!(seek_receipt.fence, seek_fence);
    let ProgressiveAsyncSeekOutcome::Succeeded(seek_result) = seek_receipt.outcome else {
        panic!(
            "HDS transactional seek must succeed, got {:?}",
            seek_receipt.outcome
        );
    };
    assert_eq!(
        seek_result.requested_position.as_duration(),
        Duration::from_millis(1_500)
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_secs(1)
    );

    let requested_paths = server.requested_paths();
    assert!(requested_paths.iter().any(|path| path == "/manifest.f4m"));
    assert!(
        requested_paths
            .iter()
            .any(|path| path == "/media/bootstrap.bin")
    );
    assert!(
        requested_paths
            .iter()
            .any(|path| path == "/media/videoSeg1-Frag1")
    );
    assert!(
        requested_paths
            .iter()
            .any(|path| path == "/media/videoSeg1-Frag2")
    );

    cancellation.cancel();
    drop(demuxer);
}

/// Reorder/URL rotation сохраняет semantic row, unavailable sibling изолируется,
/// а rematched exact identity открывает только fresh private runtime mapping.
#[test]
fn discovers_refresh_stable_coupled_row_and_opens_fresh_exact_selection() {
    let first_server = HermeticHttpServer::start(HashMap::from([
        ("/first.f4m", discovery_manifest("old", false)),
        ("/media/bootstrap.bin", vod_bootstrap()),
        ("/media/oldSeg1-Frag1", f4f_fragment(0)),
        ("/media/oldSeg1-Frag2", f4f_fragment(1_000)),
    ]));
    let first_target = first_server.target("/first.f4m");
    let capabilities = FixtureHdsCapabilities::default();
    let first = discover_hds_renditions(HdsCatalogDiscoveryRequest {
        transport_request: transport_request(&first_target, CancellationToken::new()),
        source_config: source_config(),
        demux_registry: f4f_registry(),
        policy: open_policy(),
        catalog_identity: catalog_identity(1),
        capability_probe: &capabilities,
        preferred_height: PreferredHeightPolicy::NoPreference,
    })
    .expect("first HDS catalog discovery succeeds");
    assert_eq!(first.catalog().coupled_presentations().len(), 1);
    assert!(
        first.rejections().is_empty(),
        "infrastructure failure must not be published as content rejection"
    );
    let ComponentVariantSelection::Coupled {
        presentation: old_presentation,
        ..
    } = first.provider_default()
    else {
        panic!("HDS provider default must be coupled");
    };
    let stale_exact = old_presentation.exact_identity().clone();
    let semantic_request = first.provider_default().semantic_rematch_request();

    let fresh_server = HermeticHttpServer::start(HashMap::from([
        ("/fresh.f4m", discovery_manifest("rotated", true)),
        ("/media/bootstrap.bin", vod_bootstrap()),
        ("/media/rotatedSeg1-Frag1", f4f_fragment(0)),
        ("/media/rotatedSeg1-Frag2", f4f_fragment(1_000)),
    ]));
    let fresh_target = fresh_server.target("/fresh.f4m");
    let fresh = discover_hds_renditions(HdsCatalogDiscoveryRequest {
        transport_request: transport_request(&fresh_target, CancellationToken::new()),
        source_config: source_config(),
        demux_registry: f4f_registry(),
        policy: open_policy(),
        catalog_identity: catalog_identity(2),
        capability_probe: &capabilities,
        preferred_height: PreferredHeightPolicy::NoPreference,
    })
    .expect("fresh HDS catalog discovery succeeds");
    assert!(
        fresh
            .catalog()
            .select_exact(web_media_core::ComponentVariantSelectionRequest::Coupled {
                presentation: stale_exact,
            })
            .is_err(),
        "old exact identity must not cross the fresh catalog generation"
    );
    let rematched = fresh
        .catalog()
        .rematch_semantic(semantic_request)
        .expect("semantic HDS row survives reorder and URL rotation");
    let ComponentVariantSelection::Coupled { presentation, .. } = rematched else {
        panic!("HDS rendition must remain one coupled A/V presentation");
    };
    let opened = prepare_discovered_hds_vod(fresh, presentation.exact_identity().clone())
        .expect("fresh exact HDS row opens");
    assert_eq!(
        opened
            .catalog()
            .expect("discovered open retains neutral catalog")
            .coupled_presentations()
            .len(),
        1
    );
    assert_eq!(opened.presentation_window().start(), Duration::ZERO);
    let mut demuxer = opened.into_demuxer();
    let _ = next_ready_event(demuxer.as_mut());
    assert!(
        fresh_server
            .requested_paths()
            .iter()
            .any(|path| path == "/media/rotatedSeg1-Frag1")
    );
    assert!(
        !fresh_server
            .requested_paths()
            .iter()
            .any(|path| path == "/media/oldSeg1-Frag1")
    );
    assert_eq!(capabilities.checked_rows.load(Ordering::Acquire), 2);
}

#[test]
fn capability_rejection_prevents_truthless_catalog_publication() {
    let server = HermeticHttpServer::start(HashMap::from([
        ("/manifest.f4m", vod_manifest()),
        ("/media/bootstrap.bin", vod_bootstrap()),
        ("/media/videoSeg1-Frag1", f4f_fragment(0)),
        ("/media/videoSeg1-Frag2", f4f_fragment(1_000)),
    ]));
    let target = server.target("/manifest.f4m");

    let error = discover_hds_renditions(HdsCatalogDiscoveryRequest {
        transport_request: transport_request(&target, CancellationToken::new()),
        source_config: source_config(),
        demux_registry: f4f_registry(),
        policy: open_policy(),
        catalog_identity: catalog_identity(1),
        capability_probe: &RejectingHdsCapabilities,
        preferred_height: PreferredHeightPolicy::NoPreference,
    })
    .expect_err("capability-rejected row must not be published");

    assert!(
        error.downcast_ref::<HdsNoPlayableRendition>().is_some(),
        "all content/capability rejections must preserve typed parent fallback"
    );
}

/// Fixture adapter подтверждает, что discovery дошёл до immutable capability boundary.
#[derive(Default)]
struct FixtureHdsCapabilities {
    checked_rows: AtomicUsize,
}

impl HdsRenditionCapabilityProbe for FixtureHdsCapabilities {
    fn check_coupled_av(
        &self,
        video: &media_core::TrackInfo,
        audio: &media_core::TrackInfo,
    ) -> Result<(), HdsRenditionCapabilityRejection> {
        if video.codec_id != "V_MPEG4/ISO/AVC" || audio.codec_id != "A_AAC" {
            return Err(HdsRenditionCapabilityRejection);
        }
        self.checked_rows.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

struct RejectingHdsCapabilities;

impl HdsRenditionCapabilityProbe for RejectingHdsCapabilities {
    fn check_coupled_av(
        &self,
        _video: &media_core::TrackInfo,
        _audio: &media_core::TrackInfo,
    ) -> Result<(), HdsRenditionCapabilityRejection> {
        Err(HdsRenditionCapabilityRejection)
    }
}

/// Ждёт authoritative terminal receipt только в bounded test thread-е.
fn wait_for_seek_receipt(handle: &ProgressiveAsyncSeekHandle) -> ProgressiveAsyncSeekReceipt {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(receipt) = handle.poll_receipt() {
            return receipt;
        }
        assert!(
            Instant::now() < deadline,
            "HDS transactional seek receipt timed out"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

/// Poll-ит neutral readiness contract, не блокируя player-owner call.
fn next_ready_event(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        let call_started = Instant::now();
        match demuxer.next_event().expect("HDS demux event") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(
                    call_started.elapsed() < Duration::from_millis(50),
                    "HDS player-owner poll must stay nonblocking"
                );
                assert!(Instant::now() < deadline, "HDS deferred worker timed out");
                thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

/// Строит app-equivalent request без cookies/headers или другого secret material.
fn transport_request(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
) -> TransportOpenRequest {
    let source = SourceIdentity::new(38);
    let generation = SourceGeneration::new(1);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(generation.value()),
        CandidateFormatIdentity::new("hds-runtime-test").expect("candidate format identity"),
    );
    let semantic =
        SemanticIdentity::new(source, "hds-runtime-test").expect("candidate semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("HDS muxed component identity");
    let secret_scope = SecretRequestScope::from_target(
        target,
        HttpPathScope::new("/").expect("HDS fixture root path scope"),
    );
    let secrets = SecretRequestContext::builder(secret_scope)
        .with_headers(ValidatedHttpHeaders::new(Vec::new()).expect("empty HDS fixture headers"))
        .build();
    TransportOpenRequest::new(
        TransportProviderId::new("hds-runtime-test").expect("HDS provider identity"),
        component,
        target.clone(),
        MediaPresentation::Vod,
        generation,
        secrets,
        RedirectPolicy::same_origin(
            RedirectHopLimit::new(2).expect("HDS fixture redirect hop limit"),
        ),
        cancellation,
    )
    .expect("HDS transport request")
}

/// Строит parent exact+semantic scope и отдельный catalog generation fence.
fn catalog_identity(generation: u64) -> ComponentVariantCatalogIdentity {
    let source = SourceIdentity::new(38);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("hds-catalog-test").expect("candidate format identity"),
    );
    let semantic =
        SemanticIdentity::new(source, "hds-catalog-test").expect("candidate semantic identity");
    let parent = ExactSelectionIdentity::new(exact, semantic).expect("parent selection identity");
    ComponentVariantCatalogIdentity::new(parent, ComponentVariantCatalogGeneration::new(generation))
}

/// Нормализует обычный app network config в source-core boundary.
fn source_config() -> SourceRuntimeConfig {
    SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
        .expect("HDS fixture source config")
}

/// Регистрирует production S30 F4F factory вместо fake demuxer-а.
fn f4f_registry() -> Arc<DemuxRegistry> {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            FlvDemuxFactory::new(FlvDemuxOptions::default()).expect("F4F factory"),
        ))
        .expect("register F4F factory");
    Arc::new(registry)
}

/// Собирает компактные, но явные bounds для каждого untrusted слоя.
fn open_policy() -> HdsVodOpenPolicy {
    HdsVodOpenPolicy {
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(64 * 1_024)
            .maximum_depth(16)
            .maximum_tokens(1_024)
            .maximum_attributes_per_element(16)
            .maximum_attribute_count(128)
            .maximum_attribute_bytes(8 * 1_024)
            .maximum_namespace_declarations_per_element(4)
            .maximum_namespace_declaration_count(16)
            .maximum_namespace_bytes(1_024)
            .maximum_text_bytes(8 * 1_024)
            .build()
            .expect("HDS XML budgets"),
        manifest_limits: F4mManifestLimits::new(
            non_zero(4),
            non_zero(4),
            non_zero(16 * 1_024),
            non_zero(1_024),
        ),
        bootstrap_limits: HdsBootstrapLimits {
            maximum_bytes: non_zero(16 * 1_024),
            maximum_boxes: non_zero(16),
            maximum_fragments: non_zero(8),
            maximum_string_bytes: non_zero(128),
        },
        adaptive_limits: AdaptiveTransportLimits::new(
            non_zero(64 * 1_024),
            non_zero(256 * 1_024),
            non_zero(8),
        ),
        adaptive_retry: AdaptiveRetryPolicy::new(
            NonZeroU8::new(2).expect("HDS retry attempts"),
            Duration::from_millis(2),
            Duration::from_millis(5),
        )
        .expect("HDS retry policy"),
        demux_sniff_budget: DemuxSniffBudget::new(
            non_zero(64 * 1_024),
            non_zero(2),
            Duration::from_secs(1),
        )
        .expect("HDS F4F sniff budget"),
        demux_buffer_limits: ProgressiveDemuxBufferLimits::new(non_zero(16), non_zero(512 * 1_024)),
        demux_retry_hint: DemuxRetryHint::new(Duration::from_millis(2))
            .expect("HDS demux retry hint"),
        async_seek_limits: ProgressiveAsyncSeekLimits::new(non_zero(4)),
        maximum_hierarchy_depth: 2,
        maximum_manifest_documents: 4,
        maximum_renditions: 4,
    }
}

/// Возвращает ненулевой fixture bound с читаемым failure.
fn non_zero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("HDS fixture bound is non-zero")
}

/// F4M с URL bootstrap-ом заставляет runtime выполнить оба HTTP fetch-а.
fn vod_manifest() -> Vec<u8> {
    br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><streamType>recorded</streamType><duration>2</duration><baseURL>media/</baseURL><media url="video" bitrate="1200" width="1280" height="720" bootstrapInfoId="boot"/><bootstrapInfo id="boot" url="bootstrap.bin"/></manifest>"#.to_vec()
}

/// Два refresh snapshot-а различаются только row order и valid media locator-ом.
fn discovery_manifest(valid_media: &str, valid_first: bool) -> Vec<u8> {
    let valid = format!(
        r#"<media url="{valid_media}" bitrate="1200" width="1280" height="720" bootstrapInfoId="boot"/>"#
    );
    let broken =
        r#"<media url="broken" bitrate="600" width="640" height="360" bootstrapInfoId="boot"/>"#;
    let rows = if valid_first {
        format!("{valid}{broken}")
    } else {
        format!("{broken}{valid}")
    };
    format!(
        r#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><streamType>recorded</streamType><duration>2</duration><baseURL>media/</baseURL>{rows}<bootstrapInfo id="boot" url="bootstrap.bin"/></manifest>"#
    )
    .into_bytes()
}

/// Строит VOD `abst/asrt/afrt` с двумя fragments и terminal marker-ом.
fn vod_bootstrap() -> Vec<u8> {
    let segment_table = segment_run_table(2);
    let fragment_table = fragment_run_table();
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.push(0);
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(b"fixture\0");
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(b"\0\0");
    payload.push(1);
    payload.extend_from_slice(&segment_table);
    payload.push(1);
    payload.extend_from_slice(&fragment_table);
    iso_box(b"abst", &payload)
}

/// `asrt` сопоставляет оба fragments одному segment-у.
fn segment_run_table(fragments_per_segment: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&fragments_per_segment.to_be_bytes());
    iso_box(b"asrt", &payload)
}

/// `afrt` задаёт два секундных media fragment-а и конец presentation.
fn fragment_run_table() -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.push(0);
    payload.extend_from_slice(&2_u32.to_be_bytes());
    append_fragment_run(&mut payload, 1, 0, 1_000, None);
    // Unified Streaming использует нулевой ID для terminal END_OF_PRESENTATION.
    append_fragment_run(&mut payload, 0, 0, 0, Some(0));
    iso_box(b"afrt", &payload)
}

/// Кодирует одну wire-level Adobe FRAGMENTRUNENTRY.
fn append_fragment_run(
    payload: &mut Vec<u8>,
    first_fragment: u32,
    first_timestamp: u64,
    duration: u32,
    discontinuity: Option<u8>,
) {
    payload.extend_from_slice(&first_fragment.to_be_bytes());
    payload.extend_from_slice(&first_timestamp.to_be_bytes());
    payload.extend_from_slice(&duration.to_be_bytes());
    if let Some(indicator) = discontinuity {
        payload.push(indicator);
    }
}

/// Собирает доставляемый HDS media fragment с `afra/moof/mdat` topology.
///
/// Bootstrap тестовый сервер отдаёт отдельно через `/media/bootstrap.bin`, поэтому его
/// повторение здесь скрывало бы реальную границу между provider и FLV demux adapter.
fn f4f_fragment(timestamp: u32) -> Vec<u8> {
    let mut flv_tags = flv_tag(9, timestamp, &avc_sequence());
    flv_tags.extend_from_slice(&flv_tag(8, timestamp, &aac_sequence()));
    flv_tags.extend_from_slice(&flv_tag(9, timestamp + 40, &avc_keyframe()));
    flv_tags.extend_from_slice(&flv_tag(8, timestamp + 40, &aac_frame(&[0x11, 0x22])));

    let mut fragment = f4f_afra();
    fragment.extend_from_slice(&f4f_moof());
    fragment.extend_from_slice(&iso_box(b"mdat", &flv_tags));
    fragment
}

/// Минимальный valid `afra` для production F4F topology validator-а.
fn f4f_afra() -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    iso_box(b"afra", &payload)
}

/// Минимальный `moof` с одним declared track run.
fn f4f_moof() -> Vec<u8> {
    let mut movie_header = vec![0, 0, 0, 0];
    movie_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_header = vec![0, 0, 0, 0];
    track_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_run = vec![0, 0, 0, 0];
    track_run.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_fragment = iso_box(b"tfhd", &track_header);
    track_fragment.extend_from_slice(&iso_box(b"trun", &track_run));
    let mut payload = iso_box(b"mfhd", &movie_header);
    payload.extend_from_slice(&iso_box(b"traf", &track_fragment));
    iso_box(b"moof", &payload)
}

/// Кодирует ISO BMFF box только для test fixture bytes; parsing остаётся production-owned.
fn iso_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8 + payload.len()).expect("HDS fixture box fits u32");
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(payload);
    bytes
}

/// Кодирует headerless FLV tag, который F4F adapter обязан извлечь из `mdat`.
fn flv_tag(tag_type: u8, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let payload_size = u32::try_from(payload.len()).expect("FLV fixture payload fits u32");
    let timestamp_bytes = timestamp.to_be_bytes();
    let mut bytes = Vec::new();
    bytes.push(tag_type);
    bytes.extend_from_slice(&payload_size.to_be_bytes()[1..]);
    bytes.extend_from_slice(&timestamp_bytes[1..]);
    bytes.push(timestamp_bytes[0]);
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(payload);
    let tag_size = u32::try_from(11 + payload.len()).expect("FLV fixture tag fits u32");
    bytes.extend_from_slice(&tag_size.to_be_bytes());
    bytes
}

/// Возвращает AVC sequence header с маленьким production-validated `avcC`.
fn avc_sequence() -> Vec<u8> {
    let mut payload = vec![0x17, 0, 0, 0, 0];
    payload.extend_from_slice(&[1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 1, 0x68]);
    payload
}

/// Возвращает minimal length-prefixed IDR access unit.
fn avc_keyframe() -> Vec<u8> {
    vec![0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 0]
}

/// Возвращает AAC-LC AudioSpecificConfig.
fn aac_sequence() -> Vec<u8> {
    vec![0xaf, 0, 0x12, 0x10]
}

/// Возвращает один raw AAC payload за FLV AAC packet header-ом.
fn aac_frame(frame: &[u8]) -> Vec<u8> {
    let mut payload = vec![0xaf, 1];
    payload.extend_from_slice(frame);
    payload
}
