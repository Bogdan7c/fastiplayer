// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

use std::num::NonZeroUsize;

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{
    DashContainer, DashMediaKind, DashMpd, DashMpdLimits, DashMpdParseRequest, parse_dash_mpd,
};
use source_core::HttpRequestTarget;
use web_media_core::{
    AudioTrackDescriptor, Bitrate, CandidateFormatIdentity, CandidateIdentity, ChannelCount,
    ComponentVariantCatalog, ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, DynamicRange, ExactSelectionIdentity,
    ExtractionGeneration, FrameRate, LanguageTag, NormalizedCodec, RawCodecIdentity, SampleRate,
    SemanticIdentity, SourceIdentity, VideoHeight, VideoTrackDescriptor, VideoWidth,
};
use web_media_dash::{
    DashLogicalRepresentationSelection, DashPresentationSelection, DashRepresentationEvidence,
    DashRepresentationLaneCatalogBuildError, DashRepresentationLaneCatalogBuildRequest,
    DashRepresentationLaneProbe, DashRepresentationLaneProbeError, DashRepresentationLaneProof,
    DashRepresentationLaneProofPort, DashRepresentationLaneProviderDefault,
    DashRepresentationLaneRejectionReason, DashRepresentationLaneTimelineMode, DashVideoDimensions,
    build_dash_representation_lane_catalog,
};

fn parse(document: &str) -> DashMpd {
    parse_dash_mpd(DashMpdParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(128 * 1024)
            .maximum_depth(32)
            .maximum_tokens(4_096)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(2_048)
            .maximum_attribute_bytes(64 * 1024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(32)
            .maximum_namespace_bytes(4 * 1024)
            .maximum_text_bytes(64 * 1024)
            .build()
            .expect("test XML budgets"),
        limits: DashMpdLimits {
            maximum_periods: 8,
            maximum_adaptation_sets_per_period: 16,
            maximum_representations_per_adaptation_set: 32,
            maximum_segments_per_list: 64,
            maximum_timeline_entries: 64,
            maximum_schema_string_bytes: 4 * 1024,
        },
    })
    .expect("catalog MPD")
}

fn identities(
    extraction_generation: u64,
    catalog_generation: u64,
) -> (ComponentVariantCatalogIdentity, SemanticIdentity) {
    let source = SourceIdentity::new(71);
    let semantic = SemanticIdentity::new(source, "dash-parent").expect("semantic parent");
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(extraction_generation),
        CandidateFormatIdentity::new("dash-exact").expect("exact format"),
    );
    let parent = ExactSelectionIdentity::new(exact, semantic.clone()).expect("same source");
    (
        ComponentVariantCatalogIdentity::new(
            parent,
            ComponentVariantCatalogGeneration::new(catalog_generation),
        ),
        semantic,
    )
}

fn default_selection() -> DashPresentationSelection {
    DashPresentationSelection::Separate {
        video: DashRepresentationEvidence {
            media_kind: DashMediaKind::Video,
            container: DashContainer::IsoBmff,
            representation_id: None,
            codecs: Some("avc1.4d401f".to_owned()),
            bandwidth: Some(4_000_000),
            dimensions: Some(DashVideoDimensions {
                width: 1_920,
                height: 1_080,
            }),
        },
        audio: DashRepresentationEvidence {
            media_kind: DashMediaKind::Audio,
            container: DashContainer::IsoBmff,
            representation_id: None,
            codecs: Some("mp4a.40.2".to_owned()),
            bandwidth: Some(128_000),
            dimensions: None,
        },
    }
}

fn fixture(period_one_rows: &str, period_two_rows: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT4S">
          <BaseURL>root-a/</BaseURL>
          <Period id="p0" duration="PT2S">
            {period_one_rows}
          </Period>
          <Period id="p1" start="PT2S" duration="PT2S">
            <BaseURL>rotated/</BaseURL>
            {period_two_rows}
          </Period>
        </MPD>"#
    )
}

const VIDEO: &str = r#"<AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f"
    frameRate="30000/1001">
  <SupplementalProperty schemeIdUri="urn:mpeg:mpegB:cicp:TransferCharacteristics" value="1"/>
  <SegmentTemplate timescale="1" duration="2" initialization="v-init.mp4" media="v-$Number$.m4s"/>
  <Representation id="video-a" bandwidth="4000000" width="1920" height="1080"/>
</AdaptationSet>"#;

const AUDIO: &str = r#"<AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2" lang="en"
    audioSamplingRate="48000">
  <AudioChannelConfiguration
      schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="2"/>
  <SegmentTemplate timescale="1" duration="2" initialization="a-init.mp4" media="a-$Number$.m4s"/>
  <Representation id="audio-a" bandwidth="128000"/>
</AdaptationSet>"#;

const MUXED: &str = r#"<AdaptationSet mimeType="application/mp4" contentType="application"
    codecs="avc1.4d401f,mp4a.40.2" frameRate="24" audioSamplingRate="48000">
  <SegmentTemplate timescale="1" duration="2" initialization="m-init.mp4" media="m-$Number$.m4s"/>
  <Representation id="muxed-a" bandwidth="2500000" width="1280" height="720"/>
</AdaptationSet>"#;

fn build(
    presentation: &DashMpd,
    extraction_generation: u64,
    catalog_generation: u64,
    provider_default: &DashPresentationSelection,
) -> Result<web_media_dash::DashRepresentationLaneCatalog, DashRepresentationLaneCatalogBuildError>
{
    let mut proof_port = TestProofPort;
    build_with_proof(
        presentation,
        extraction_generation,
        catalog_generation,
        provider_default,
        &mut proof_port,
    )
}

fn build_with_proof(
    presentation: &DashMpd,
    extraction_generation: u64,
    catalog_generation: u64,
    provider_default: &DashPresentationSelection,
    proof_port: &mut dyn DashRepresentationLaneProofPort,
) -> Result<web_media_dash::DashRepresentationLaneCatalog, DashRepresentationLaneCatalogBuildError>
{
    let (catalog_identity, parent_semantic) = identities(extraction_generation, catalog_generation);
    build_dash_representation_lane_catalog(
        DashRepresentationLaneCatalogBuildRequest {
            presentation,
            manifest_base: &HttpRequestTarget::parse_exact(
                "https://media.invalid/root/manifest.mpd",
            )
            .expect("manifest target"),
            catalog_identity,
            parent_semantic: &parent_semantic,
            provider_default: DashRepresentationLaneProviderDefault::ExactEvidence(
                provider_default,
            ),
            catalog_limit: ComponentVariantCatalogLimit::new(64).expect("catalog limit"),
            compatibility_edge_limit: ComponentVariantEdgeLimit::new(256).expect("edge limit"),
            maximum_planned_segments: NonZeroUsize::new(256).expect("segment limit"),
            timeline_mode: DashRepresentationLaneTimelineMode::Static,
        },
        proof_port,
    )
}

struct RejectMuxedProofPort;

impl DashRepresentationLaneProofPort for RejectMuxedProofPort {
    fn prove_lane(
        &mut self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
        if request.kind() == DashMediaKind::Muxed {
            Err(DashRepresentationLaneProbeError::CapabilityRejected)
        } else {
            TestProofPort.prove_lane(request)
        }
    }
}

struct TestProofPort;

impl DashRepresentationLaneProofPort for TestProofPort {
    fn prove_lane(
        &mut self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
        Ok(match request.kind() {
            DashMediaKind::Video => DashRepresentationLaneProof::VideoOnly(video_track(
                "avc1.4d401f",
                1_920,
                1_080,
                Some((30_000, 1_001)),
                Some(4_000_000),
                DynamicRange::Sdr,
            )),
            DashMediaKind::Audio => DashRepresentationLaneProof::AudioOnly(audio_track(
                Some(128_000),
                Some("en"),
                Some(2),
            )),
            DashMediaKind::Muxed => DashRepresentationLaneProof::Muxed {
                video: video_track(
                    "avc1.4d401f",
                    1_280,
                    720,
                    Some((24, 1)),
                    None,
                    DynamicRange::Unknown,
                ),
                audio: audio_track(None, None, None),
            },
        })
    }
}

fn codec(raw: &str) -> NormalizedCodec {
    NormalizedCodec::parse(RawCodecIdentity::new(raw).expect("test codec"))
}

fn video_track(
    raw_codec: &str,
    width: u32,
    height: u32,
    frame_rate: Option<(u32, u32)>,
    bitrate: Option<u64>,
    dynamic_range: DynamicRange,
) -> VideoTrackDescriptor {
    VideoTrackDescriptor::new(
        codec(raw_codec),
        Some(VideoWidth::new(width).expect("width")),
        Some(VideoHeight::new(height).expect("height")),
        frame_rate.map(|(numerator, denominator)| {
            FrameRate::new(numerator, denominator).expect("frame rate")
        }),
        bitrate.map(|value| Bitrate::new(value).expect("video bitrate")),
        dynamic_range,
    )
}

fn audio_track(
    bitrate: Option<u64>,
    language: Option<&str>,
    channels: Option<u16>,
) -> AudioTrackDescriptor {
    AudioTrackDescriptor::new(
        codec("mp4a.40.2"),
        Some(SampleRate::new(48_000).expect("sample rate")),
        channels.map(|value| ChannelCount::new(value).expect("channels")),
        bitrate.map(|value| Bitrate::new(value).expect("audio bitrate")),
        language.map(|value| LanguageTag::new(value).expect("language")),
    )
}

#[test]
fn multi_period_lanes_publish_sparse_separate_edges_and_muxed_coupled_rows() {
    let rows = format!("{VIDEO}{AUDIO}{MUXED}");
    let presentation = parse(&fixture(&rows, &rows));
    let built = build(&presentation, 1, 1, &default_selection()).expect("lane catalog");

    let ComponentVariantCatalog::Topology {
        video,
        audio,
        compatibility,
        coupled,
        video_only,
        audio_only,
        ..
    } = built.catalog()
    else {
        panic!("DASH catalog must preserve constrained/coupled topology");
    };
    assert_eq!(video.len(), 1);
    assert_eq!(audio.len(), 1);
    assert_eq!(compatibility.logical_edge_count(), 1);
    assert_eq!(coupled.len(), 1);
    assert_eq!(video_only.len(), 1);
    assert_eq!(audio_only.len(), 1);
    assert_eq!(
        video[0]
            .track()
            .frame_rate()
            .expect("exact FPS")
            .numerator(),
        30_000
    );
    assert_eq!(
        video[0]
            .track()
            .frame_rate()
            .expect("exact FPS")
            .denominator(),
        1_001
    );
    assert_eq!(video[0].track().dynamic_range(), DynamicRange::Sdr);
    assert_eq!(
        audio[0].track().sample_rate().expect("sample rate").hertz(),
        48_000
    );
    assert_eq!(audio[0].track().channels().expect("channels").get(), 2);
    assert!(matches!(
        built
            .resolve_selection(built.provider_default())
            .expect("provider default mapping"),
        DashLogicalRepresentationSelection::Separate { video, audio }
            if video.required_period_count() == 2 && audio.required_period_count() == 2
    ));
}

#[test]
fn semantic_selection_survives_xml_order_ids_and_base_url_rotation() {
    let original_rows = format!("{VIDEO}{AUDIO}{MUXED}");
    let fresh_rows = format!("{MUXED}{AUDIO}{VIDEO}")
        .replace("video-a", "video-fresh")
        .replace("audio-a", "audio-fresh")
        .replace("muxed-a", "muxed-fresh")
        .replace("v-$Number$", "video-rotated-$Number$")
        .replace("a-$Number$", "audio-rotated-$Number$")
        .replace("m-$Number$", "muxed-rotated-$Number$");
    let original = parse(&fixture(&original_rows, &original_rows));
    let fresh = parse(&fixture(&fresh_rows, &fresh_rows).replace("root-a/", "root-b/"));
    let old = build(&original, 1, 1, &default_selection()).expect("old catalog");
    let request = old.provider_default().semantic_rematch_request();
    let fresh = build(&fresh, 2, 2, &default_selection()).expect("fresh catalog");
    let rematched = fresh
        .catalog()
        .rematch_semantic(request)
        .expect("semantic rematch");
    assert!(matches!(
        fresh.resolve_selection(&rematched).expect("fresh mapping"),
        DashLogicalRepresentationSelection::Separate { .. }
    ));
}

#[test]
fn capability_rejected_sibling_is_isolated_but_rejected_provider_default_is_fatal() {
    let rows = format!("{VIDEO}{AUDIO}{MUXED}");
    let presentation = parse(&fixture(&rows, &rows));
    let mut proof = RejectMuxedProofPort;
    let built = build_with_proof(&presentation, 1, 1, &default_selection(), &mut proof)
        .expect("non-default muxed sibling is isolatable");
    let ComponentVariantCatalog::Topology { coupled, .. } = built.catalog() else {
        panic!("topology catalog");
    };
    assert!(coupled.is_empty());
    assert!(built.rejections().iter().any(|rejection| {
        rejection.reason() == DashRepresentationLaneRejectionReason::CapabilityRejected
    }));

    let muxed_default = DashPresentationSelection::Single {
        main: DashRepresentationEvidence {
            media_kind: DashMediaKind::Muxed,
            container: DashContainer::IsoBmff,
            representation_id: None,
            codecs: Some("avc1.4d401f,mp4a.40.2".to_owned()),
            bandwidth: Some(2_500_000),
            dimensions: Some(DashVideoDimensions {
                width: 1_280,
                height: 720,
            }),
        },
    };
    let mut proof = RejectMuxedProofPort;
    assert!(matches!(
        build_with_proof(&presentation, 1, 1, &muxed_default, &mut proof),
        Err(
            DashRepresentationLaneCatalogBuildError::ProviderDefaultRejected(
                DashRepresentationLaneProbeError::CapabilityRejected
            )
        )
    ));
}

#[test]
fn missing_ambiguous_and_unsupported_siblings_are_typed_without_hiding_valid_audio() {
    let ambiguous_video = VIDEO.replace(
        r#"<Representation id="video-a" bandwidth="4000000" width="1920" height="1080"/>"#,
        r#"<Representation id="video-a" bandwidth="4000000" width="1920" height="1080"/>
           <Representation id="video-duplicate" bandwidth="4000000" width="1920" height="1080"/>"#,
    );
    let unsupported_audio = AUDIO.replace(
        "urn:mpeg:dash:23003:3:audio_channel_configuration:2011",
        "urn:example:unsupported-audio-layout",
    );
    let first = format!("{VIDEO}{AUDIO}{unsupported_audio}");
    let second = format!("{ambiguous_video}{AUDIO}{unsupported_audio}");
    let presentation = parse(&fixture(&first, &second));
    let audio_default = DashPresentationSelection::Single {
        main: default_selection_audio(),
    };
    let built = build(&presentation, 1, 1, &audio_default).expect("valid audio sibling survives");
    assert!(built.rejections().iter().any(|rejection| {
        rejection.reason() == DashRepresentationLaneRejectionReason::AmbiguousRequiredPeriod
            && rejection.required_period_ordinal() == 1
    }));
    assert!(built.rejections().iter().any(|rejection| {
        rejection.reason() == DashRepresentationLaneRejectionReason::UnsupportedMetadata
    }));

    let first_with_audio = format!("{VIDEO}{AUDIO}");
    let missing_video = parse(&fixture(&first_with_audio, AUDIO));
    assert!(matches!(
        build(&missing_video, 1, 1, &default_selection()),
        Err(DashRepresentationLaneCatalogBuildError::ProviderDefaultMissing)
    ));
}

fn default_selection_audio() -> DashRepresentationEvidence {
    DashRepresentationEvidence {
        media_kind: DashMediaKind::Audio,
        container: DashContainer::IsoBmff,
        representation_id: None,
        codecs: Some("mp4a.40.2".to_owned()),
        bandwidth: Some(128_000),
        dimensions: None,
    }
}

/// MPD в точности как его пишет `ffmpeg -f dash`: `ProgramInformation`, `ServiceDescription`
/// и `bitstreamSwitching` раньше отвергались парсером и уводили источник в yt-dlp.
const FFMPEG_DASH_MPD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
	xmlns="urn:mpeg:dash:schema:mpd:2011"
	xmlns:xlink="http://www.w3.org/1999/xlink"
	xsi:schemaLocation="urn:mpeg:DASH:schema:MPD:2011 http://standards.iso.org/ittf/PubliclyAvailableStandards/MPEG-DASH_schema_files/DASH-MPD.xsd"
	profiles="urn:mpeg:dash:profile:isoff-live:2011"
	type="static"
	mediaPresentationDuration="PT1M30.0S"
	maxSegmentDuration="PT6.0S"
	minBufferTime="PT12.0S">
	<ProgramInformation>
	</ProgramInformation>
	<ServiceDescription id="0">
	</ServiceDescription>
	<Period id="0" start="PT0.0S">
		<AdaptationSet id="0" contentType="video" startWithSAP="1" segmentAlignment="true" bitstreamSwitching="true" frameRate="24/1" maxWidth="1280" maxHeight="544" par="40:17" lang="eng">
			<Representation id="0" mimeType="video/mp4" codecs="avc1.64001f" bandwidth="1450630" width="1280" height="544" sar="1:1">
				<SegmentTemplate timescale="1000000" duration="6000000" initialization="init-$RepresentationID$.m4s" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
		<AdaptationSet id="1" contentType="audio" startWithSAP="1" segmentAlignment="true" bitstreamSwitching="true" lang="eng">
			<Representation id="1" mimeType="audio/mp4" codecs="mp4a.40.2" bandwidth="132651" audioSamplingRate="48000">
				<AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="2" />
				<SegmentTemplate timescale="1000000" duration="6000000" initialization="init-$RepresentationID$.m4s" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
	</Period>
</MPD>
"#;

/// Proof port, который подтверждает ровно те дорожки, что заявлены в ffmpeg MPD.
struct FfmpegProofPort;

impl DashRepresentationLaneProofPort for FfmpegProofPort {
    fn prove_lane(
        &mut self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
        Ok(match request.kind() {
            DashMediaKind::Video => DashRepresentationLaneProof::VideoOnly(video_track(
                "avc1.64001f",
                1_280,
                544,
                Some((24, 1)),
                Some(1_450_630),
                DynamicRange::Sdr,
            )),
            DashMediaKind::Audio => DashRepresentationLaneProof::AudioOnly(audio_track(
                Some(132_651),
                Some("eng"),
                Some(2),
            )),
            DashMediaKind::Muxed => {
                return Err(DashRepresentationLaneProbeError::CapabilityRejected);
            }
        })
    }
}

#[test]
fn ffmpeg_dash_mpd_builds_a_native_separate_video_audio_catalog() {
    // `parse` паникует на любом отказе парсера, то есть именно на прежнем fallback-сценарии.
    let presentation = parse(FFMPEG_DASH_MPD);
    // Provider default описывает ровно те две строки, что объявлены в ffmpeg MPD.
    let provider_default = DashPresentationSelection::Separate {
        video: DashRepresentationEvidence {
            media_kind: DashMediaKind::Video,
            container: DashContainer::IsoBmff,
            representation_id: Some("0".to_owned()),
            codecs: Some("avc1.64001f".to_owned()),
            bandwidth: Some(1_450_630),
            dimensions: Some(DashVideoDimensions {
                width: 1_280,
                height: 544,
            }),
        },
        audio: DashRepresentationEvidence {
            media_kind: DashMediaKind::Audio,
            container: DashContainer::IsoBmff,
            representation_id: Some("1".to_owned()),
            codecs: Some("mp4a.40.2".to_owned()),
            bandwidth: Some(132_651),
            dimensions: None,
        },
    };
    let mut proof_port = FfmpegProofPort;
    let built = build_with_proof(&presentation, 1, 1, &provider_default, &mut proof_port)
        .expect("lane catalog");

    let ComponentVariantCatalog::Topology {
        video,
        audio,
        compatibility,
        ..
    } = built.catalog()
    else {
        panic!("DASH catalog must preserve constrained/coupled topology");
    };
    assert_eq!(video.len(), 1);
    assert_eq!(audio.len(), 1);
    assert_eq!(compatibility.logical_edge_count(), 1);
    assert!(matches!(
        built
            .resolve_selection(built.provider_default())
            .expect("provider default mapping"),
        DashLogicalRepresentationSelection::Separate { .. }
    ));
}

/// Static MPD, который `ffmpeg -f dash` пишет для видео + AAC: audio timeline начинается
/// с priming offset 10832 и заканчивается на 5_771_856 тиков против Period 5_760_000.
const FFMPEG_PRIMING_AUDIO_MPD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
	profiles="urn:mpeg:dash:profile:isoff-live:2011"
	type="static"
	mediaPresentationDuration="PT2M0.0S"
	maxSegmentDuration="PT6.0S"
	minBufferTime="PT12.0S">
	<Period id="0" start="PT0.0S">
		<AdaptationSet id="0" contentType="video" startWithSAP="1" segmentAlignment="true" frameRate="24/1" maxWidth="960" maxHeight="408">
			<Representation id="0" mimeType="video/mp4" codecs="avc1.64001e" bandwidth="585034" width="960" height="408">
				<SegmentTemplate timescale="12288" initialization="init-stream$RepresentationID$.m4s" media="chunk-stream$RepresentationID$-$Number%05d$.m4s" startNumber="1">
					<SegmentTimeline>
						<S t="0" d="73728" r="19" />
					</SegmentTimeline>
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
		<AdaptationSet id="1" contentType="audio" startWithSAP="1" segmentAlignment="true" lang="eng">
			<Representation id="1" mimeType="audio/mp4" codecs="mp4a.40.2" bandwidth="128000" audioSamplingRate="48000">
				<SegmentTemplate timescale="48000" initialization="init-stream$RepresentationID$.m4s" media="chunk-stream$RepresentationID$-$Number%05d$.m4s" startNumber="1">
					<SegmentTimeline>
						<S t="10832" d="273408" />
						<S d="288768" />
						<S d="287744" r="2" />
						<S d="288768" />
						<S d="287744" r="2" />
						<S d="288768" />
						<S d="287744" r="2" />
						<S d="288768" />
						<S d="287744" r="2" />
						<S d="288768" />
						<S d="287744" />
						<S d="288768" />
						<S d="14336" />
					</SegmentTimeline>
				</SegmentTemplate>
			</Representation>
		</AdaptationSet>
	</Period>
</MPD>"#;

/// Proof port, подтверждающий ровно те дорожки, что объявлены в MPD с priming audio.
struct PrimingProofPort;

impl DashRepresentationLaneProofPort for PrimingProofPort {
    fn prove_lane(
        &mut self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
        Ok(match request.kind() {
            DashMediaKind::Video => DashRepresentationLaneProof::VideoOnly(video_track(
                "avc1.64001e",
                960,
                408,
                Some((24, 1)),
                Some(585_034),
                DynamicRange::Sdr,
            )),
            DashMediaKind::Audio => DashRepresentationLaneProof::AudioOnly(audio_track(
                Some(128_000),
                Some("eng"),
                Some(2),
            )),
            DashMediaKind::Muxed => {
                return Err(DashRepresentationLaneProbeError::CapabilityRejected);
            }
        })
    }
}

#[test]
fn ffmpeg_audio_priming_timeline_keeps_audio_lane_and_default_video_and_audio() {
    let presentation = parse(FFMPEG_PRIMING_AUDIO_MPD);
    let provider_default = DashPresentationSelection::Separate {
        video: DashRepresentationEvidence {
            media_kind: DashMediaKind::Video,
            container: DashContainer::IsoBmff,
            representation_id: Some("0".to_owned()),
            codecs: Some("avc1.64001e".to_owned()),
            bandwidth: Some(585_034),
            dimensions: Some(DashVideoDimensions {
                width: 960,
                height: 408,
            }),
        },
        audio: DashRepresentationEvidence {
            media_kind: DashMediaKind::Audio,
            container: DashContainer::IsoBmff,
            representation_id: Some("1".to_owned()),
            codecs: Some("mp4a.40.2".to_owned()),
            bandwidth: Some(128_000),
            dimensions: None,
        },
    };
    let mut proof_port = PrimingProofPort;
    let built = build_with_proof(&presentation, 1, 1, &provider_default, &mut proof_port)
        .expect("audio lane with priming offset must not be rejected as TimelineIncompatible");

    let ComponentVariantCatalog::Topology { video, audio, .. } = built.catalog() else {
        panic!("DASH catalog must be a Topology");
    };
    assert_eq!(video.len(), 1, "video lane stays");
    assert_eq!(audio.len(), 1, "audio lane must survive priming timeline");
    // Default selection обязан быть VideoAndAudio, а не деградировать в VideoOnly.
    assert!(matches!(
        built
            .resolve_selection(built.provider_default())
            .expect("provider default mapping"),
        DashLogicalRepresentationSelection::Separate { .. }
    ));
}
