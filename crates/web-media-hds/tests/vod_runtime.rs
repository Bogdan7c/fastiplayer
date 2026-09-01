//! Hermetic S38 acceptance evidence: local F4M/bootstrap/F4F проходят production runtime.

use std::collections::HashMap;
use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveRetryPolicy, AdaptiveTransportLimits,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantSelection, ExactSelectionIdentity,
    ExtractionGeneration, PreferredHeightPolicy, SemanticIdentity, SourceIdentity,
};
use web_media_hds::{
    HdsCatalogDiscoveryRequest, HdsFetchedCatalogDiscoveryRequest, HdsFetchedManifestInput,
    HdsNoPlayableRendition, HdsRenditionCapabilityProbe, HdsRenditionCapabilityRejection,
    HdsRenditionSelection, HdsVodOpenPolicy, HdsVodOpenRequest, discover_fetched_hds_renditions,
    discover_hds_renditions, prepare_discovered_hds_vod, prepare_hds_vod,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

#[path = "support/http_server.rs"]
mod http_server;

use http_server::HermeticHttpServer;

/// Общий deadline ограничивает только ожидание worker-а в test thread-е.
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Доказывает полный positive path вместо отдельного parser-only smoke.
#[test]
fn prepares_local_f4m_bootstrap_and_f4f_until_tracks_and_packet() {
    // Первый fragment намеренно содержит только video config: one-segment sniff
    // обязан открыть F4F, а transactional demuxer — дочитать второй до exact A/V.
    let first_fragment = f4f_video_configuration_fragment(0);
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
    assert_eq!(
        requested_paths
            .iter()
            .filter(|path| path.as_str() == "/media/videoSeg1-Frag1")
            .count(),
        1,
        "one-segment sniff must replay the first fragment instead of fetching it twice"
    );

    cancellation.cancel();
    drop(demuxer);
}

/// Direct-ingress handoff переиспользует root response и eager-probed Frag1.
#[test]
fn fetched_root_discovery_does_not_repeat_manifest_or_selected_initial_fragment() {
    let server = HermeticHttpServer::start(HashMap::from([
        ("/manifest.f4m", vod_manifest()),
        ("/media/bootstrap.bin", vod_bootstrap()),
        ("/media/videoSeg1-Frag1", f4f_fragment(0)),
        ("/media/videoSeg1-Frag2", f4f_fragment(1_000)),
    ]));
    let cancellation = CancellationToken::new();
    let root_target = server.target("/manifest.f4m");
    let transport = transport_request(&root_target, cancellation.clone());
    let source_config = source_config();
    let policy = open_policy();
    let http = AdaptiveHttpContext::new(
        transport.clone(),
        &source_config,
        policy.adaptive_limits,
        policy.adaptive_retry,
    )
    .expect("fetched HDS context");
    let fetched = http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            transport.source_generation(),
            root_target.clone(),
            policy.adaptive_limits.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        ))
        .expect("initial direct root fetch");
    let capabilities = FixtureHdsCapabilities::default();
    let discovered = discover_fetched_hds_renditions(HdsFetchedCatalogDiscoveryRequest {
        discovery: HdsCatalogDiscoveryRequest {
            transport_request: transport,
            source_config,
            demux_registry: f4f_registry(),
            policy,
            catalog_identity: catalog_identity(17),
            capability_probe: &capabilities,
            preferred_height: PreferredHeightPolicy::NoPreference,
        },
        fetched_manifest: HdsFetchedManifestInput::new(root_target, http, fetched),
    })
    .expect("fetched root discovery");
    let ComponentVariantSelection::Coupled { presentation, .. } = discovered.provider_default()
    else {
        panic!("HDS provider default remains coupled");
    };
    let selected_exact = presentation.exact_identity().clone();
    let opened = prepare_discovered_hds_vod(discovered, selected_exact)
        .expect("selected eager-probed HDS row opens");

    let requested_paths = server.requested_paths();
    assert_eq!(
        requested_paths
            .iter()
            .filter(|path| path.as_str() == "/manifest.f4m")
            .count(),
        1,
        "fetched root handoff must not issue a second manifest GET"
    );
    assert_eq!(
        requested_paths
            .iter()
            .filter(|path| path.as_str() == "/media/videoSeg1-Frag1")
            .count(),
        1,
        "selected eager probe must hand its demuxer to runtime"
    );

    cancellation.cancel();
    drop(opened);
}

#[test]
fn live_presentation_is_rejected_before_any_manifest_fetch() {
    let server = HermeticHttpServer::start(HashMap::new());
    let root_target = server.target("/live.f4m");
    let request = HdsVodOpenRequest {
        transport_request: transport_request_for_presentation(
            &root_target,
            MediaPresentation::Live,
            CancellationToken::new(),
        ),
        source_config: source_config(),
        demux_registry: f4f_registry(),
        policy: open_policy(),
        selection: HdsRenditionSelection::BestByPreference(PreferredHeightPolicy::NoPreference),
    };

    let error = match prepare_hds_vod(request) {
        Ok(_) => panic!("VOD-only HDS runtime не должен принимать live presentation"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("accepts only VOD"));
    assert!(
        server.requested_paths().is_empty(),
        "presentation contract должен быть проверен до network side effects"
    );
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

    let typed_error = error
        .downcast_ref::<HdsNoPlayableRendition>()
        .expect("all content/capability rejections must preserve typed parent fallback");
    assert!(
        typed_error
            .to_string()
            .contains("no probed playable rendition"),
        "terminal fallback diagnostic должен называть отсутствие playable rendition"
    );
}

#[test]
fn malformed_siblings_are_reported_without_hiding_a_playable_rendition() {
    let server = HermeticHttpServer::start(HashMap::from([
        ("/mixed.f4m", mixed_rejection_manifest()),
        ("/media/bootstrap.bin", vod_bootstrap()),
        ("/media/validSeg1-Frag1", f4f_fragment(0)),
        ("/media/validSeg1-Frag2", f4f_fragment(1_000)),
    ]));
    let target = server.target("/mixed.f4m");
    let capabilities = FixtureHdsCapabilities::default();

    let discovered = discover_hds_renditions(HdsCatalogDiscoveryRequest {
        transport_request: transport_request(&target, CancellationToken::new()),
        source_config: source_config(),
        demux_registry: f4f_registry(),
        policy: open_policy(),
        catalog_identity: catalog_identity(1),
        capability_probe: &capabilities,
        preferred_height: PreferredHeightPolicy::NoPreference,
    })
    .expect("валидная HDS row должна пережить malformed siblings");

    assert_eq!(discovered.catalog().coupled_presentations().len(), 1);
    let rejection_reasons = discovered
        .rejections()
        .iter()
        .copied()
        .map(web_media_hds::HdsRenditionRejection::reason)
        .collect::<Vec<_>>();
    assert!(
        rejection_reasons
            .contains(&web_media_hds::HdsRenditionRejectionReason::MalformedManifestRow)
    );
    assert!(
        rejection_reasons.contains(&web_media_hds::HdsRenditionRejectionReason::InvalidLocator)
    );

    let diagnostic = format!("{discovered:?}");
    assert!(diagnostic.contains("published_rows: 1"));
    assert!(diagnostic.contains("rejected_rows: 2"));
    assert!(!diagnostic.contains(target.expose_secret_for_request()));
}

/// Доказывает user-visible startup contract на полном production path-е:
/// медленные sibling fragments пробуются с caller-owned bound, complete catalog
/// сохраняется, а выбранный rendition доходит до настоящего demux packet-а.
#[test]
fn bounds_parallel_slow_rendition_probes_and_opens_selected_packet() {
    const MEDIA_RESPONSE_DELAY: Duration = Duration::from_millis(400);
    const SEQUENTIAL_LOWER_BOUND: Duration = Duration::from_millis(1_600);
    let first_fragment = f4f_fragment(0);
    let second_fragment = f4f_fragment(1_000);
    let third_fragment = f4f_fragment(2_000);
    let server = HermeticHttpServer::start_with_media_delay(
        HashMap::from([
            ("/manifest.f4m", parallel_discovery_manifest()),
            ("/media/bootstrap.bin", parallel_vod_bootstrap()),
            ("/media/lowSeg1-Frag1", first_fragment.clone()),
            ("/media/lowSeg1-Frag2", second_fragment.clone()),
            ("/media/lowSeg1-Frag3", third_fragment.clone()),
            ("/media/mediumSeg1-Frag1", first_fragment.clone()),
            ("/media/mediumSeg1-Frag2", second_fragment.clone()),
            ("/media/mediumSeg1-Frag3", third_fragment.clone()),
            ("/media/highSeg1-Frag1", first_fragment.clone()),
            ("/media/highSeg1-Frag2", second_fragment.clone()),
            ("/media/highSeg1-Frag3", third_fragment.clone()),
            ("/media/maximumSeg1-Frag1", first_fragment),
            ("/media/maximumSeg1-Frag2", second_fragment),
            ("/media/maximumSeg1-Frag3", third_fragment),
        ]),
        MEDIA_RESPONSE_DELAY,
    );
    let target = server.target("/manifest.f4m");
    let capabilities = FixtureHdsCapabilities::default();
    let discovery_started_at = Instant::now();
    let discovered = discover_hds_renditions(HdsCatalogDiscoveryRequest {
        transport_request: transport_request(&target, CancellationToken::new()),
        source_config: source_config(),
        demux_registry: f4f_registry(),
        policy: open_policy(),
        catalog_identity: catalog_identity(1),
        capability_probe: &capabilities,
        preferred_height: PreferredHeightPolicy::NoPreference,
    })
    .expect("bounded parallel HDS discovery succeeds");
    let discovery_elapsed = discovery_started_at.elapsed();

    assert_eq!(
        discovered.catalog().coupled_presentations().len(),
        4,
        "parallel scheduling must not turn complete catalog discovery into an early exit"
    );
    assert_eq!(capabilities.checked_rows.load(Ordering::Acquire), 4);
    assert_eq!(
        server.maximum_concurrent_media_requests(),
        2,
        "fixture must observe the caller-owned two-probe concurrency bound"
    );
    assert_eq!(
        server
            .requested_paths()
            .iter()
            .filter(|path| path.as_str() == "/media/bootstrap.bin")
            .count(),
        1,
        "shared external bootstrap must be fetched once per manifest snapshot"
    );
    assert!(
        discovery_elapsed < SEQUENTIAL_LOWER_BOUND,
        "four first-fragment probes must overlap instead of taking the sequential lower bound: {discovery_elapsed:?}"
    );
    assert!(
        server
            .requested_paths()
            .iter()
            .all(|path| !path.ends_with("Seg1-Frag2") && !path.ends_with("Seg1-Frag3")),
        "catalog proof must not eagerly download successor fragments from every rendition"
    );
    server.reset_maximum_concurrent_media_requests();

    let ComponentVariantSelection::Coupled { presentation, .. } =
        discovered.provider_default().clone()
    else {
        panic!("HDS provider default must remain coupled");
    };
    let opened = prepare_discovered_hds_vod(discovered, presentation.exact_identity().clone())
        .expect("selected rendition reuses its content-probed demuxer");
    let seek_handle = opened.async_seek_handle();
    let mut demuxer = opened.into_demuxer();
    wait_for_requested_path(&server, "/media/maximumSeg1-Frag2");
    wait_for_requested_path(&server, "/media/maximumSeg1-Frag3");
    assert_eq!(
        server.maximum_concurrent_media_requests(),
        2,
        "selected runtime должен перекрыть latency двух successor fetch-ов"
    );
    assert_eq!(
        server
            .requested_paths()
            .iter()
            .filter(|path| path.as_str() == "/media/maximumSeg1-Frag2")
            .count(),
        1,
        "selected runtime должен начать ровно один successor fetch до packet consumption"
    );
    assert_eq!(
        server
            .requested_paths()
            .iter()
            .filter(|path| path.as_str() == "/media/maximumSeg1-Frag3")
            .count(),
        1,
        "selected runtime должен запросить второй FIFO successor ровно один раз"
    );
    let mut packet_seen = false;
    for _ in 0..16 {
        match next_ready_event(demuxer.as_mut()) {
            DemuxReadEvent::Packet(_) => {
                packet_seen = true;
                break;
            }
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                unreachable!("next_ready_event filters readiness events")
            }
        }
    }
    assert!(
        packet_seen,
        "selected slow HDS rendition must reach a demux packet"
    );

    server.reset_maximum_concurrent_media_requests();
    let seek_fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            seek_fence,
            DemuxSeekRequest::accurate(Duration::from_millis(1_500)),
        )
        .expect("selected seek request is accepted");
    let seek_receipt = wait_for_seek_receipt(&seek_handle);
    assert_eq!(seek_receipt.fence, seek_fence);
    let ProgressiveAsyncSeekOutcome::Succeeded(seek_result) = seek_receipt.outcome else {
        panic!(
            "selected HDS seek must succeed, got {:?}",
            seek_receipt.outcome
        );
    };
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_secs(1),
        "seek должен подтвердить preceding fragment anchor"
    );
    assert_eq!(
        server.maximum_concurrent_media_requests(),
        2,
        "seek replacement должен одновременно готовить anchor и successor"
    );
    assert_eq!(
        server
            .requested_paths()
            .iter()
            .filter(|path| path.as_str() == "/media/maximumSeg1-Frag2")
            .count(),
        2,
        "seek должен повторно запросить anchor ровно один раз"
    );
    assert_eq!(
        server
            .requested_paths()
            .iter()
            .filter(|path| path.as_str() == "/media/maximumSeg1-Frag3")
            .count(),
        2,
        "seek должен повторно запросить successor ровно один раз"
    );

    let mut post_seek_packet_seen = false;
    for _ in 0..16 {
        match next_ready_event(demuxer.as_mut()) {
            DemuxReadEvent::Packet(_) => {
                post_seek_packet_seen = true;
                break;
            }
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                unreachable!("next_ready_event filters readiness events")
            }
        }
    }
    assert!(
        post_seek_packet_seen,
        "concurrent seek replacement must continue through a real demux packet"
    );
}

/// Ждёт network request от уже выбранного runtime-а без consumer demux poll-а.
fn wait_for_requested_path(server: &HermeticHttpServer, expected_path: &str) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !server
        .requested_paths()
        .iter()
        .any(|path| path == expected_path)
    {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for selected HDS read-ahead request {expected_path}"
        );
        thread::sleep(Duration::from_millis(2));
    }
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
    transport_request_for_presentation(target, MediaPresentation::Vod, cancellation)
}

fn transport_request_for_presentation(
    target: &HttpRequestTarget,
    presentation: MediaPresentation,
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
        presentation,
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
            Duration::from_millis(5),
        )
        .expect("HDS retry policy"),
        demux_sniff_budget: DemuxSniffBudget::new(
            non_zero(64 * 1_024),
            non_zero(1),
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
        maximum_parallel_rendition_probes: non_zero(2),
        maximum_buffered_fragments: non_zero(2),
        maximum_concurrent_fragment_fetches: non_zero(2),
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

/// Валидная row сосуществует с parser-level и locator-level content rejections.
fn mixed_rejection_manifest() -> Vec<u8> {
    br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><streamType>recorded</streamType><duration>2</duration><baseURL>media/</baseURL><media url="broken" width="wide"/><media href="http://["/><media url="valid" bitrate="1200" width="1280" height="720" bootstrapInfoId="boot"/><bootstrapInfo id="boot" url="bootstrap.bin"/></manifest>"#.to_vec()
}

/// Четыре валидных rows заставляют discovery доказать полный bounded parallel pass.
fn parallel_discovery_manifest() -> Vec<u8> {
    br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><streamType>recorded</streamType><duration>3</duration><baseURL>media/</baseURL><media url="low" bitrate="400" width="640" height="360" bootstrapInfoId="boot"/><media url="medium" bitrate="800" width="960" height="540" bootstrapInfoId="boot"/><media url="high" bitrate="1200" width="1280" height="720" bootstrapInfoId="boot"/><media url="maximum" bitrate="2000" width="1920" height="1080" bootstrapInfoId="boot"/><bootstrapInfo id="boot" url="bootstrap.bin"/></manifest>"#.to_vec()
}

/// Строит VOD `abst/asrt/afrt` с двумя fragments и terminal marker-ом.
fn vod_bootstrap() -> Vec<u8> {
    vod_bootstrap_with_fragment_count(2)
}

/// Три fragments дают selected runtime два одновременно готовящихся successor-а.
fn parallel_vod_bootstrap() -> Vec<u8> {
    vod_bootstrap_with_fragment_count(3)
}

/// Общий wire builder сохраняет один parser-owned bootstrap формат.
fn vod_bootstrap_with_fragment_count(fragment_count: u32) -> Vec<u8> {
    let segment_table = segment_run_table(fragment_count);
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

/// Строит первый F4F fragment только с video configuration для on-demand A/V discovery.
fn f4f_video_configuration_fragment(timestamp: u32) -> Vec<u8> {
    let mut fragment = f4f_afra();
    fragment.extend_from_slice(&f4f_moof());
    fragment.extend_from_slice(&iso_box(b"mdat", &flv_tag(9, timestamp, &avc_sequence())));
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
