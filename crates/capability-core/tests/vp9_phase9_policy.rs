use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SystemCapabilities,
    VideoCapabilityRejection, VideoExportPath,
};
use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId, MatrixCoefficients,
    SupportedVideoDecodeFormat, TransferFunction, VideoCodec, VideoColorMetadata,
    VideoDecodeRequirement, VideoProfile, Vp9Profile,
};
use render_core::{P010RenderReadiness, P010StorageLayout, RenderCapabilities, VideoFrameFormat};

#[test]
fn p010_boundary_without_hdr_renderer_does_not_enable_production_hdr() {
    let capabilities = capabilities_with_profile2_p010_boundary();
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420)
        .with_color(bt2020_pq_limited());

    let production_error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("Phase 9 не должен выбирать HDR stream в production path");

    assert!(matches!(
        production_error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedHdrRenderer {
            frame_format: Some(VideoFrameFormat::P010),
        })
    ));

    assert!(
        production_error
            .user_message()
            .contains("HDR-to-SDR renderer")
    );
}

/// Собирает Phase 9 capability report: decode/P010 boundary есть, HDR renderer-а нет.
fn capabilities_with_profile2_p010_boundary() -> SystemCapabilities {
    let mut render_capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
    render_capabilities.p010_render_readiness = P010RenderReadiness::ZeroCopyBoundaryVerified;

    SystemCapabilities {
        schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id: DecodeBackendId::vaapi(),
            display_name: "VA-API".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                codec: VideoCodec::Vp9,
                profile: VideoProfile::Vp9(Vp9Profile::Profile2),
                bit_depth: BitDepth::Ten,
                chroma: ChromaSubsampling::Yuv420,
                max_width: Some(4096),
                max_height: Some(2304),
                max_fps: None,
                hdr_input: true,
                backend: DecodeBackendId::vaapi(),
            }],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            export_paths: vec![VideoExportPath::DmaBuf],
            p010_storage_layouts: vec![P010StorageLayout::BaselineSeparateLayer],
            diagnostics: Vec::new(),
        }],
        render_backends: vec![render_capabilities],
    }
}

/// Создаёт strict core HDR metadata для VP9 Profile 2 P010 candidate-а.
fn bt2020_pq_limited() -> VideoColorMetadata {
    VideoColorMetadata::container(
        ColorRange::Limited,
        MatrixCoefficients::Bt2020,
        ColorPrimaries::Bt2020,
        TransferFunction::Pq,
        None,
    )
}
