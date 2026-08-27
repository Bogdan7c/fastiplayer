use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DecodeBackendId,
    HdrMetadata, MatrixCoefficients, SupportedVideoDecodeFormat, TransferFunction, VideoCodec,
    VideoColorMetadata, VideoDecodeRequirement, VideoProfile, Vp9Profile,
};
use render_core::{P010RenderReadiness, RenderCapabilities};

use crate::{BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SystemCapabilities};

use super::*;

fn capabilities_with_vp9_profile0() -> SystemCapabilities {
    capabilities_with_formats(
        vec![vp9_format(
            Vp9Profile::Profile0,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            false,
        )],
        vec![RenderCapabilities::wgpu_nv12(Some(4096))],
    )
}

fn capabilities_with_formats(
    supported_formats: Vec<SupportedVideoDecodeFormat>,
    render_backends: Vec<RenderCapabilities>,
) -> SystemCapabilities {
    let raw_supported_outputs = supported_formats
        .into_iter()
        .filter_map(output_for_supported_format)
        .collect::<Vec<_>>();
    capabilities_with_outputs(raw_supported_outputs, render_backends)
}

fn capabilities_with_outputs(
    raw_supported_outputs: Vec<SupportedVideoOutput>,
    render_backends: Vec<RenderCapabilities>,
) -> SystemCapabilities {
    let playable_video_outputs = raw_supported_outputs
        .iter()
        .filter(|output| {
            let mut requirement = VideoDecodeRequirement::new(output.decode_format.codec)
                .with_profile(output.decode_format.profile)
                .with_bit_depth(output.decode_format.bit_depth)
                .with_chroma(output.decode_format.chroma);
            requirement.hdr = output.decode_format.hdr_input;
            render_backends
                .iter()
                .any(|renderer| renderer.supports_video_output(&requirement, output.frame_contract))
        })
        .cloned()
        .collect::<Vec<_>>();

    SystemCapabilities {
        schema_version: crate::CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id: DecodeBackendId::vaapi(),
            display_name: "VA-API".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs,
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends,
        playable_video_outputs,
    }
}

fn output_for_supported_format(
    decode_format: SupportedVideoDecodeFormat,
) -> Option<SupportedVideoOutput> {
    let frame_contract = match (decode_format.bit_depth, decode_format.chroma) {
        (BitDepth::Eight, ChromaSubsampling::Yuv420) => {
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers)
        }
        (BitDepth::Ten, ChromaSubsampling::Yuv420) => {
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers)
        }
        _ => return None,
    };

    Some(SupportedVideoOutput {
        backend: DecodeBackendId::vaapi(),
        decode_format,
        frame_contract,
    })
}

fn vp9_format(
    profile: Vp9Profile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    hdr_input: bool,
) -> SupportedVideoDecodeFormat {
    SupportedVideoDecodeFormat {
        codec: VideoCodec::Vp9,
        profile: VideoProfile::Vp9(profile),
        bit_depth,
        chroma,
        max_width: Some(4096),
        max_height: Some(2304),
        max_fps: None,
        hdr_input,
    }
}

fn av1_format(
    profile: Av1Profile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    hdr_input: bool,
) -> SupportedVideoDecodeFormat {
    SupportedVideoDecodeFormat {
        codec: VideoCodec::Av1,
        profile: VideoProfile::Av1(profile),
        bit_depth,
        chroma,
        max_width: Some(4096),
        max_height: Some(2304),
        max_fps: None,
        hdr_input,
    }
}

fn vp9_requirement(
    profile: Vp9Profile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(profile))
        .with_bit_depth(bit_depth)
        .with_chroma(chroma)
}

fn bt2020_pq_limited() -> VideoColorMetadata {
    VideoColorMetadata::container(
        ColorRange::Limited,
        MatrixCoefficients::Bt2020,
        ColorPrimaries::Bt2020,
        TransferFunction::Pq,
        None,
    )
}

fn bt709_limited_with_content_light_metadata() -> VideoColorMetadata {
    VideoColorMetadata::container(
        ColorRange::Limited,
        MatrixCoefficients::Bt709,
        ColorPrimaries::Bt709,
        TransferFunction::Bt709,
        Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt709,
            transfer_function: TransferFunction::Bt709,
            max_luminance_nits: None,
            min_luminance_nits: None,
            max_content_light_level_nits: Some(1_100),
            max_frame_average_light_level_nits: Some(180),
        }),
    )
}

#[test]
fn exact_backend_lookup_returns_matching_playable_output() {
    let capabilities = capabilities_with_vp9_profile0();
    let requirement = vp9_requirement(
        Vp9Profile::Profile0,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
    );
    let vaapi_backend_id = DecodeBackendId::vaapi();

    let selected_output = capabilities
        .find_playable_video_output_for_backend(&vaapi_backend_id, &requirement)
        .expect("exact playable VA-API output должен быть найден");

    assert_eq!(selected_output.backend, vaapi_backend_id);
    assert!(selected_output.satisfies(&requirement));
}

#[test]
fn exact_backend_lookup_rejects_output_owned_by_another_backend() {
    let capabilities = capabilities_with_vp9_profile0();
    let requirement = vp9_requirement(
        Vp9Profile::Profile0,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
    );
    let software_backend_id =
        DecodeBackendId::new("ffmpeg").expect("canonical software backend id должен быть валиден");

    assert!(
        capabilities
            .find_playable_video_output_for_backend(&software_backend_id, &requirement)
            .is_none()
    );
}

#[test]
fn exact_backend_lookup_rejects_requirement_mismatch() {
    let capabilities = capabilities_with_vp9_profile0();
    let unsupported_requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
    );
    let vaapi_backend_id = DecodeBackendId::vaapi();

    assert!(
        capabilities
            .find_playable_video_output_for_backend(&vaapi_backend_id, &unsupported_requirement,)
            .is_none()
    );
}

#[test]
fn selection_picks_supported_highest_quality_candidate() {
    let capabilities = capabilities_with_vp9_profile0();
    let candidates = vec![
        VideoStreamCandidate {
            stream_id: "low".to_string(),
            requirement: VideoDecodeRequirement::new(VideoCodec::Vp9)
                .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0)),
            quality_score: 10,
        },
        VideoStreamCandidate {
            stream_id: "high".to_string(),
            requirement: VideoDecodeRequirement::new(VideoCodec::Vp9)
                .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0)),
            quality_score: 20,
        },
    ];

    let selected = capabilities
        .select_best_video_stream(&candidates)
        .expect("supported stream should be selected");

    assert_eq!(selected.stream_id, "high");
}

#[test]
fn vp9_profile0_bt709_with_content_light_metadata_stays_on_sdr_nv12_path() {
    let capabilities = capabilities_with_vp9_profile0();
    let requirement = vp9_requirement(
        Vp9Profile::Profile0,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
    )
    .with_resolution(3840, 2160)
    .with_color(bt709_limited_with_content_light_metadata());
    let candidates = vec![VideoStreamCandidate {
        stream_id: "sdr-vp9-profile0".to_string(),
        requirement: requirement.clone(),
        quality_score: 10,
    }];

    let selected = capabilities
        .select_best_video_stream(&candidates)
        .expect("BT.709 SDR with content-light side metadata must stay playable");

    assert!(!requirement.hdr);
    assert_eq!(selected.stream_id, "sdr-vp9-profile0");
}

#[test]
fn unsupported_profile_is_reported_before_decode() {
    let capabilities = capabilities_with_vp9_profile0();
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("profile2 should be unsupported");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedProfile { .. })
    ));
    let message = error.user_message();
    assert!(message.contains("profile VP9 Profile 2"));
    assert!(message.contains("доступными video decode backend-ами"));
}

#[test]
fn profile1_and_profile3_are_rejected_as_unsupported_chroma() {
    let cases = [
        (
            Vp9Profile::Profile1,
            BitDepth::Eight,
            ChromaSubsampling::Yuv422,
        ),
        (
            Vp9Profile::Profile3,
            BitDepth::Ten,
            ChromaSubsampling::Yuv444,
        ),
    ];

    for (profile, bit_depth, chroma) in cases {
        let capabilities = capabilities_with_formats(
            vec![vp9_format(profile, bit_depth, chroma, false)],
            vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        );
        let requirement = vp9_requirement(profile, bit_depth, chroma);

        let error = capabilities
            .check_video_requirement(&requirement)
            .expect_err("VP9 non-4:2:0 profiles must be rejected by chroma policy");

        assert!(matches!(
            error.rejections.first(),
            Some(VideoCapabilityRejection::UnsupportedChroma {
                codec: VideoCodec::Vp9,
                chroma: rejected_chroma,
            }) if *rejected_chroma == chroma
        ));
    }
}

#[test]
fn twelve_bit_requirement_is_rejected_as_unsupported_bit_depth() {
    let capabilities = capabilities_with_formats(
        vec![vp9_format(
            Vp9Profile::Profile2,
            BitDepth::Twelve,
            ChromaSubsampling::Yuv420,
            true,
        )],
        vec![RenderCapabilities::wgpu_nv12(Some(4096))],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Twelve,
        ChromaSubsampling::Yuv420,
    );

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("12-bit stream must be rejected before render selection");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedBitDepth {
            codec: VideoCodec::Vp9,
            bit_depth: BitDepth::Twelve,
        })
    ));
}

#[test]
fn vp9_profile2_10bit_hdr_is_rejected_until_hdr_renderer_exists() {
    let mut p010_without_hdr_renderer =
        RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
            Some(4096),
            vec![DmaBufImageLayout::SeparateLayers],
        );
    p010_without_hdr_renderer.supports_hdr_to_sdr = false;
    p010_without_hdr_renderer
        .supported_hdr_to_sdr_operators
        .clear();

    let capabilities = capabilities_with_formats(
        vec![vp9_format(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            true,
        )],
        vec![p010_without_hdr_renderer],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
    )
    .with_color(bt2020_pq_limited());

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("HDR stream must wait for Phase 10 HDR renderer");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedHdrRenderer {
            frame_format: Some(VideoFramePixelLayout::P010),
        })
    ));
    assert!(error.user_message().contains("HDR-to-SDR renderer"));
}

#[test]
fn yuv420_requirement_accepts_backend_software_host_upload_contract() {
    let capabilities = capabilities_with_outputs(
        vec![SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format: vp9_format(
                Vp9Profile::Profile0,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
                false,
            ),
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        }],
        vec![RenderCapabilities::wgpu_nv12(Some(4096))],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile0,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
    );

    let selected_output = capabilities
        .check_video_requirement(&requirement)
        .expect("YUV420 software host-upload output is renderable");

    assert_eq!(
        selected_output.frame_contract,
        VideoFrameContract::host_yuv420_planar8()
    );
}

#[test]
fn p010_boundary_verified_state_alone_does_not_make_stream_playable() {
    let mut render_capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
    render_capabilities.p010_render_readiness = P010RenderReadiness::ZeroCopyBoundaryVerified;
    render_capabilities
        .supported_frame_contracts
        .push(VideoFrameContract::dma_buf_p010(
            DmaBufImageLayout::SeparateLayers,
        ));
    let capabilities = capabilities_with_formats(
        vec![vp9_format(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            false,
        )],
        vec![render_capabilities],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
    );

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("P010 boundary diagnostics must not enable production playback");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::P010NotRenderable {
            readiness: P010RenderReadiness::ZeroCopyBoundaryVerified,
        })
    ));
}

#[test]
fn p010_renderable_bt2446c_renderer_makes_hdr_to_sdr_stream_playable() {
    let capabilities = capabilities_with_formats(
        vec![vp9_format(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            true,
        )],
        vec![RenderCapabilities::wgpu_p010_bt2446c(Some(4096))],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
    )
    .with_color(bt2020_pq_limited());

    let selected_output = capabilities
        .check_video_requirement(&requirement)
        .expect("P010 renderable + BT.2446-C must enable HDR-to-SDR playback");

    assert_eq!(selected_output.decode_format.bit_depth, BitDepth::Ten);
    assert_eq!(
        selected_output.decode_format.chroma,
        ChromaSubsampling::Yuv420
    );
    assert!(selected_output.decode_format.hdr_input);
}

#[test]
fn hdr_stream_is_selected_only_when_decode_p010_layout_and_hdr_to_sdr_pass() {
    let capabilities = capabilities_with_formats(
        vec![
            vp9_format(
                Vp9Profile::Profile0,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
                false,
            ),
            vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            ),
        ],
        vec![
            RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                Some(4096),
                vec![DmaBufImageLayout::SeparateLayers],
            ),
        ],
    );
    let candidates = vec![
        VideoStreamCandidate {
            stream_id: "sdr".to_string(),
            requirement: vp9_requirement(
                Vp9Profile::Profile0,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
            ),
            quality_score: 10,
        },
        VideoStreamCandidate {
            stream_id: "hdr".to_string(),
            requirement: vp9_requirement(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
            )
            .with_color(bt2020_pq_limited()),
            quality_score: 100,
        },
    ];

    let selected = capabilities
        .select_best_video_stream(&candidates)
        .expect("HDR stream should be selected when full Phase 10 intersection passes");

    assert_eq!(selected.stream_id, "hdr");
}

#[test]
fn hdr_stream_is_skipped_when_p010_layout_feature_is_missing() {
    let capabilities = capabilities_with_formats(
        vec![
            vp9_format(
                Vp9Profile::Profile0,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
                false,
            ),
            vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            ),
        ],
        vec![
            RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                Some(4096),
                vec![DmaBufImageLayout::ComposedLayers],
            ),
        ],
    );
    let candidates = vec![
        VideoStreamCandidate {
            stream_id: "sdr".to_string(),
            requirement: vp9_requirement(
                Vp9Profile::Profile0,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
            ),
            quality_score: 10,
        },
        VideoStreamCandidate {
            stream_id: "hdr".to_string(),
            requirement: vp9_requirement(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
            )
            .with_color(bt2020_pq_limited()),
            quality_score: 100,
        },
    ];

    let selected = capabilities
        .select_best_video_stream(&candidates)
        .expect("SDR fallback candidate should be selected instead of unsupported HDR layout");

    assert_eq!(selected.stream_id, "sdr");
}

#[test]
fn missing_separate_layer_p010_import_feature_rejects_hdr_stream() {
    let capabilities = capabilities_with_formats(
        vec![vp9_format(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            true,
        )],
        vec![
            RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                Some(4096),
                vec![DmaBufImageLayout::ComposedLayers],
            ),
        ],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
    )
    .with_color(bt2020_pq_limited());

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("baseline separate-layer P010 must require TEXTURE_FORMAT_16BIT_NORM");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout {
            storage_layout: DmaBufImageLayout::SeparateLayers,
            required_wgpu_feature,
            ..
        }) if required_wgpu_feature == "TEXTURE_FORMAT_16BIT_NORM"
    ));
}

#[test]
fn missing_composed_p010_import_feature_rejects_hdr_stream() {
    let capabilities = capabilities_with_outputs(
        vec![SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format: vp9_format(
                Vp9Profile::Profile2,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
                true,
            ),
            frame_contract: VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
        }],
        vec![
            RenderCapabilities::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
                Some(4096),
                vec![DmaBufImageLayout::SeparateLayers],
            ),
        ],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
    )
    .with_color(bt2020_pq_limited());

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("compatibility composed P010 must require TEXTURE_FORMAT_P010");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout {
            storage_layout: DmaBufImageLayout::ComposedLayers,
            required_wgpu_feature,
            ..
        }) if required_wgpu_feature == "TEXTURE_FORMAT_P010"
    ));
}

#[test]
fn known_multi_object_contract_is_rejected_before_decode_start() {
    let capabilities = capabilities_with_outputs(
        vec![SupportedVideoOutput {
            backend: DecodeBackendId::vaapi(),
            decode_format: vp9_format(
                Vp9Profile::Profile0,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
                false,
            ),
            frame_contract: VideoFrameContract::dma_buf_nv12(
                DmaBufImageLayout::ComposedMultiObject,
            ),
        }],
        vec![RenderCapabilities::wgpu_nv12(Some(4096))],
    );
    let requirement = vp9_requirement(
        Vp9Profile::Profile0,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
    );

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("known multi-object output must not enter the playable capability set");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout {
            storage_layout: DmaBufImageLayout::ComposedMultiObject,
            required_wgpu_feature,
            ..
        }) if required_wgpu_feature == "MULTI_OBJECT_DMA_BUF_IMPORT"
    ));
}

#[test]
fn missing_strict_hdr_metadata_rejects_stream_before_render() {
    let capabilities = capabilities_with_formats(
        vec![vp9_format(
            Vp9Profile::Profile2,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            true,
        )],
        vec![RenderCapabilities::wgpu_p010_bt2446c(Some(4096))],
    );
    let mut requirement = vp9_requirement(
        Vp9Profile::Profile2,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
    );
    requirement.hdr = true;

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("HDR stream without resolved strict metadata must be rejected");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::InvalidHdrMetadata { reason })
            if reason.contains("отсутствует resolved color metadata")
    ));
}

#[test]
fn unsupported_av1_profile_is_reported_before_decode_start() {
    let capabilities = capabilities_with_formats(
        vec![av1_format(
            Av1Profile::Main,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            false,
        )],
        vec![RenderCapabilities::wgpu_nv12(Some(4096))],
    );
    let requirement = VideoDecodeRequirement::new(VideoCodec::Av1)
        .with_profile(VideoProfile::Av1(Av1Profile::High))
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420);

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err("AV1 High must be rejected by AV1 Main-only capabilities");

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedProfile {
            codec: VideoCodec::Av1,
            profile: VideoProfile::Av1(Av1Profile::High),
        })
    ));
}

#[test]
fn codec_neutral_p010_surface_without_renderer_support_is_rejected_before_decode_start() {
    let capabilities = capabilities_with_formats(
        vec![av1_format(
            Av1Profile::Main,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            true,
        )],
        vec![RenderCapabilities::wgpu_nv12(Some(4096))],
    );
    let requirement = VideoDecodeRequirement::new(VideoCodec::Av1)
        .with_profile(VideoProfile::Av1(Av1Profile::Main))
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420);

    let error = capabilities
        .check_video_requirement(&requirement)
        .expect_err(
            "P010 surface must be rejected before hardware decode if renderer cannot import it",
        );

    assert!(matches!(
        error.rejections.first(),
        Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat {
            frame_format: VideoFramePixelLayout::P010,
        })
    ));
}

#[test]
fn reason_formatter_produces_user_facing_russian_explanation() {
    let message = VideoCapabilityRejection::UnsupportedBitDepth {
        codec: VideoCodec::Vp9,
        bit_depth: BitDepth::Twelve,
    }
    .user_message();

    assert!(message.contains("VP9 12-bit"));
    assert!(message.contains("не поддерживается"));
}
