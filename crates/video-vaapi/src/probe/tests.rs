use super::*;

mod av1;

#[test]
fn intel_vendor_string_is_normalized_to_driver_family() {
    assert_eq!(
        normalize_driver_name("Intel iHD driver for Intel(R) Gen Graphics"),
        Some("intel-ihd".to_string())
    );
    assert_eq!(
        normalize_driver_name("Intel i965 driver for Intel(R) Broadwell"),
        Some("intel-i965".to_string())
    );
    assert_eq!(
        normalize_driver_name("Mesa Gallium driver"),
        Some("mesa".to_string())
    );
}

#[test]
fn vp9_profile0_yuv420_mask_builds_profile0_format() {
    let formats = formats_for_va_profile(
        libva::VAProfile::VAProfileVP9Profile0,
        libva::VA_RT_FORMAT_YUV420,
        MaxResolution {
            width: Some(1920),
            height: Some(1080),
        },
    );

    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].codec, VideoCodec::Vp9);
    assert_eq!(formats[0].profile, VideoProfile::Vp9(Vp9Profile::Profile0));
    assert_eq!(formats[0].bit_depth, BitDepth::Eight);
    assert_eq!(formats[0].chroma, ChromaSubsampling::Yuv420);
    assert_eq!(
        vaapi_output_contracts_for_format(&formats[0], Some("mesa")),
        vec![
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        ]
    );
}

#[test]
fn vp9_profile2_yuv420_10_reports_baseline_p010_layout() {
    let formats = formats_for_va_profile(
        libva::VAProfile::VAProfileVP9Profile2,
        libva::VA_RT_FORMAT_YUV420_10,
        MaxResolution {
            width: Some(3840),
            height: Some(2160),
        },
    );

    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].codec, VideoCodec::Vp9);
    assert_eq!(formats[0].profile, VideoProfile::Vp9(Vp9Profile::Profile2));
    assert_eq!(formats[0].bit_depth, BitDepth::Ten);
    assert_eq!(formats[0].chroma, ChromaSubsampling::Yuv420);
    assert_eq!(
        vaapi_output_contracts_for_format(&formats[0], Some("intel-i965")),
        vec![
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
        ]
    );
    assert_eq!(
        vaapi_output_contracts_for_format(&formats[0], None),
        vec![
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
        ]
    );
    assert_eq!(
        vaapi_output_contracts_for_format(&formats[0], Some("mesa")),
        vec![
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        ]
    );
}

#[test]
fn capability_probe_advertises_implemented_h264_8bit_yuv420_slot() {
    let formats = formats_for_va_profile(
        libva::VAProfile::VAProfileH264High,
        libva::VA_RT_FORMAT_YUV420 | libva::VA_RT_FORMAT_YUV420_10,
        MaxResolution {
            width: Some(1920),
            height: Some(1080),
        },
    );

    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].codec, VideoCodec::H264);
    assert_eq!(formats[0].profile, VideoProfile::H264(H264Profile::High));
    assert_eq!(formats[0].bit_depth, BitDepth::Eight);
    assert_eq!(formats[0].chroma, ChromaSubsampling::Yuv420);
}

/// Проверяет, что ordinary Baseline появляется только из exact VA profile,
/// а неподдерживаемый 10-bit RT format не расширяет production matrix.
#[test]
fn h264_baseline_va_profile_maps_to_exact_8bit_yuv420_slot() {
    let formats = formats_for_va_profile(
        libva::VAProfile::VAProfileH264Baseline,
        libva::VA_RT_FORMAT_YUV420 | libva::VA_RT_FORMAT_YUV420_10,
        MaxResolution {
            width: Some(1920),
            height: Some(1080),
        },
    );

    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].codec, VideoCodec::H264);
    assert_eq!(
        formats[0].profile,
        VideoProfile::H264(H264Profile::Baseline)
    );
    assert_eq!(formats[0].bit_depth, BitDepth::Eight);
    assert_eq!(formats[0].chroma, ChromaSubsampling::Yuv420);
    assert_eq!(formats[0].max_width, Some(1920));
    assert_eq!(formats[0].max_height, Some(1080));
}

/// Закрепляет отсутствие опасного alias ordinary Baseline на Constrained Baseline.
#[test]
fn constrained_baseline_va_profile_does_not_advertise_ordinary_baseline() {
    let formats = formats_for_va_profile(
        libva::VAProfile::VAProfileH264ConstrainedBaseline,
        libva::VA_RT_FORMAT_YUV420,
        MaxResolution {
            width: Some(1920),
            height: Some(1080),
        },
    );

    assert_eq!(formats.len(), 1);
    assert_eq!(
        formats[0].profile,
        VideoProfile::H264(H264Profile::ConstrainedBaseline)
    );
    assert_ne!(
        formats[0].profile,
        VideoProfile::H264(H264Profile::Baseline)
    );
}

/// Фиксирует безопасные labels обоих разных VA-API Baseline profiles.
#[test]
fn h264_baseline_profile_labels_are_distinct() {
    assert_eq!(
        profile_label(libva::VAProfile::VAProfileH264Baseline),
        "VAProfileH264Baseline"
    );
    assert_eq!(
        profile_label(libva::VAProfile::VAProfileH264ConstrainedBaseline),
        "VAProfileH264ConstrainedBaseline"
    );
}

#[test]
fn capability_probe_advertises_h265_main_and_main10_yuv420_slots() {
    let max_resolution = MaxResolution {
        width: Some(3840),
        height: Some(2160),
    };
    let main_formats = formats_for_va_profile(
        libva::VAProfile::VAProfileHEVCMain,
        libva::VA_RT_FORMAT_YUV420 | libva::VA_RT_FORMAT_YUV420_10,
        max_resolution,
    );
    let main10_formats = formats_for_va_profile(
        libva::VAProfile::VAProfileHEVCMain10,
        libva::VA_RT_FORMAT_YUV420 | libva::VA_RT_FORMAT_YUV420_10,
        max_resolution,
    );

    assert_eq!(main_formats.len(), 1);
    assert_eq!(main_formats[0].codec, VideoCodec::H265);
    assert_eq!(
        main_formats[0].profile,
        VideoProfile::H265(H265Profile::Main)
    );
    assert_eq!(main_formats[0].bit_depth, BitDepth::Eight);
    assert_eq!(main_formats[0].chroma, ChromaSubsampling::Yuv420);
    assert!(!main_formats[0].hdr_input);
    assert_eq!(main10_formats.len(), 1);
    assert_eq!(main10_formats[0].codec, VideoCodec::H265);
    assert_eq!(
        main10_formats[0].profile,
        VideoProfile::H265(H265Profile::Main10)
    );
    assert_eq!(main10_formats[0].bit_depth, BitDepth::Ten);
    assert_eq!(main10_formats[0].chroma, ChromaSubsampling::Yuv420);
    assert!(main10_formats[0].hdr_input);
}

#[test]
fn capability_probe_does_not_advertise_unimplemented_vp9_profiles() {
    let profile1_formats = formats_for_va_profile(
        libva::VAProfile::VAProfileVP9Profile1,
        libva::VA_RT_FORMAT_YUV422,
        MaxResolution {
            width: Some(1920),
            height: Some(1080),
        },
    );
    let profile3_formats = formats_for_va_profile(
        libva::VAProfile::VAProfileVP9Profile3,
        libva::VA_RT_FORMAT_YUV420_10,
        MaxResolution {
            width: Some(1920),
            height: Some(1080),
        },
    );

    assert!(profile1_formats.is_empty());
    assert!(profile3_formats.is_empty());
}

#[test]
fn capability_probe_does_not_advertise_future_vp8_without_adapter() {
    let profile = libva::VAProfile::VAProfileVP8Version0_3;
    let formats = formats_for_va_profile(
        profile,
        libva::VA_RT_FORMAT_YUV420,
        MaxResolution {
            width: Some(1920),
            height: Some(1080),
        },
    );

    assert!(formats.is_empty(), "{profile:?} must not be advertised");
}

#[test]
fn capability_probe_does_not_advertise_future_hevc_profiles() {
    for (profile, rt_format) in [
        (
            libva::VAProfile::VAProfileHEVCMain12,
            libva::VA_RT_FORMAT_YUV420_12,
        ),
        (
            libva::VAProfile::VAProfileHEVCMain422_10,
            libva::VA_RT_FORMAT_YUV422_10,
        ),
        (
            libva::VAProfile::VAProfileHEVCMain444,
            libva::VA_RT_FORMAT_YUV444,
        ),
        (
            libva::VAProfile::VAProfileHEVCSccMain,
            libva::VA_RT_FORMAT_YUV420,
        ),
    ] {
        let formats = formats_for_va_profile(
            profile,
            rt_format,
            MaxResolution {
                width: Some(1920),
                height: Some(1080),
            },
        );

        assert!(formats.is_empty(), "{profile:?} must not be advertised");
    }
}
