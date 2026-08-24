//! AV1 probe matrix вынесена из уже крупного VA-API capability module.

use super::*;

/// Profile 0 публикует только production AV1 Main 8/10-bit YUV420 rows.
#[test]
fn capability_probe_advertises_av1_main_8bit_and_10bit_yuv420_slots() {
    let formats = formats_for_va_profile(
        libva::VAProfile::VAProfileAV1Profile0,
        libva::VA_RT_FORMAT_YUV420
            | libva::VA_RT_FORMAT_YUV420_10
            | libva::VA_RT_FORMAT_YUV420_12
            | libva::VA_RT_FORMAT_YUV444,
        MaxResolution {
            width: Some(7_680),
            height: Some(4_320),
        },
    );

    assert_eq!(formats.len(), 2);
    assert!(formats.iter().any(|format| {
        format.codec == VideoCodec::Av1
            && format.profile == VideoProfile::Av1(Av1Profile::Main)
            && format.bit_depth == BitDepth::Eight
            && format.chroma == ChromaSubsampling::Yuv420
            && !format.hdr_input
    }));
    assert!(formats.iter().any(|format| {
        format.codec == VideoCodec::Av1
            && format.profile == VideoProfile::Av1(Av1Profile::Main)
            && format.bit_depth == BitDepth::Ten
            && format.chroma == ChromaSubsampling::Yuv420
            && format.hdr_input
    }));
    assert!(
        formats
            .iter()
            .all(|format| format.max_width == Some(7_680) && format.max_height == Some(4_320))
    );
}

/// Profile 1/High остаётся за пределами заявленной production matrix.
#[test]
fn capability_probe_does_not_advertise_av1_high_profile() {
    let formats = formats_for_va_profile(
        libva::VAProfile::VAProfileAV1Profile1,
        libva::VA_RT_FORMAT_YUV444 | libva::VA_RT_FORMAT_YUV444_10,
        MaxResolution {
            width: Some(3_840),
            height: Some(2_160),
        },
    );

    assert!(formats.is_empty());
}
