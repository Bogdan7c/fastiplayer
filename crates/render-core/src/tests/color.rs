use std::time::Duration;

use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction,
    VideoColorMetadata, VideoDisplayOrientation,
};
use video_frame_contract::VideoFramePixelLayout;

use super::*;
use crate::*;
#[test]
fn color_pipeline_settings_default_is_identity() {
    let settings = ColorPipelineSettings::default();

    assert_eq!(settings.adjustment, ColorAdjustment::identity());
    assert_eq!(settings.tone_mapping, ToneMappingMode::Off);
    assert_eq!(
        settings.swapchain_transfer,
        SwapchainTransferMode::PreserveCurrentUnorm
    );
    assert!(settings.is_identity());
}

#[test]
fn hdr_to_sdr_settings_default_to_bt2446c_sdr_bt709_contract() {
    let settings = HdrToSdrSettings::default();

    assert!(settings.enabled);
    assert_eq!(settings.operator, HdrToneMappingOperator::Bt2446C);
    assert_eq!(settings.output_mode, HdrOutputMode::SdrBt709Only);
    assert_eq!(settings.sdr_reference_white_nits, 100.0);
    assert_eq!(settings.hdr_reference_peak_nits, 1_000.0);
    assert!(settings.is_phase10_bt2446_c_sdr_bt709());
}

#[test]
fn active_color_path_describes_current_nv12_bt709_limited_sdr_path() {
    let frame = RenderableFrame {
        handle: 7,
        pts: Duration::ZERO,
        format: VideoFramePixelLayout::Nv12,
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        coded_width: 1920,
        coded_height: 1080,
        render_width: 1920,
        render_height: 1080,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
    };
    let settings = ColorPipelineSettings::default();

    let active_path = ActiveColorPath::from_frame(&frame, &settings);

    assert_eq!(active_path.input_format, VideoFramePixelLayout::Nv12);
    assert_eq!(active_path.input_bit_depth, BitDepth::Eight);
    assert_eq!(active_path.input_chroma, ChromaSubsampling::Yuv420);
    assert_eq!(active_path.input_color.range, ColorRange::Limited);
    assert_eq!(active_path.input_color.matrix, MatrixCoefficients::Bt709);
    assert_eq!(active_path.fallback, None);
    assert_eq!(
        active_path.diagnostic_text(),
        "NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm"
    );
}

#[test]
fn active_color_path_marks_bt2020_sdr_as_sdr_bt709_fallback() {
    let color = VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Bt709,
        hdr_metadata: None,
        origin: codec_core::ColorMetadataOrigin::Container,
        confidence: codec_core::ColorMetadataConfidence::Hint,
    };
    let settings = ColorPipelineSettings::default();

    let active_path = ActiveColorPath::from_parts(
        VideoFramePixelLayout::Nv12,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        color,
        &settings,
    );

    assert_eq!(
        active_path.fallback,
        Some(ActiveColorPathFallback::WideGamutToSdrBt709)
    );
    assert_eq!(
        active_path.diagnostic_text(),
        "NV12 8-bit BT.2020 limited -> SDR BT.709 fallback preserve-current-unorm"
    );
}

#[test]
fn active_color_path_describes_p010_hdr_to_sdr_bt2446c_path() {
    let color = VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        hdr_metadata: None,
        origin: codec_core::ColorMetadataOrigin::Bitstream,
        confidence: codec_core::ColorMetadataConfidence::Confirmed,
    };
    let settings = ColorPipelineSettings {
        swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
        ..ColorPipelineSettings::default()
    };

    let active_path = ActiveColorPath::from_parts_with_hdr_to_sdr(
        VideoFramePixelLayout::P010,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
        color,
        &settings,
        Some(HdrToSdrSettings::default()),
    );

    assert_eq!(active_path.fallback, None);
    assert_eq!(
        active_path.hdr_to_sdr.map(|settings| settings.operator),
        Some(HdrToneMappingOperator::Bt2446C)
    );
    assert_eq!(
        active_path.diagnostic_text(),
        "P010 10-bit BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf"
    );
}

#[test]
fn active_color_path_describes_host_yuv420_hdr_to_sdr_bt2446c_path() {
    let color = VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        hdr_metadata: None,
        origin: codec_core::ColorMetadataOrigin::Bitstream,
        confidence: codec_core::ColorMetadataConfidence::Confirmed,
    };
    let settings = ColorPipelineSettings {
        swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
        ..ColorPipelineSettings::default()
    };

    let active_path = ActiveColorPath::from_parts_with_hdr_to_sdr(
        VideoFramePixelLayout::Yuv420Planar10Le,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
        color,
        &settings,
        Some(HdrToSdrSettings::default()),
    );

    assert_eq!(active_path.fallback, None);
    assert_eq!(
        active_path.hdr_to_sdr.map(|settings| settings.operator),
        Some(HdrToneMappingOperator::Bt2446C)
    );
    assert_eq!(
        active_path.diagnostic_text(),
        "YUV420 planar 10-bit little-endian 10-bit BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf"
    );
}

#[test]
fn active_color_path_keeps_hdr_fallback_without_explicit_hdr_to_sdr_contract() {
    let color = VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        hdr_metadata: None,
        origin: codec_core::ColorMetadataOrigin::Bitstream,
        confidence: codec_core::ColorMetadataConfidence::Confirmed,
    };
    let settings = ColorPipelineSettings::default();

    let active_path = ActiveColorPath::from_parts(
        VideoFramePixelLayout::P010,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
        color,
        &settings,
    );

    assert_eq!(
        active_path.fallback,
        Some(ActiveColorPathFallback::UnsupportedHdrInput)
    );
    assert_eq!(active_path.hdr_to_sdr, None);
}

#[test]
fn active_color_path_treats_bt709_content_light_side_metadata_as_sdr() {
    let color = VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Bt709,
        hdr_metadata: Some(codec_core::HdrMetadata {
            color_primaries: ColorPrimaries::Bt2020,
            transfer_function: TransferFunction::Bt709,
            max_luminance_nits: Some(1_000.0),
            min_luminance_nits: Some(0.01),
            max_content_light_level_nits: Some(1_000),
            max_frame_average_light_level_nits: Some(400),
        }),
        origin: codec_core::ColorMetadataOrigin::Container,
        confidence: codec_core::ColorMetadataConfidence::Hint,
    };
    let settings = ColorPipelineSettings {
        swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
        ..ColorPipelineSettings::default()
    };

    let active_path = ActiveColorPath::from_parts_with_hdr_to_sdr(
        VideoFramePixelLayout::P010,
        BitDepth::Ten,
        ChromaSubsampling::Yuv420,
        color,
        &settings,
        Some(HdrToSdrSettings::default()),
    );

    assert_eq!(
        active_path.fallback,
        Some(ActiveColorPathFallback::WideGamutToSdrBt709)
    );
    assert_eq!(active_path.hdr_to_sdr, None);
    assert!(!active_path.diagnostic_text().contains("bt2446-c"));
}
