use codec_core::{
    BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement,
    video_frame_pixel_layout_from_decode_requirement,
};
use video_frame_contract::{
    DmaBufImageLayout, FrameChromaSubsampling, VideoFrameContract, VideoFramePixelLayout,
    VideoFrameTransferPath,
};

use super::software_host_upload_contract;
use super::*;
use crate::*;
impl RenderCapabilities {
    /// Создаёт fake renderer из exact contracts без WGPU/materializer promises.
    fn fake_with_frame_contracts_for_tests(
        display_name: &str,
        supported_frame_contracts: Vec<VideoFrameContract>,
        max_texture_size: Option<u32>,
    ) -> Self {
        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: display_name.to_string(),
            supported_frame_contracts,
            p010_render_readiness: P010RenderReadiness::Unavailable,
            supported_hdr_to_sdr_operators: Vec::new(),
            hdr_output_mode: HdrOutputMode::SdrBt709Only,
            supports_hdr_to_sdr: false,
            supports_native_hdr_output: false,
            max_texture_size,
            advanced_ui: false,
            ui_composition_mode: UiCompositionMode::Overlay,
            present_timing_metrics: false,
        }
    }

    /// Создаёт fake renderer, который объявляет только exact host-upload contracts.
    fn fake_host_upload_for_tests(
        supported_pixel_layouts: &[VideoFramePixelLayout],
        max_texture_size: Option<u32>,
    ) -> Self {
        let supported_frame_contracts = supported_pixel_layouts
            .iter()
            .copied()
            .map(host_upload_contract_for_tests)
            .collect();

        Self::fake_with_frame_contracts_for_tests(
            "Fake host-upload renderer",
            supported_frame_contracts,
            max_texture_size,
        )
    }
}

/// Создаёт host-upload contract для explicit planar layout-а в тестах.
fn host_upload_contract_for_tests(pixel_layout: VideoFramePixelLayout) -> VideoFrameContract {
    VideoFrameContract {
        pixel_layout,
        transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
    }
}

/// Собирает stream requirement с теми metadata, которые должен покрыть contract.
fn video_requirement_for_tests(
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_bit_depth(bit_depth)
        .with_chroma(chroma)
}

#[test]
fn eight_bit_yuv420_requirement_maps_to_nv12() {
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420);

    assert_eq!(
        video_frame_pixel_layout_from_decode_requirement(&requirement),
        Some(VideoFramePixelLayout::Nv12)
    );
}

#[test]
fn ten_bit_requirement_maps_to_p010() {
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

    assert_eq!(
        video_frame_pixel_layout_from_decode_requirement(&requirement),
        Some(VideoFramePixelLayout::P010)
    );
}

#[test]
fn neutral_render_settings_crates_do_not_depend_on_wgpu_specific_crates() {
    let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("render-core crate has crates parent");
    let neutral_manifests = ["render-core/Cargo.toml", "settings-core/Cargo.toml"];

    for manifest in neutral_manifests {
        let manifest_path = crates_dir.join(manifest);
        let manifest_text =
            std::fs::read_to_string(&manifest_path).expect("neutral manifest is readable");

        for dependency_name in ["wgpu", "wgpu-types", "egui", "egui-wgpu", "render-wgpu"] {
            let has_disallowed_dependency = manifest_text.lines().any(|line| {
                let trimmed_line = line.trim_start();

                trimmed_line.starts_with(&format!("{dependency_name}."))
                    || trimmed_line.starts_with(&format!("{dependency_name} "))
                    || trimmed_line.starts_with(&format!("{dependency_name}="))
            });

            assert!(
                !has_disallowed_dependency,
                "{manifest} must stay renderer/UI neutral and not depend on {dependency_name}"
            );
        }
    }
}

#[test]
fn current_wgpu_nv12_capabilities_advertise_host_yuv_matrix_without_p010_or_hdr() {
    let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));

    assert!(capabilities.supports_frame_format(VideoFramePixelLayout::Nv12));
    assert!(
        capabilities.supports_frame_contract(VideoFrameContract::dma_buf_nv12(
            DmaBufImageLayout::ComposedLayers
        ))
    );
    assert!(
        capabilities.supports_frame_contract(VideoFrameContract::dma_buf_nv12(
            DmaBufImageLayout::SeparateLayers
        ))
    );
    assert!(capabilities.supports_frame_contract(VideoFrameContract::host_yuv420_planar8()));
    assert!(capabilities.supports_frame_contract(VideoFrameContract::host_yuv420_planar10le()));
    assert!(capabilities.supports_frame_contract(VideoFrameContract::host_yuv420_planar12le()));
    assert!(
        capabilities.supports_frame_contract(software_host_upload_contract(
            VideoFramePixelLayout::Yuv422Planar8
        ))
    );
    assert!(
        capabilities.supports_frame_contract(software_host_upload_contract(
            VideoFramePixelLayout::Yuv422Planar10Le
        ))
    );
    assert!(
        capabilities.supports_frame_contract(software_host_upload_contract(
            VideoFramePixelLayout::Yuv422Planar12Le
        ))
    );
    assert!(
        capabilities.supports_frame_contract(software_host_upload_contract(
            VideoFramePixelLayout::Yuv444Planar8
        ))
    );
    assert!(
        capabilities.supports_frame_contract(software_host_upload_contract(
            VideoFramePixelLayout::Yuv444Planar10Le
        ))
    );
    assert!(!capabilities.supports_frame_format(VideoFramePixelLayout::P010));
    assert_eq!(
        capabilities.p010_render_readiness,
        P010RenderReadiness::Unavailable
    );
    assert!(capabilities.supported_hdr_to_sdr_operators.is_empty());
    assert_eq!(capabilities.hdr_output_mode, HdrOutputMode::SdrBt709Only);
    assert!(!capabilities.supports_p010_rendering());
    assert!(!capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
    assert!(!capabilities.supports_hdr_to_sdr);
    assert!(!capabilities.supports_native_hdr_output);
    assert!(capabilities.summary_text().contains("SDR only"));
    assert!(
        capabilities
            .summary_text()
            .contains("native HDR unsupported")
    );
    assert!(capabilities.summary_text().contains("P010 unavailable"));
    assert!(
        capabilities
            .summary_text()
            .contains("NV12 via hardware zero-copy via DMA-BUF (composed DMA-BUF layers)")
    );
    assert!(capabilities.summary_text().contains("software host upload"));
    assert!(!capabilities.summary_text().contains("HDR supported"));
}

#[test]
fn current_wgpu_capabilities_advertise_dma_buf_and_exact_v1_host_upload_matrix() {
    let nv12_capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
    let p010_capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));

    assert_eq!(
        nv12_capabilities.supported_frame_contracts,
        vec![
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            VideoFrameContract::host_yuv420_planar8(),
            VideoFrameContract::host_yuv420_planar10le(),
            VideoFrameContract::host_yuv420_planar12le(),
            software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar8),
            software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar10Le),
            software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar12Le),
            software_host_upload_contract(VideoFramePixelLayout::Yuv444Planar8),
            software_host_upload_contract(VideoFramePixelLayout::Yuv444Planar10Le),
        ]
    );
    assert_eq!(
        p010_capabilities.supported_frame_contracts,
        vec![
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
            VideoFrameContract::host_yuv420_planar8(),
            VideoFrameContract::host_yuv420_planar10le(),
            VideoFrameContract::host_yuv420_planar12le(),
            software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar8),
            software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar10Le),
            software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar12Le),
            software_host_upload_contract(VideoFramePixelLayout::Yuv444Planar8),
            software_host_upload_contract(VideoFramePixelLayout::Yuv444Planar10Le),
        ]
    );

    for capabilities in [&nv12_capabilities, &p010_capabilities] {
        assert!(
            capabilities
                .supported_frame_contracts
                .iter()
                .all(|contract| {
                    matches!(
                        contract.pixel_layout,
                        VideoFramePixelLayout::Nv12
                            | VideoFramePixelLayout::P010
                            | VideoFramePixelLayout::Yuv420Planar8
                            | VideoFramePixelLayout::Yuv420Planar10Le
                            | VideoFramePixelLayout::Yuv420Planar12Le
                            | VideoFramePixelLayout::Yuv422Planar8
                            | VideoFramePixelLayout::Yuv422Planar10Le
                            | VideoFramePixelLayout::Yuv422Planar12Le
                            | VideoFramePixelLayout::Yuv444Planar8
                            | VideoFramePixelLayout::Yuv444Planar10Le
                    )
                })
        );
        assert!(
            capabilities
                .supported_frame_contracts
                .iter()
                .filter(|contract| contract.transfer_path.is_software_host_upload())
                .all(|contract| matches!(
                    contract.pixel_layout.chroma(),
                    Some(
                        FrameChromaSubsampling::Yuv420
                            | FrameChromaSubsampling::Yuv422
                            | FrameChromaSubsampling::Yuv444
                    )
                ))
        );
    }
}

#[test]
fn fake_capabilities_can_advertise_host_upload_without_cartesian_product() {
    let capabilities = RenderCapabilities::fake_with_frame_contracts_for_tests(
        "Fake mixed renderer",
        vec![
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            host_upload_contract_for_tests(VideoFramePixelLayout::Yuv422Planar10Le),
        ],
        Some(4096),
    );

    let unsupported_host_layout =
        host_upload_contract_for_tests(VideoFramePixelLayout::Yuv420Planar10Le);
    let unsupported_dma_buf_layout =
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers);

    let invalid_cartesian_contract = VideoFrameContract {
        pixel_layout: VideoFramePixelLayout::Nv12,
        transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
    };

    assert!(
        capabilities.supports_frame_contract(host_upload_contract_for_tests(
            VideoFramePixelLayout::Yuv422Planar10Le
        ))
    );
    assert!(matches!(
        capabilities.check_frame_contract(unsupported_host_layout),
        Err(RenderFrameContractRejection::UnsupportedPixelLayout {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le,
        })
    ));
    assert!(matches!(
        capabilities.check_frame_contract(unsupported_dma_buf_layout),
        Err(RenderFrameContractRejection::UnsupportedDmaBufImageLayout {
            pixel_layout: VideoFramePixelLayout::Nv12,
            image_layout: DmaBufImageLayout::ComposedLayers,
        })
    ));
    assert!(matches!(
        capabilities.check_frame_contract(invalid_cartesian_contract),
        Err(RenderFrameContractRejection::InvalidContract { .. })
    ));
    assert!(capabilities.summary_text().contains("software host upload"));
}

#[test]
fn fake_capabilities_support_host_upload_exact_video_outputs() {
    let supported_layouts = [
        (
            VideoFramePixelLayout::Yuv420Planar8,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
        ),
        (
            VideoFramePixelLayout::Yuv420Planar10Le,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
        ),
        (
            VideoFramePixelLayout::Yuv420Planar12Le,
            BitDepth::Twelve,
            ChromaSubsampling::Yuv420,
        ),
        (
            VideoFramePixelLayout::Yuv422Planar8,
            BitDepth::Eight,
            ChromaSubsampling::Yuv422,
        ),
        (
            VideoFramePixelLayout::Yuv422Planar10Le,
            BitDepth::Ten,
            ChromaSubsampling::Yuv422,
        ),
        (
            VideoFramePixelLayout::Yuv422Planar12Le,
            BitDepth::Twelve,
            ChromaSubsampling::Yuv422,
        ),
        (
            VideoFramePixelLayout::Yuv444Planar8,
            BitDepth::Eight,
            ChromaSubsampling::Yuv444,
        ),
        (
            VideoFramePixelLayout::Yuv444Planar10Le,
            BitDepth::Ten,
            ChromaSubsampling::Yuv444,
        ),
    ];
    let supported_pixel_layouts = supported_layouts
        .iter()
        .map(|(pixel_layout, _, _)| *pixel_layout)
        .collect::<Vec<_>>();
    let capabilities =
        RenderCapabilities::fake_host_upload_for_tests(&supported_pixel_layouts, Some(4096));

    for (pixel_layout, bit_depth, chroma) in supported_layouts {
        let requirement = video_requirement_for_tests(bit_depth, chroma);
        let frame_contract = host_upload_contract_for_tests(pixel_layout);

        assert!(capabilities.supports_video_output(&requirement, frame_contract));
    }

    let wrong_chroma_requirement =
        video_requirement_for_tests(BitDepth::Ten, ChromaSubsampling::Yuv420);
    let yuv422_contract = host_upload_contract_for_tests(VideoFramePixelLayout::Yuv422Planar10Le);

    assert!(matches!(
        capabilities.check_video_output(&wrong_chroma_requirement, yuv422_contract),
        Err(RenderVideoOutputRejection::FrameContract {
            reason: RenderFrameContractRejection::UnsupportedContractCombination {
                frame_contract
            },
        }) if frame_contract == yuv422_contract
    ));
}

#[test]
fn video_output_rejections_keep_contract_policy_and_size_distinct() {
    let yuv420_requirement =
        video_requirement_for_tests(BitDepth::Eight, ChromaSubsampling::Yuv420);
    let yuv422_requirement =
        video_requirement_for_tests(BitDepth::Eight, ChromaSubsampling::Yuv422);
    let yuv420_host_contract = host_upload_contract_for_tests(VideoFramePixelLayout::Yuv420Planar8);
    let yuv422_host_contract = host_upload_contract_for_tests(VideoFramePixelLayout::Yuv422Planar8);

    let host_upload_yuv420_capabilities = RenderCapabilities::fake_host_upload_for_tests(
        &[VideoFramePixelLayout::Yuv420Planar8],
        Some(4096),
    );
    assert!(matches!(
        host_upload_yuv420_capabilities
            .check_video_output(&yuv422_requirement, yuv422_host_contract),
        Err(RenderVideoOutputRejection::FrameContract {
            reason: RenderFrameContractRejection::UnsupportedPixelLayout {
                pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
            },
        })
    ));

    let dma_buf_only_capabilities = RenderCapabilities::fake_with_frame_contracts_for_tests(
        "Fake DMA-BUF-only renderer",
        vec![VideoFrameContract::dma_buf_nv12(
            DmaBufImageLayout::ComposedLayers,
        )],
        Some(4096),
    );
    assert!(matches!(
        dma_buf_only_capabilities.check_video_output(&yuv420_requirement, yuv420_host_contract),
        Err(RenderVideoOutputRejection::FrameContract {
            reason: RenderFrameContractRejection::UnsupportedTransferPath {
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            },
        })
    ));

    let mut p010_boundary_only_capabilities =
        RenderCapabilities::fake_with_frame_contracts_for_tests(
            "Fake P010 boundary-only renderer",
            vec![VideoFrameContract::dma_buf_p010(
                DmaBufImageLayout::SeparateLayers,
            )],
            Some(4096),
        );
    p010_boundary_only_capabilities.p010_render_readiness =
        P010RenderReadiness::ZeroCopyBoundaryVerified;
    let p010_requirement = video_requirement_for_tests(BitDepth::Ten, ChromaSubsampling::Yuv420);
    assert!(matches!(
        p010_boundary_only_capabilities.check_video_output(
            &p010_requirement,
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        ),
        Err(RenderVideoOutputRejection::P010NotRenderable {
            readiness: P010RenderReadiness::ZeroCopyBoundaryVerified,
        })
    ));

    let mut hdr_requirement = yuv420_requirement.clone();
    hdr_requirement.hdr = true;
    assert!(matches!(
        host_upload_yuv420_capabilities
            .check_video_output(&hdr_requirement, yuv420_host_contract),
        Err(RenderVideoOutputRejection::HdrUnsupported {
            frame_contract
        }) if frame_contract == yuv420_host_contract
    ));

    let small_texture_capabilities = RenderCapabilities::fake_host_upload_for_tests(
        &[VideoFramePixelLayout::Yuv420Planar8],
        Some(32),
    );
    let oversized_requirement = yuv420_requirement.with_resolution(64, 16);
    assert!(matches!(
        small_texture_capabilities.check_video_output(&oversized_requirement, yuv420_host_contract),
        Err(RenderVideoOutputRejection::MaxTextureSizeExceeded {
            dimension: RenderTextureDimension::Width,
            requested: 64,
            max_texture_size: 32,
        })
    ));
}

#[test]
fn hdr_to_sdr_capability_requires_p010_renderable_and_bt2446c_operator() {
    let settings = HdrToSdrSettings::default();

    let mut raw_hdr_without_p010 = RenderCapabilities::wgpu_nv12(Some(4096));
    raw_hdr_without_p010.supports_hdr_to_sdr = true;
    raw_hdr_without_p010
        .supported_hdr_to_sdr_operators
        .push(HdrToneMappingOperator::Bt2446C);

    let mut p010_without_operator = RenderCapabilities::wgpu_nv12(Some(4096));
    p010_without_operator
        .supported_frame_contracts
        .push(VideoFrameContract::dma_buf_p010(
            DmaBufImageLayout::SeparateLayers,
        ));
    p010_without_operator.p010_render_readiness = P010RenderReadiness::Renderable;
    p010_without_operator.supports_hdr_to_sdr = true;

    let production_capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));

    assert!(!raw_hdr_without_p010.supports_hdr_to_sdr_with(&settings));
    assert!(!p010_without_operator.supports_hdr_to_sdr_with(&settings));
    assert!(production_capabilities.supports_hdr_to_sdr_with(&settings));
    assert!(!production_capabilities.supports_native_hdr_output);
}

#[test]
fn p010_zero_copy_boundary_state_is_not_renderable() {
    let mut capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
    capabilities.p010_render_readiness = P010RenderReadiness::ZeroCopyBoundaryVerified;
    capabilities
        .supported_frame_contracts
        .push(VideoFrameContract::dma_buf_p010(
            DmaBufImageLayout::SeparateLayers,
        ));
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

    assert!(!capabilities.supports_p010_rendering());
    assert!(!capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
    assert!(matches!(
        capabilities.check_video_output(
            &requirement,
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        ),
        Err(RenderVideoOutputRejection::P010NotRenderable {
            readiness: P010RenderReadiness::ZeroCopyBoundaryVerified,
        })
    ));
    assert!(
        capabilities
            .summary_text()
            .contains("P010 zero-copy boundary verified")
    );
}

#[test]
fn current_wgpu_nv12_capabilities_reject_p010_as_unsupported_pixel_layout() {
    let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

    assert!(matches!(
        capabilities.check_video_output(
            &requirement,
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        ),
        Err(RenderVideoOutputRejection::FrameContract {
            reason: RenderFrameContractRejection::UnsupportedPixelLayout {
                pixel_layout: VideoFramePixelLayout::P010,
            },
        })
    ));
}

#[test]
fn current_wgpu_nv12_capabilities_reject_p010_before_hdr_policy() {
    let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
    let mut requirement =
        VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);
    requirement.hdr = true;

    assert!(matches!(
        capabilities.check_video_output(
            &requirement,
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        ),
        Err(RenderVideoOutputRejection::FrameContract {
            reason: RenderFrameContractRejection::UnsupportedPixelLayout {
                pixel_layout: VideoFramePixelLayout::P010,
            },
        })
    ));
}

#[test]
fn p010_bt2446c_capabilities_accept_ten_bit_hdr_but_not_native_hdr_output() {
    let capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420);
    requirement.hdr = true;

    assert!(capabilities.supports_video_output(
        &requirement,
        VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
    ));
    assert!(capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
    assert!(!capabilities.supports_native_hdr_output);
    assert!(capabilities.summary_text().contains("HDR available"));
    assert!(
        capabilities
            .summary_text()
            .contains("native HDR unsupported")
    );
}

#[test]
fn host_yuv420_hdr_policy_requires_high_bit_gpu_shader_contract() {
    let capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));
    let mut eight_bit_hdr_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420);
    eight_bit_hdr_requirement.hdr = true;
    let mut ten_bit_hdr_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420);
    ten_bit_hdr_requirement.hdr = true;
    let mut twelve_bit_hdr_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_bit_depth(BitDepth::Twelve)
        .with_chroma(ChromaSubsampling::Yuv420);
    twelve_bit_hdr_requirement.hdr = true;

    assert!(matches!(
        capabilities.check_video_output(
            &eight_bit_hdr_requirement,
            VideoFrameContract::host_yuv420_planar8(),
        ),
        Err(RenderVideoOutputRejection::HdrUnsupported { .. })
    ));
    assert!(capabilities.supports_video_output(
        &ten_bit_hdr_requirement,
        VideoFrameContract::host_yuv420_planar10le(),
    ));
    assert!(capabilities.supports_video_output(
        &twelve_bit_hdr_requirement,
        VideoFrameContract::host_yuv420_planar12le(),
    ));
}
