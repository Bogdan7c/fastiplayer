use codec_core::{BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement};
use render_core::{HdrToSdrSettings, RenderVideoOutputRejection};
use video_frame_contract::DmaBufImageLayout;

use super::*;

fn hdr_requirement(bit_depth: BitDepth, chroma: ChromaSubsampling) -> VideoDecodeRequirement {
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::H265)
        .with_bit_depth(bit_depth)
        .with_chroma(chroma);
    requirement.hdr = true;
    requirement
}

#[test]
fn highp_declares_the_v1_high_bit_matrix_and_hdr_to_sdr() {
    let capabilities =
        gles2_render_capabilities_for(ShaderPrecision::High, 8192, HardwareZeroCopy::Unavailable);
    for contract in GLES2_SOFTWARE_FRAME_CONTRACTS
        .into_iter()
        .chain(GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS)
    {
        assert!(
            capabilities.supports_frame_contract(contract),
            "{contract:?}"
        );
    }
    assert_eq!(capabilities.supported_frame_contracts.len(), 8);
    // 4:4:4 12 бит вне матрицы v1 (как у wgpu).
    assert!(
        !capabilities.supports_frame_contract(software_host_upload(VideoFramePixelLayout::Rgba8))
    );
    assert_eq!(capabilities.max_texture_size, Some(8192));
    let settings = HdrToSdrSettings::default();
    for contract in GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS {
        assert!(
            capabilities.supports_hdr_to_sdr_for_contract(&settings, contract),
            "{contract:?}"
        );
    }
    let ten_bit_420 = software_host_upload(VideoFramePixelLayout::Yuv420Planar10Le);
    assert_eq!(
        capabilities.check_video_output(
            &hdr_requirement(BitDepth::Ten, ChromaSubsampling::Yuv420),
            ten_bit_420
        ),
        Ok(())
    );
}

#[test]
fn hdr_stays_unavailable_for_eight_bit_and_hardware_contracts() {
    let capabilities =
        gles2_render_capabilities_for(ShaderPrecision::High, 8192, HardwareZeroCopy::Unavailable);
    let eight_bit = software_host_upload(VideoFramePixelLayout::Yuv420Planar8);
    assert_eq!(
        capabilities.check_video_output(
            &hdr_requirement(BitDepth::Eight, ChromaSubsampling::Yuv420),
            eight_bit
        ),
        Err(RenderVideoOutputRejection::HdrUnsupported {
            frame_contract: eight_bit
        })
    );
    assert!(!capabilities.supports_p010_rendering());
}

/// Без `highp` — только 8 бит, без HDR, кадр не шире 2048: `auto` не предложит 10/12 бит.
#[test]
fn mediump_declares_only_eight_bit_without_hdr() {
    let capabilities =
        gles2_render_capabilities_for(ShaderPrecision::Medium, 8192, HardwareZeroCopy::Unavailable);
    assert_eq!(
        capabilities.supported_frame_contracts,
        GLES2_SOFTWARE_FRAME_CONTRACTS.to_vec()
    );
    assert!(!capabilities.supports_hdr_to_sdr);
    assert!(capabilities.supported_hdr_to_sdr_operators.is_empty());
    assert_eq!(capabilities.max_texture_size, Some(2048));
    let ten_bit_420 = software_host_upload(VideoFramePixelLayout::Yuv420Planar10Le);
    assert!(matches!(
        capabilities.check_video_output(
            &hdr_requirement(BitDepth::Ten, ChromaSubsampling::Yuv420),
            ten_bit_420
        ),
        Err(RenderVideoOutputRejection::FrameContract { .. })
    ));
    assert!(!capabilities.summary_text().contains("HDR available"));
}

fn hardware_contracts_supported(capabilities: &RenderCapabilities) -> Vec<VideoFrameContract> {
    [
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
        VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
    ]
    .into_iter()
    .filter(|contract| capabilities.supports_frame_contract(*contract))
    .collect()
}

/// Уровень zero-copy есть: оба аппаратных NV12 объявлены ВМЕСТЕ с прежними программными, а
/// P010 не объявлен ни в одной раскладке (решение владельца 33).
#[test]
fn zero_copy_tier_declares_both_nv12_layouts_and_keeps_software_contracts() {
    for precision in [ShaderPrecision::High, ShaderPrecision::Medium] {
        let capabilities =
            gles2_render_capabilities_for(precision, 8192, HardwareZeroCopy::Available);
        assert_eq!(
            hardware_contracts_supported(&capabilities),
            GLES2_HARDWARE_FRAME_CONTRACTS.to_vec(),
            "{precision:?}"
        );
        for contract in GLES2_SOFTWARE_FRAME_CONTRACTS {
            assert!(
                capabilities.supports_frame_contract(contract),
                "{precision:?} {contract:?}"
            );
        }
        assert!(!capabilities.supports_p010_rendering());
        assert!(capabilities.display_name.contains("NV12"));
        assert!(!capabilities.display_name.contains("P010"));
    }
}

/// Уровня нет: ни одного аппаратного контракта, имя не обещает zero-copy.
#[test]
fn without_zero_copy_tier_no_hardware_contract_is_declared() {
    let capabilities =
        gles2_render_capabilities_for(ShaderPrecision::High, 8192, HardwareZeroCopy::Unavailable);
    assert!(hardware_contracts_supported(&capabilities).is_empty());
    assert!(!capabilities.display_name.contains("zero-copy"));
    assert!(
        capabilities
            .supported_frame_contracts
            .iter()
            .all(|contract| contract.transfer_path == VideoFrameTransferPath::SoftwareHostUpload)
    );
}

/// Выбор декодера: аппаратный NV12 без уровня отвергается как «контракт не объявлен».
#[test]
fn hardware_nv12_is_rejected_by_output_check_without_the_tier() {
    let requirement = VideoDecodeRequirement::new(VideoCodec::H264);
    let hardware = VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers);
    let without =
        gles2_render_capabilities_for(ShaderPrecision::High, 8192, HardwareZeroCopy::Unavailable);
    let with =
        gles2_render_capabilities_for(ShaderPrecision::High, 8192, HardwareZeroCopy::Available);
    assert!(matches!(
        without.check_video_output(&requirement, hardware),
        Err(RenderVideoOutputRejection::FrameContract { .. })
    ));
    assert_eq!(with.check_video_output(&requirement, hardware), Ok(()));
}
