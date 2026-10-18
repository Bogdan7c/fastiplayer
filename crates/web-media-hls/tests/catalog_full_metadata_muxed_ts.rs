//! Регрессия: обычный ffmpeg MPEG-TS HLS VOD (muxed H.264 High + AAC LC, полный набор
//! атрибутов `STREAM-INF`) обязан проходить native catalog и реально демультиплексироваться.
//!
//! Раньше ключ coupled-строки такого варианта превышал 256 байт, и открытие падало с
//! `ProviderDefaultRejected { DescriptorBounds }`.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)]
mod support;

use std::num::{NonZeroU32, NonZeroUsize};
use std::time::{Duration, Instant};

use demux_api::ProgressiveAsyncSeekLimits;
use media_core::{DemuxReadEvent, TrackInfo, TrackKind};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, muxed_ts, open_policy, response,
};
use web_media_core::{
    AudioTrackDescriptor, Bitrate, CandidateFormatIdentity, CandidateIdentity, ChannelCount,
    ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, DynamicRange, ExactSelectionIdentity,
    ExtractionGeneration, FrameRate, NormalizedCodec, RawCodecIdentity, SampleRate,
    SemanticIdentity, SourceIdentity, VideoHeight, VideoTrackDescriptor, VideoWidth,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsCatalogBuildPolicy, HlsCatalogCapabilityProofPort,
    HlsCatalogCapabilityRejection, HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest,
    HlsCatalogPresentation, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsProviderDefaultAudioPolicy, HlsRequestOverrides,
    HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest, discover_hls_catalog,
    prepare_hls_catalog_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);

/// Capability-proof с максимально полным набором метаданных, как у реального ffmpeg-файла.
struct FullMetadataCapabilities;

impl HlsCatalogCapabilityProofPort for FullMetadataCapabilities {
    fn prove_video(
        &mut self,
        track: &TrackInfo,
    ) -> Result<VideoTrackDescriptor, HlsCatalogCapabilityRejection> {
        assert_eq!(track.kind, TrackKind::Video);
        Ok(VideoTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("avc1.64001f").expect("video codec")),
            Some(VideoWidth::new(1280).expect("width")),
            Some(VideoHeight::new(544).expect("height")),
            Some(FrameRate::new(24, 1).expect("frame rate")),
            Some(Bitrate::new(1_500_000).expect("video bitrate")),
            DynamicRange::Sdr,
        ))
    }

    fn prove_audio(
        &mut self,
        track: &TrackInfo,
    ) -> Result<AudioTrackDescriptor, HlsCatalogCapabilityRejection> {
        assert_eq!(track.kind, TrackKind::Audio);
        Ok(AudioTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").expect("audio codec")),
            Some(SampleRate::new(48_000).expect("sample rate")),
            Some(ChannelCount::new(2).expect("channels")),
            Some(Bitrate::new(130_000).expect("audio bitrate")),
            None,
        ))
    }
}

fn catalog_identity() -> ComponentVariantCatalogIdentity {
    let source = SourceIdentity::new(92);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("hls-full-metadata").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "hls-full-metadata").expect("semantic identity");
    ComponentVariantCatalogIdentity::new(
        ExactSelectionIdentity::new(exact, semantic).expect("same source"),
        ComponentVariantCatalogGeneration::new(1),
    )
}

#[test]
fn ffmpeg_muxed_ts_with_full_variant_metadata_opens_and_demuxes() {
    let segment = muxed_ts(90_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            return response(
                "200 OK",
                &[],
                b"#EXTM3U\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=3849487,AVERAGE-BANDWIDTH=1651843,RESOLUTION=1280x544,FRAME-RATE=24.000,CODECS=\"avc1.64001f,mp4a.40.2\"\n\
                  index.m3u8\n",
            );
        }
        if request.request_line.contains("/index.m3u8") {
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
        selection: HlsVariantSelectionIntent {
            resolution: Some((
                NonZeroU32::new(1280).expect("width"),
                NonZeroU32::new(544).expect("height"),
            )),
            codecs: Some("avc1.64001f,mp4a.40.2".into()),
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

    let outcome = discover_hls_catalog(
        HlsCatalogDiscoveryRequest {
            open: &open,
            catalog_identity: catalog_identity(),
            presentation: HlsCatalogPresentation::Vod,
            provider_default_variant_index: Some(0),
            policy: HlsCatalogBuildPolicy {
                catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
                compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
                maximum_unique_children: NonZeroUsize::new(8).expect("child limit"),
                provider_default_audio: HlsProviderDefaultAudioPolicy::AllowUnsupportedOmission,
            },
        },
        &mut FullMetadataCapabilities,
    )
    .expect("full-metadata muxed TS variant must pass the native catalog");
    let HlsCatalogDiscoveryOutcome::Installed(snapshot) = outcome else {
        panic!("master discovery must install a catalog");
    };
    assert_eq!(snapshot.catalog().coupled_presentations().len(), 1);
    assert!(snapshot.sibling_rejections().is_empty());

    let reopen = snapshot
        .reopen_exact(snapshot.provider_default_selection())
        .expect("provider default selection exists and is reopenable");
    let opened = prepare_hls_catalog_vod_receipted(
        open,
        reopen,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("catalog reopen of the default variant");
    let mut demuxer = opened.into_demuxer();

    // Реальный demux: после смены дорожек должны прийти пакеты video и audio.
    let deadline = Instant::now() + TEST_TIMEOUT;
    let (mut video_packets, mut audio_packets) = (0_usize, 0_usize);
    while Instant::now() < deadline && (video_packets == 0 || audio_packets == 0) {
        match demuxer.next_event().expect("demuxer stays readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(2))
            }
            DemuxReadEvent::Packet(packet) => match packet.kind {
                TrackKind::Video => video_packets += 1,
                TrackKind::Audio => audio_packets += 1,
                _ => {}
            },
            _ => {}
        }
    }
    assert!(video_packets > 0, "video packets must demux");
    assert!(audio_packets > 0, "audio packets must demux");
}
