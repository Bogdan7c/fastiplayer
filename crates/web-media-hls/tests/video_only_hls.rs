//! A13-2: HLS без звука (video-only media playlist) открывается и играет.
//!
//! Тест идёт настоящим путём native-ingress: admission top media playlist -> intent и
//! container evidence из admission -> `prepare_hls_vod`/`prepare_hls_live` -> пакеты.
//! Раньше admission требовал форму `MuxedAv`, и видео без звука отвергалось ошибкой
//! «HLS main track shape mismatch: expected MuxedAv, video=1, audio=0».

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)] // Shared fixture экспортирует больше builders, чем нужно этому focused binary.
mod support;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::{Duration, Instant};

use demux_api::{ProgressiveAsyncSeekLimits, ProgressiveSeekFence, ProgressiveSeekRequestId};
use hls_playlist_core::HlsParserLimits;
use media_core::TrackInfo;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, DynamicMediaTimelineEpoch,
    DynamicMediaTimelinePortGeneration, Packet, PacketKeyframe, TrackKind,
};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, decode_base64, demux_registry, muxed_fmp4, muxed_ts,
    open_policy, response, video_ts,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, CodecFamily, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit, ComponentVariantEdgeLimit,
    DynamicRange, ExactSelectionIdentity, ExtractionGeneration, NormalizedCodec,
    PreferredHeightPolicy, RawCodecIdentity, SemanticIdentity, SourceIdentity,
    VideoTrackDescriptor,
};
use web_media_hls::{
    HlsCatalogBuildPolicy, HlsCatalogCapabilityProofPort, HlsCatalogCapabilityRejection,
    HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest, HlsCatalogPresentation,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsManifestInput, HlsProviderDefaultAudioPolicy,
    HlsRequestOverrides, HlsVodOpenRequest, NativeHlsCatalogAdmission, NativeHlsSelectionPolicy,
    SecretInlineMediaPlaylist, admit_native_hls_catalog, discover_hls_catalog,
    prepare_hls_catalog_vod_receipted, prepare_hls_live, prepare_hls_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);
const SEGMENT_SECONDS: u64 = 10;
const PTS_PER_SECOND: u64 = 90_000;

/// Video-only VP9 fMP4 (ffmpeg, 4 кадра): единственная video-only fMP4 фикстура харнесса.
const VIDEO_ONLY_FMP4_BASE64: &str = include_str!("fixtures/vp9-video-fmp4.base64");

fn policy() -> NativeHlsSelectionPolicy {
    NativeHlsSelectionPolicy::new(
        PreferredHeightPolicy::NoPreference,
        vec![CodecFamily::H264, CodecFamily::Vp9, CodecFamily::Av1],
    )
    .expect("valid policy")
}

/// Admission ровно так, как его делает production: top media playlist без master.
fn admit(playlist: &str, target: &HttpRequestTarget) -> NativeHlsCatalogAdmission {
    admit_native_hls_catalog(
        playlist.as_bytes(),
        target,
        HlsParserLimits::default(),
        &policy(),
    )
    .expect("video-only media playlist должен пройти native admission")
}

fn vod_request(
    server: &TestServer,
    playlist: &'static str,
    admission: &NativeHlsCatalogAdmission,
) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let target = server.target("/media.m3u8");
    HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::InlineMedia {
            selected_url: target,
            playlist: SecretInlineMediaPlaylist::new(playlist),
        },
        selection: admission.runtime_intent(),
        overrides: HlsRequestOverrides::new(None),
        containers: admission.container_intent(),
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

fn next_ready(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event().expect("HLS runtime читается") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "HLS worker не стал готов");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

/// Читает события до первого пакета; попутно собирает виды опубликованных дорожек.
fn first_packet_and_track_kinds(demuxer: &mut dyn Demuxer) -> (Packet, Vec<TrackKind>) {
    let mut track_kinds = Vec::new();
    loop {
        match next_ready(demuxer) {
            DemuxReadEvent::TracksChanged(update) => {
                track_kinds = update.tracks.iter().map(|track| track.kind).collect();
            }
            DemuxReadEvent::Packet(packet) => return (packet, track_kinds),
            DemuxReadEvent::EndOfStream => panic!("поток закончился до первого пакета"),
            _ => {}
        }
    }
}

fn vod_ts_playlist() -> &'static str {
    "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXT-X-PLAYLIST-TYPE:VOD\n\
     #EXTINF:10,\nseg0.ts\n#EXTINF:10,\nseg1.ts\n#EXTINF:10,\nseg2.ts\n#EXT-X-ENDLIST\n"
}

fn serve_video_only_ts() -> TestServer {
    TestServer::start(move |_, request| {
        for index in 0..3_u64 {
            if request.request_line.contains(&format!("/seg{index}.ts")) {
                let pts = index * SEGMENT_SECONDS * PTS_PER_SECOND;
                return response("200 OK", &[], &video_ts(pts));
            }
        }
        response("404 Not Found", &[], b"")
    })
}

/// Video-only ts VOD: admission принимает, длительность - сумма EXTINF, пакеты видео,
/// звуковой дорожки нет, а перемотка садится на keyframe нужного сегмента.
#[test]
fn video_only_ts_vod_opens_reaches_video_packets_and_seeks() {
    let server = serve_video_only_ts();
    let admission = admit(vod_ts_playlist(), &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        vod_request(&server, vod_ts_playlist(), &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("video-only ts VOD должен открыться");
    assert_eq!(opened.duration(), Duration::from_secs(30));
    let seek_handle = opened.async_seek_handle().expect("receipted seek handle");
    let mut demuxer = opened.into_demuxer();

    let (packet, track_kinds) = first_packet_and_track_kinds(demuxer.as_mut());
    assert_eq!(track_kinds, vec![TrackKind::Video], "звука быть не должно");
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);

    // Перемотка на 15 с: ближайший keyframe до цели - начало второго сегмента (10 с).
    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(15)),
        )
        .expect("enqueue seek");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let receipt = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "seek receipt не пришёл");
        std::thread::sleep(Duration::from_millis(2));
    };
    let demux_api::ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("seek video-only VOD должен удаться: {receipt:?}");
    };
    // В fixture-е PTS сегмента смещён на 3000 тиков (33 мс) относительно DTS.
    let landed = result.actual_position.as_duration();
    assert!(
        landed >= Duration::from_secs(10) && landed < Duration::from_millis(10_100),
        "seek должен сесть на keyframe второго сегмента, получено {landed:?}"
    );
    loop {
        if let DemuxReadEvent::Packet(packet) = next_ready(demuxer.as_mut()) {
            assert_eq!(packet.kind, TrackKind::Video);
            assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
            assert!(
                packet.pts >= Duration::from_secs(10),
                "после seek первый пакет не раньше anchor-а: {:?}",
                packet.pts
            );
            break;
        }
    }
}

/// Video-only fMP4 VOD (EXT-X-MAP): тот же путь, контейнер определяется по содержимому.
#[test]
fn video_only_fmp4_vod_opens_and_reaches_keyframe_packet() {
    let bytes = decode_base64(VIDEO_ONLY_FMP4_BASE64);
    let moof_at = bytes
        .windows(4)
        .position(|window| window == b"moof")
        .expect("в fixture есть moof")
        - 4;
    let init = Arc::new(bytes[..moof_at].to_vec());
    let media = Arc::new(bytes[moof_at..].to_vec());
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &init)
        } else if request.request_line.contains("/seg.m4s") {
            response("200 OK", &[], &media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    const PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n\
        #EXT-X-TARGETDURATION:1\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:0.4,\nseg.m4s\n\
        #EXT-X-ENDLIST\n";
    let admission = admit(PLAYLIST, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        vod_request(&server, PLAYLIST, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("video-only fMP4 VOD должен открыться");
    let mut demuxer = opened.into_demuxer();
    let (packet, track_kinds) = first_packet_and_track_kinds(demuxer.as_mut());
    assert_eq!(track_kinds, vec![TrackKind::Video]);
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

/// Регрессия: обычный muxed (видео + звук) через тот же admission по-прежнему отдаёт обе дорожки.
#[test]
fn muxed_ts_and_fmp4_still_open_with_both_tracks() {
    let (init, first, _) = muxed_fmp4();
    let (init, first, ts) = (Arc::new(init), Arc::new(first), Arc::new(muxed_ts(90_000)));
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &init)
        } else if request.request_line.contains(".m4s") {
            response("200 OK", &[], &first)
        } else if request.request_line.contains(".ts") {
            response("200 OK", &[], &ts)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    const TS: &str = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n\
        #EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n";
    const FMP4: &str = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n\
        #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\na.m4s\n#EXT-X-ENDLIST\n";
    for playlist in [TS, FMP4] {
        let admission = admit(playlist, &server.target("/media.m3u8"));
        let opened = prepare_hls_vod_receipted(
            vod_request(&server, playlist, &admission),
            ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
        )
        .expect("muxed VOD должен открыться");
        let mut demuxer = opened.into_demuxer();
        let (_, mut kinds) = first_packet_and_track_kinds(demuxer.as_mut());
        kinds.sort_by_key(|kind| matches!(kind, TrackKind::Audio));
        assert_eq!(kinds, vec![TrackKind::Video, TrackKind::Audio]);
    }
}

struct NoRefresh;
impl HlsEndpointRefreshPort for NoRefresh {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        Err(HlsEndpointRefreshError::Cancelled)
    }
}

/// Video-only live (без ENDLIST): открывается, отдаёт видео-пакеты с keyframe-ами.
#[test]
fn video_only_live_ts_opens_and_reaches_video_packets() {
    const PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n\
        #EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n";
    let first = video_ts(90_000);
    let second = video_ts(180_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /live.m3u8 ") {
            response("200 OK", &[], PLAYLIST.as_bytes())
        } else if request.request_line.starts_with("GET /a.ts ") {
            response("200 OK", &[], &first)
        } else if request.request_line.starts_with("GET /b.ts ") {
            response("200 OK", &[], &second)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let target = server.target("/live.m3u8");
    let admission = admit(PLAYLIST, &target);
    let generation = SourceGeneration::new(1);
    let opened = prepare_hls_live(HlsLiveOpenRequest {
        common: HlsVodOpenRequest {
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
            selection: admission.runtime_intent(),
            overrides: HlsRequestOverrides::new(None),
            containers: admission.container_intent(),
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        endpoint_refresh: Arc::new(NoRefresh),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    })
    .expect("video-only live должен открыться");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    let (packet, _) = first_packet_and_track_kinds(demuxer.as_mut());
    let track_kinds: Vec<TrackKind> = demuxer.tracks().iter().map(|track| track.kind).collect();
    assert_eq!(track_kinds, vec![TrackKind::Video]);
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

/// Capability-порт, который знает только видео: звуковой дорожки в проверяемом child нет.
struct VideoOnlyCapabilities;

impl HlsCatalogCapabilityProofPort for VideoOnlyCapabilities {
    fn prove_video(
        &mut self,
        track: &TrackInfo,
    ) -> Result<VideoTrackDescriptor, HlsCatalogCapabilityRejection> {
        assert_eq!(track.kind, TrackKind::Video);
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
        _track: &TrackInfo,
    ) -> Result<web_media_core::AudioTrackDescriptor, HlsCatalogCapabilityRejection> {
        panic!("в video-only child не должно быть аудио-дорожки");
    }
}

/// Master с variant-ом без audio-кодека в CODECS: native admission его принимает, каталог
/// доказывает video-only, provider default = video-only, и воспроизведение доходит до видео.
#[test]
fn master_variant_without_audio_codec_plays_video_only_through_catalog() {
    let segment = video_ts(90_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.42001e\",\
                  RESOLUTION=640x360\nchild.m3u8\n",
            )
        } else if request.request_line.contains("/child.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n",
            )
        } else if request.request_line.contains("/segment.ts") {
            response("200 OK", &[], &segment)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let target = server.target("/master.m3u8");
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.42001e\",\
                  RESOLUTION=640x360\nchild.m3u8\n";
    let admission = admit(master, &target);
    let generation = SourceGeneration::new(1);
    let open = HlsVodOpenRequest {
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
        selection: admission.runtime_intent(),
        overrides: HlsRequestOverrides::new(None),
        containers: admission.container_intent(),
        demux_registry: demux_registry(),
        policy: open_policy(),
    };
    let source = SourceIdentity::new(92);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("hls-video-only").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "hls-video-only").expect("semantic identity");
    let outcome = discover_hls_catalog(
        HlsCatalogDiscoveryRequest {
            open: &open,
            catalog_identity: ComponentVariantCatalogIdentity::new(
                ExactSelectionIdentity::new(exact, semantic).expect("same source"),
                ComponentVariantCatalogGeneration::new(1),
            ),
            presentation: HlsCatalogPresentation::Vod,
            provider_default_variant_index: admission.current_master_variant_index(),
            policy: HlsCatalogBuildPolicy {
                catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
                compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
                maximum_unique_children: NonZeroUsize::new(8).expect("child limit"),
                provider_default_audio: HlsProviderDefaultAudioPolicy::RequireDeclared,
            },
        },
        &mut VideoOnlyCapabilities,
    )
    .expect("video-only master должен пройти discovery");
    let HlsCatalogDiscoveryOutcome::Installed(snapshot) = outcome else {
        panic!("master discovery must install a catalog");
    };
    assert!(matches!(
        snapshot.provider_default_selection(),
        web_media_core::ComponentVariantSelection::VideoOnly { .. }
    ));
    assert!(snapshot.catalog().coupled_presentations().is_empty());

    let reopen = snapshot
        .reopen_exact(snapshot.provider_default_selection())
        .expect("provider default video-only");
    let opened = prepare_hls_catalog_vod_receipted(
        open,
        reopen,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("video-only master child должен открыться");
    let mut demuxer = opened.into_demuxer();
    let (packet, track_kinds) = first_packet_and_track_kinds(demuxer.as_mut());
    assert_eq!(track_kinds, vec![TrackKind::Video]);
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}
