use codec_core::{BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement};
use render_core::{HdrToSdrSettings, RenderVideoOutputRejection};

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
    let capabilities = gles2_render_capabilities_for(ShaderPrecision::High, 8192);
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
    let capabilities = gles2_render_capabilities_for(ShaderPrecision::High, 8192);
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
    let capabilities = gles2_render_capabilities_for(ShaderPrecision::Medium, 8192);
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
