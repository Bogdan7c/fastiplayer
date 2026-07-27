use audio_core::AudioDecodeCodecFamily;
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId,
    H264Profile, MatrixCoefficients, SupportedVideoDecodeFormat, TransferFunction, VideoCodec,
    VideoColorMetadata, VideoDecodeRequirement, VideoProfile, Vp9Profile,
};
use demux_api::{DemuxInputCapabilities, DemuxInputCapability};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use web_media_core::{
    AudioComponentDescriptor, AudioTrackDescriptor, Bitrate, CandidateDescriptor,
    CandidateFormatIdentity, CandidateIdentity, ChannelCount, ContainerFamily, ContainerIdentity,
    DynamicRange, ExactSelectionIdentity, ExtractionGeneration, FtpScheme,
    HlsMuxedCodecDeferredDescriptor, HttpScheme, MuxedComponentDescriptor, NormalizedCodec,
    NormalizedTransport, PreferredHeightPolicy, RawCodecIdentity, RawContainerIdentity,
    RawTransportIdentity, SampleRate, SelectionRequest, SemanticIdentity, SourceIdentity,
    StreamLayout, TransportFamily, VideoComponentDescriptor, VideoHeight, VideoTrackDescriptor,
    VideoWidth,
};

use crate::{
    CandidateQualityScore, CandidateRuntimeRequirements, DemuxCapabilityRegistration,
    DemuxCapabilitySnapshot, HdrSelectionPolicy, PlanningCandidate, PlanningCandidateSnapshot,
    PlaybackSelectionPolicy, TransportCapabilityRegistration, TransportCapabilitySnapshot,
};

/// Одна source lineage для всего focused test inventory.
pub(super) const TEST_SOURCE: SourceIdentity = SourceIdentity::new(7);

/// Одна immutable extraction generation для обычных scenarios.
pub(super) const TEST_GENERATION: ExtractionGeneration = ExtractionGeneration::new(11);

/// Строит exact+semantic neutral descriptor без request material.
pub(super) fn candidate_descriptor(
    format_id: &str,
    semantic_key: &str,
    layout: StreamLayout,
) -> CandidateDescriptor {
    CandidateDescriptor::new(
        CandidateIdentity::new(
            TEST_SOURCE,
            TEST_GENERATION,
            CandidateFormatIdentity::new(format_id).expect("test format identity валидна"),
        ),
        SemanticIdentity::new(TEST_SOURCE, semantic_key).expect("test semantic identity валидна"),
        layout,
        Vec::new(),
    )
    .expect("test candidate descriptor валиден")
}

/// Строит normalized transport из exact S00 identity.
pub(super) fn transport(raw: &str) -> NormalizedTransport {
    NormalizedTransport::parse(
        RawTransportIdentity::new(raw).expect("test transport identity валидна"),
    )
}

/// Строит непротиворечивую container identity.
pub(super) fn container(raw: &str) -> ContainerIdentity {
    ContainerIdentity::parse(
        None,
        Some(RawContainerIdentity::new(raw).expect("test container identity валидна")),
    )
}

/// Строит video descriptor с явной codec/dynamic-range/height metadata.
pub(super) fn video_track(
    codec: &str,
    height: u32,
    dynamic_range: DynamicRange,
) -> VideoTrackDescriptor {
    VideoTrackDescriptor::new(
        NormalizedCodec::parse(
            RawCodecIdentity::new(codec).expect("test video codec identity валидна"),
        ),
        Some(VideoWidth::new(1920).expect("test width валидна")),
        Some(VideoHeight::new(height).expect("test height валидна")),
        None,
        Some(Bitrate::new(4_000_000).expect("test video bitrate валиден")),
        dynamic_range,
    )
}

/// Строит доказанный Opus audio descriptor.
pub(super) fn audio_track() -> AudioTrackDescriptor {
    audio_track_for("opus")
}

/// Строит audio descriptor для явно названной proven codec family.
pub(super) fn audio_track_for(codec_raw: &str) -> AudioTrackDescriptor {
    AudioTrackDescriptor::new(
        NormalizedCodec::parse(
            RawCodecIdentity::new(codec_raw).expect("test audio codec identity валидна"),
        ),
        Some(SampleRate::new(48_000).expect("test sample rate валиден")),
        Some(ChannelCount::new(2).expect("test channel count валиден")),
        Some(Bitrate::new(160_000).expect("test audio bitrate валиден")),
        None,
    )
}

/// Строит full SDR requirement для existing capability-core.
pub(super) fn sdr_requirement(codec: VideoCodec, height: u32) -> VideoDecodeRequirement {
    let profile = match codec {
        VideoCodec::Av1 => VideoProfile::Av1(Av1Profile::Main),
        VideoCodec::Vp9 => VideoProfile::Vp9(Vp9Profile::Profile0),
        VideoCodec::H264 => VideoProfile::H264(H264Profile::High),
        _ => panic!("focused tests используют только AV1/VP9/H.264"),
    };

    VideoDecodeRequirement::new(codec)
        .with_profile(profile)
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420)
        .with_resolution(1920, height)
        .with_color(VideoColorMetadata::sdr_bt709_limited())
}

/// Строит full HDR VP9 requirement для strict HDR capability path.
pub(super) fn hdr_requirement(height: u32) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420)
        .with_resolution(1920, height)
        .with_color(VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        ))
}

/// Named test input не прячет смысл многочисленных descriptor полей.
pub(super) struct VideoCandidateSpec<'input> {
    /// Exact snapshot-local format identity.
    pub(super) format_id: &'input str,
    /// Refresh-stable semantic key.
    pub(super) semantic_key: &'input str,
    /// Exact raw transport identity.
    pub(super) transport_raw: &'input str,
    /// Exact raw container identity.
    pub(super) container_raw: &'input str,
    /// Exact raw video codec identity.
    pub(super) codec_raw: &'input str,
    /// Checked video height.
    pub(super) height: u32,
    /// Static SDR/HDR hint.
    pub(super) dynamic_range: DynamicRange,
    /// Existing full video decode requirement.
    pub(super) requirement: VideoDecodeRequirement,
    /// Service-owned deterministic quality score.
    pub(super) quality_score: i64,
}

/// Строит video-only planning candidate.
pub(super) fn video_only_candidate(spec: VideoCandidateSpec<'_>) -> PlanningCandidate {
    let component = VideoComponentDescriptor::new(
        transport(spec.transport_raw),
        container(spec.container_raw),
        video_track(spec.codec_raw, spec.height, spec.dynamic_range),
    );
    PlanningCandidate::new(
        candidate_descriptor(
            spec.format_id,
            spec.semantic_key,
            StreamLayout::VideoOnly(component),
        ),
        CandidateRuntimeRequirements::VideoOnly {
            video: spec.requirement,
        },
        CandidateQualityScore::new(spec.quality_score),
    )
    .expect("test video-only candidate проходит admission")
}

/// Строит audio-only planning candidate.
pub(super) fn audio_only_candidate(format_id: &str, semantic_key: &str) -> PlanningCandidate {
    let component =
        AudioComponentDescriptor::new(transport("https"), container("webm"), audio_track());
    PlanningCandidate::new(
        candidate_descriptor(format_id, semantic_key, StreamLayout::AudioOnly(component)),
        CandidateRuntimeRequirements::AudioOnly {
            audio: AudioDecodeCodecFamily::Opus,
        },
        CandidateQualityScore::new(10),
    )
    .expect("test audio-only candidate проходит admission")
}

/// Named параметры не дают S42 matrix перепутать transport, container и codec.
pub(super) struct AudioCandidateSpec<'a> {
    /// Exact S42 row identity.
    pub(super) format_id: &'a str,
    /// Refresh-stable semantic identity.
    pub(super) semantic_key: &'a str,
    /// Raw transport identity из approved profile.
    pub(super) transport_raw: &'a str,
    /// Raw container identity из approved profile.
    pub(super) container_raw: &'a str,
    /// Raw audio codec identity.
    pub(super) codec_raw: &'a str,
    /// Read-only audio decoder capability family.
    pub(super) codec_family: AudioDecodeCodecFamily,
}

/// Строит audio-only candidate для exact S42 transport/container row.
pub(super) fn audio_only_candidate_for(spec: AudioCandidateSpec<'_>) -> PlanningCandidate {
    let component = AudioComponentDescriptor::new(
        transport(spec.transport_raw),
        container(spec.container_raw),
        audio_track_for(spec.codec_raw),
    );
    PlanningCandidate::new(
        candidate_descriptor(
            spec.format_id,
            spec.semantic_key,
            StreamLayout::AudioOnly(component),
        ),
        CandidateRuntimeRequirements::AudioOnly {
            audio: spec.codec_family,
        },
        CandidateQualityScore::new(10),
    )
    .expect("S42 audio-only candidate проходит admission")
}

/// Named параметры удерживают muxed S42 fixture shape самодокументируемым.
pub(super) struct MuxedCandidateSpec<'a> {
    /// Exact S42 row identity.
    pub(super) format_id: &'a str,
    /// Refresh-stable semantic identity.
    pub(super) semantic_key: &'a str,
    /// Raw transport identity из approved profile.
    pub(super) transport_raw: &'a str,
    /// Raw container identity из approved profile.
    pub(super) container_raw: &'a str,
    /// Raw video codec identity.
    pub(super) video_codec_raw: &'a str,
    /// Neutral video decoder capability family.
    pub(super) video_codec: VideoCodec,
    /// Raw audio codec identity.
    pub(super) audio_codec_raw: &'a str,
    /// Read-only audio decoder capability family.
    pub(super) audio_codec_family: AudioDecodeCodecFamily,
}

/// Строит muxed candidate для одного exact S42 provider/container сочетания.
pub(super) fn muxed_candidate_for(spec: MuxedCandidateSpec<'_>) -> PlanningCandidate {
    let component = MuxedComponentDescriptor::new(
        transport(spec.transport_raw),
        container(spec.container_raw),
        video_track(spec.video_codec_raw, 1080, DynamicRange::Sdr),
        audio_track_for(spec.audio_codec_raw),
    );
    PlanningCandidate::new(
        candidate_descriptor(
            spec.format_id,
            spec.semantic_key,
            StreamLayout::Muxed(component),
        ),
        CandidateRuntimeRequirements::Muxed {
            video: sdr_requirement(spec.video_codec, 1080),
            audio: spec.audio_codec_family,
        },
        CandidateQualityScore::new(10),
    )
    .expect("S42 muxed candidate проходит admission")
}

/// Строит muxed planning candidate.
pub(super) fn muxed_candidate(format_id: &str, semantic_key: &str) -> PlanningCandidate {
    let component = MuxedComponentDescriptor::new(
        transport("https"),
        container("webm"),
        video_track("vp09.00.41.08", 1080, DynamicRange::Sdr),
        audio_track(),
    );
    PlanningCandidate::new(
        candidate_descriptor(format_id, semantic_key, StreamLayout::Muxed(component)),
        CandidateRuntimeRequirements::Muxed {
            video: sdr_requirement(VideoCodec::Vp9, 1080),
            audio: AudioDecodeCodecFamily::Opus,
        },
        CandidateQualityScore::new(10),
    )
    .expect("test muxed candidate проходит admission")
}

/// Строит deferred HLS muxed candidate без static codec evidence.
pub(super) fn hls_deferred_candidate(
    format_id: &str,
    semantic_key: &str,
    height: u32,
    quality_score: i64,
) -> PlanningCandidate {
    let component = HlsMuxedCodecDeferredDescriptor::new(
        transport("m3u8_native"),
        container("mp4"),
        VideoHeight::new(height).expect("test deferred height валидна"),
        None,
        None,
        None,
        DynamicRange::Sdr,
    )
    .expect("test deferred HLS descriptor валиден");
    PlanningCandidate::new(
        candidate_descriptor(
            format_id,
            semantic_key,
            StreamLayout::HlsMuxedCodecDeferred(component),
        ),
        CandidateRuntimeRequirements::HlsMuxedCodecDeferred,
        CandidateQualityScore::new(quality_score),
    )
    .expect("test deferred HLS candidate проходит admission")
}

/// Строит separate A/V planning candidate.
pub(super) fn separate_candidate(format_id: &str, semantic_key: &str) -> PlanningCandidate {
    let video = VideoComponentDescriptor::new(
        transport("https"),
        container("webm"),
        video_track("vp09.00.41.08", 1080, DynamicRange::Sdr),
    );
    let audio = AudioComponentDescriptor::new(transport("https"), container("webm"), audio_track());
    PlanningCandidate::new(
        candidate_descriptor(
            format_id,
            semantic_key,
            StreamLayout::Separate { video, audio },
        ),
        CandidateRuntimeRequirements::Separate {
            video: sdr_requirement(VideoCodec::Vp9, 1080),
            audio: AudioDecodeCodecFamily::Opus,
        },
        CandidateQualityScore::new(10),
    )
    .expect("test separate candidate проходит admission")
}

/// Собирает immutable candidate snapshot.
pub(super) fn candidate_snapshot(candidates: Vec<PlanningCandidate>) -> PlanningCandidateSnapshot {
    PlanningCandidateSnapshot::new(TEST_SOURCE, TEST_GENERATION, candidates)
        .expect("test candidate snapshot валиден")
}

/// Строит coherent video capability snapshot для указанных decode formats.
pub(super) fn video_capabilities(formats: Vec<SupportedVideoDecodeFormat>) -> SystemCapabilities {
    let outputs = formats
        .into_iter()
        .map(|decode_format| {
            let frame_contract = if decode_format.bit_depth == BitDepth::Ten {
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers)
            } else {
                VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers)
            };
            SupportedVideoOutput {
                backend: DecodeBackendId::vaapi(),
                decode_format,
                frame_contract,
            }
        })
        .collect::<Vec<_>>();

    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id: DecodeBackendId::vaapi(),
            display_name: "test-video".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: outputs.clone(),
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs: outputs,
    }
}

/// Строит пустой video snapshot для absent-layer tests.
pub(super) fn empty_video_capabilities() -> SystemCapabilities {
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: Vec::new(),
        render_backends: Vec::new(),
        playable_video_outputs: Vec::new(),
    }
}

/// Строит decode format, совпадающий с focused candidate requirement.
pub(super) fn supported_video_format(codec: VideoCodec, hdr: bool) -> SupportedVideoDecodeFormat {
    let profile = match (codec, hdr) {
        (VideoCodec::Av1, false) => VideoProfile::Av1(Av1Profile::Main),
        (VideoCodec::Vp9, false) => VideoProfile::Vp9(Vp9Profile::Profile0),
        (VideoCodec::Vp9, true) => VideoProfile::Vp9(Vp9Profile::Profile2),
        (VideoCodec::H264, false) => VideoProfile::H264(H264Profile::High),
        _ => panic!("focused test format не поддержан helper-ом"),
    };
    SupportedVideoDecodeFormat {
        codec,
        profile,
        bit_depth: if hdr { BitDepth::Ten } else { BitDepth::Eight },
        chroma: ChromaSubsampling::Yuv420,
        max_width: Some(4096),
        max_height: Some(4320),
        max_fps: None,
        hdr_input: hdr,
    }
}

/// Строит transport/demux snapshots для progressive WebM/Matroska paths.
pub(super) fn full_resource_capabilities() -> (TransportCapabilitySnapshot, DemuxCapabilitySnapshot)
{
    let transport_outputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
        .with(DemuxInputCapability::StreamingBytes);
    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            transport_outputs,
        )
        .expect("test transport registration валидна"),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(ContainerFamily::WebM, transport_outputs)
            .expect("WebM demux registration валидна"),
        DemuxCapabilityRegistration::new(ContainerFamily::Matroska, transport_outputs)
            .expect("Matroska demux registration валидна"),
    ]);
    (transport, demux)
}

/// Строит production-shaped positive resource matrix для всех Implemented S42 rows.
pub(super) fn s42_resource_capabilities() -> (TransportCapabilitySnapshot, DemuxCapabilitySnapshot)
{
    let progressive = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
        .with(DemuxInputCapability::StreamingBytes);
    let ordered = DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments);
    let ordered_and_seekable = ordered.with(DemuxInputCapability::SeekableBytes);
    let all_input_shapes = progressive.with(DemuxInputCapability::OrderedSegments);

    let transport = TransportCapabilitySnapshot::new(vec![
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Http),
            progressive,
        )
        .expect("HTTP capability валидна"),
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveHttp(HttpScheme::Https),
            progressive,
        )
        .expect("HTTPS capability валидна"),
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveFtp(FtpScheme::Ftp),
            progressive,
        )
        .expect("FTP capability валидна"),
        TransportCapabilityRegistration::new(
            TransportFamily::ProgressiveFtp(FtpScheme::Ftps),
            progressive,
        )
        .expect("FTPS capability валидна"),
        TransportCapabilityRegistration::new(TransportFamily::Hls, ordered)
            .expect("HLS capability валидна"),
        TransportCapabilityRegistration::new(TransportFamily::Dash, ordered_and_seekable)
            .expect("DASH capability валидна"),
        TransportCapabilityRegistration::new(TransportFamily::SmoothStreaming, ordered)
            .expect("Smooth capability валидна"),
        TransportCapabilityRegistration::new(TransportFamily::Hds, ordered)
            .expect("HDS capability валидна"),
    ]);
    let demux = DemuxCapabilitySnapshot::new(vec![
        DemuxCapabilityRegistration::new(ContainerFamily::IsoBmff, progressive)
            .expect("ISO BMFF demux capability валидна"),
        DemuxCapabilityRegistration::new(ContainerFamily::FragmentedIsoBmff, ordered_and_seekable)
            .expect("fMP4 demux capability валидна"),
        DemuxCapabilityRegistration::new(ContainerFamily::WebM, all_input_shapes)
            .expect("WebM demux capability валидна"),
        DemuxCapabilityRegistration::new(ContainerFamily::Ogg, progressive)
            .expect("Ogg demux capability валидна"),
        DemuxCapabilityRegistration::new(
            ContainerFamily::MpegTs,
            ordered.with(DemuxInputCapability::StreamingBytes),
        )
        .expect("MPEG-TS demux capability валидна"),
        DemuxCapabilityRegistration::new(ContainerFamily::F4f, ordered)
            .expect("F4F demux capability валидна"),
    ]);
    (transport, demux)
}

/// Строит обычную policy с explicit codec/container order.
pub(super) fn selection_policy(
    hdr: HdrSelectionPolicy,
    preferred_height: PreferredHeightPolicy,
    codecs: Vec<VideoCodec>,
    containers: Vec<ContainerFamily>,
) -> PlaybackSelectionPolicy {
    PlaybackSelectionPolicy::new(hdr, codecs, preferred_height, containers)
        .expect("test selection policy валидна")
}

/// Проверяет Exact request для candidate-а из текущего snapshot-а.
pub(super) fn exact_request(candidate: &PlanningCandidate) -> SelectionRequest {
    SelectionRequest::Exact(
        ExactSelectionIdentity::new(
            candidate.descriptor().identity().clone(),
            candidate.descriptor().semantic_identity().clone(),
        )
        .expect("test exact selection identity валидна"),
    )
}
