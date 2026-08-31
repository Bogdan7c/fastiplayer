//! Provider-owned catalog discovery over real bounded transport and demux probes.

#[allow(dead_code)]
mod support;

use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use demux_api::ProgressiveAsyncSeekLimits;
use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration,
    TrackInfo, TrackKind,
};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, muxed_ts, open_policy, response,
};
use web_media_core::{
    AudioTrackDescriptor, CandidateFormatIdentity, CandidateIdentity,
    ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, DynamicRange, ExactSelectionIdentity,
    ExtractionGeneration, NormalizedCodec, RawCodecIdentity, SemanticIdentity, SourceIdentity,
    VideoTrackDescriptor,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsCatalogBuildPolicy, HlsCatalogCapabilityProofPort,
    HlsCatalogCapabilityRejection, HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest,
    HlsCatalogPresentation, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest,
    discover_hls_catalog, prepare_hls_catalog_live_receipted, prepare_hls_catalog_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

#[derive(Default)]
struct RecordingCapabilities {
    video_calls: usize,
    audio_calls: usize,
}

const TEST_TIMEOUT: Duration = Duration::from_secs(4);

impl HlsCatalogCapabilityProofPort for RecordingCapabilities {
    fn prove_video(
        &mut self,
        track: &TrackInfo,
    ) -> Result<VideoTrackDescriptor, HlsCatalogCapabilityRejection> {
        assert_eq!(track.kind, TrackKind::Video);
        self.video_calls += 1;
        Ok(VideoTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("avc1.42001e").expect("video codec")),
            None,
            None,
            None,
            None,
            DynamicRange::Unknown,
        ))
    }

    fn prove_audio(
        &mut self,
        track: &TrackInfo,
    ) -> Result<AudioTrackDescriptor, HlsCatalogCapabilityRejection> {
        assert_eq!(track.kind, TrackKind::Audio);
        self.audio_calls += 1;
        Ok(AudioTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").expect("audio codec")),
            None,
            None,
            None,
            None,
        ))
    }
}

fn catalog_identity() -> ComponentVariantCatalogIdentity {
    let source = SourceIdentity::new(91);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("hls-catalog-runtime").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "hls-catalog-runtime").expect("semantic identity");
    ComponentVariantCatalogIdentity::new(
        ExactSelectionIdentity::new(exact, semantic).expect("same source"),
        ComponentVariantCatalogGeneration::new(1),
    )
}

fn next_ready_event(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer
            .next_event()
            .expect("catalog runtime remains readable")
        {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "catalog runtime timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

struct RotatingRefreshPort {
    fresh_target: HttpRequestTarget,
    cancellation: CancellationToken,
    refreshed: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl HlsEndpointRefreshPort for RotatingRefreshPort {
    fn refresh(
        &self,
        request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let generation = SourceGeneration::new(
            request
                .previous_generation
                .value()
                .checked_add(1)
                .expect("test generation space"),
        );
        self.refreshed.store(true, Ordering::SeqCst);
        Ok(HlsEndpointRefreshReply {
            http: adaptive_context(
                &self.fresh_target,
                self.cancellation.clone(),
                generation,
                TestQueries::default(),
            ),
            generation,
            manifest: HlsManifestInput::Fetch {
                selected_url: self.fresh_target.clone(),
            },
            overrides: HlsRequestOverrides::new(None),
        })
    }
}

#[test]
fn discovery_content_proves_selected_child_and_isolates_unavailable_sibling() {
    let segment = muxed_ts(90_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.42001e,mp4a.40.2\",RESOLUTION=640x360\n\
                  selected.m3u8\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"avc1.42001e,mp4a.40.2\",RESOLUTION=1280x720\n\
                  unavailable.m3u8\n",
            );
        }
        if request.request_line.contains("/selected.m3u8") {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
            );
        }
        if request.request_line.contains("/segment.ts") {
            return response("200 OK", &[], &segment);
        }
        response("404 Not Found", &[], b"")
    });
    let generation = SourceGeneration::new(1);
    let target = server.target("/master.m3u8");
    let mut open = HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::Fetch {
            selected_url: target,
        },
        selection: HlsVariantSelectionIntent {
            resolution: Some((
                NonZeroU32::new(640).expect("width"),
                NonZeroU32::new(360).expect("height"),
            )),
            codecs: Some("avc1.42001e,mp4a.40.2".into()),
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    };
    let mut capabilities = RecordingCapabilities::default();
    let outcome = discover_hls_catalog(
        HlsCatalogDiscoveryRequest {
            open: &open,
            catalog_identity: catalog_identity(),
            presentation: HlsCatalogPresentation::Vod,
            provider_default_variant_index: None,
            policy: HlsCatalogBuildPolicy {
                catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
                compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
                maximum_unique_children: NonZeroUsize::new(8).expect("child limit"),
            },
        },
        &mut capabilities,
    )
    .expect("selected child remains authoritative");
    let HlsCatalogDiscoveryOutcome::Installed(snapshot) = outcome else {
        panic!("master discovery must install a catalog");
    };

    assert_eq!(capabilities.video_calls, 1);
    assert_eq!(capabilities.audio_calls, 1);
    assert_eq!(snapshot.catalog().coupled_presentations().len(), 1);
    assert_eq!(snapshot.sibling_rejections().len(), 1);

    let reopen = snapshot
        .reopen_exact(snapshot.provider_default_selection())
        .expect("canonical catalog selection");
    open.selection.resolution = Some((
        NonZeroU32::new(1280).expect("width"),
        NonZeroU32::new(720).expect("height"),
    ));
    let opened = prepare_hls_catalog_vod_receipted(
        open,
        reopen,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("catalog exact reopen ignores unrelated caller default");
    let mut demuxer = opened.into_demuxer();
    assert!(matches!(
        next_ready_event(demuxer.as_mut()),
        DemuxReadEvent::TracksChanged(_)
    ));
}

#[test]
fn live_catalog_reopen_semantically_tracks_rotated_child_after_endpoint_replacement() {
    let first = muxed_ts(90_000);
    let second = muxed_ts(180_000);
    let third = muxed_ts(270_000);
    let server = TestServer::start(move |_, request| {
        if request
            .request_line
            .starts_with("GET /initial-master.m3u8 ")
        {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.42001e,mp4a.40.2\",RESOLUTION=640x360\nold/live.m3u8?token=old\n",
            );
        }
        if request.request_line.starts_with("GET /fresh-master.m3u8 ") {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"mp4a.40.2,avc1.42001e\",RESOLUTION=640x360\nnew/live.m3u8?token=fresh\n",
            );
        }
        if request
            .request_line
            .starts_with("GET /old/live.m3u8?token=old ")
        {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n",
            );
        }
        if request
            .request_line
            .starts_with("GET /new/live.m3u8?token=fresh ")
        {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:2\n#EXTINF:1,\nb.ts\n#EXTINF:1,\nc.ts\n",
            );
        }
        if request.request_line.starts_with("GET /old/a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /old/b.ts ") {
            return response("410 Gone", &[], b"");
        }
        if request.request_line.starts_with("GET /new/b.ts ") {
            return response("200 OK", &[], &second);
        }
        if request.request_line.starts_with("GET /new/c.ts ") {
            return response("200 OK", &[], &third);
        }
        response("404 Not Found", &[], b"")
    });
    let initial_target = server.target("/initial-master.m3u8");
    let generation = SourceGeneration::new(1);
    let selection = HlsVariantSelectionIntent {
        resolution: Some((
            NonZeroU32::new(640).expect("width"),
            NonZeroU32::new(360).expect("height"),
        )),
        codecs: Some("avc1.42001e,mp4a.40.2".into()),
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    };
    let discovery_open = HlsVodOpenRequest {
        http: adaptive_context(
            &initial_target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::Fetch {
            selected_url: initial_target.clone(),
        },
        selection: selection.clone(),
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    };
    let mut capabilities = RecordingCapabilities::default();
    let HlsCatalogDiscoveryOutcome::Installed(snapshot) = discover_hls_catalog(
        HlsCatalogDiscoveryRequest {
            open: &discovery_open,
            catalog_identity: catalog_identity(),
            presentation: HlsCatalogPresentation::Live,
            provider_default_variant_index: None,
            policy: HlsCatalogBuildPolicy {
                catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
                compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
                maximum_unique_children: NonZeroUsize::new(8).expect("child limit"),
            },
        },
        &mut capabilities,
    )
    .expect("live catalog discovery") else {
        panic!("master must produce live catalog");
    };
    let reopen = snapshot
        .reopen_exact(snapshot.provider_default_selection())
        .expect("live catalog selection");
    let cancellation = CancellationToken::new();
    let refreshed = Arc::new(AtomicBool::new(false));
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let endpoint_refresh = Arc::new(RotatingRefreshPort {
        fresh_target: server.target("/fresh-master.m3u8"),
        cancellation: cancellation.clone(),
        refreshed: Arc::clone(&refreshed),
        calls: Arc::clone(&refresh_calls),
    });
    let live = HlsLiveOpenRequest {
        common: HlsVodOpenRequest {
            http: adaptive_context(
                &initial_target,
                cancellation,
                generation,
                TestQueries::default(),
            ),
            generation,
            manifest: HlsManifestInput::Fetch {
                selected_url: initial_target,
            },
            selection,
            overrides: HlsRequestOverrides::new(None),
            containers: HlsComponentContainerIntent {
                main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
                alternate_audio: None,
            },
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        endpoint_refresh,
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("timeline port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    };
    let opened = prepare_hls_catalog_live_receipted(
        live,
        reopen,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("exact initial live catalog reopen");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut packet_after_replacement = false;
    while Instant::now() < deadline {
        match demuxer
            .next_event()
            .expect("semantic replacement remains readable")
        {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::Packet(_) if refreshed.load(Ordering::SeqCst) => {
                let used_rotated_child = server
                    .requests()
                    .iter()
                    .any(|request| request.request_line.contains("/new/live.m3u8?token=fresh"));
                if used_rotated_child {
                    packet_after_replacement = true;
                    break;
                }
            }
            _ => {}
        }
    }
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    assert!(
        packet_after_replacement,
        "rotated semantic child was not resumed"
    );
}
