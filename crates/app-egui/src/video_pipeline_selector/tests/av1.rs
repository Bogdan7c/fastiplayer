//! AV1-specific selector matrix вынесена из уже крупного production selector module.

use codec_core::Av1Profile;

use super::*;

/// AV1 Main 8/10-bit выбирает один VA-API plan и в auto, и в strict hardware режиме.
#[test]
fn advertised_av1_main_outputs_select_vaapi_for_auto_and_hardware() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();

    for bit_depth in [BitDepth::Eight, BitDepth::Ten] {
        let capabilities = capabilities_with_playable_outputs(vec![
            ffmpeg_av1_main_output(bit_depth),
            vaapi_av1_main_output(bit_depth),
        ]);
        let stream_requirement = av1_main_requirement(bit_depth);

        for preferred_backend in [
            VideoBackendPreference::Auto,
            VideoBackendPreference::Hardware,
        ] {
            let plan = select_video_pipeline_plan(
                preferred_backend,
                Some(&capabilities),
                decoder_thread_config,
                Some(&stream_requirement),
            )
            .expect("advertised AV1 Main VA-API output should be selected");

            assert_eq!(
                plan,
                VideoPipelinePlan::VaapiDmaBufWgpu {
                    decoder_thread_config,
                },
                "unexpected AV1 plan for {preferred_backend:?}, {bit_depth:?}"
            );
        }
    }
}

/// Auto сохраняет software fallback, когда hardware рекламирует только другой codec.
#[test]
fn av1_auto_falls_back_to_ffmpeg_when_no_hardware_av1_is_advertised() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();

    for bit_depth in [BitDepth::Eight, BitDepth::Ten] {
        let capabilities = capabilities_with_playable_outputs(vec![
            current_vaapi_output(),
            ffmpeg_av1_main_output(bit_depth),
        ]);
        let stream_requirement = av1_main_requirement(bit_depth);

        let plan = select_video_pipeline_plan(
            VideoBackendPreference::Auto,
            Some(&capabilities),
            decoder_thread_config,
            Some(&stream_requirement),
        )
        .expect("auto should use FFmpeg when AV1 hardware output is absent");

        assert_eq!(
            plan,
            VideoPipelinePlan::FfmpegHostUploadWgpu {
                decoder_thread_config,
            },
            "unexpected AV1 fallback plan for {bit_depth:?}"
        );
    }
}

/// Strict hardware не имеет права молча использовать совместимый software AV1 output.
#[test]
fn av1_hardware_preference_does_not_fall_back_to_ffmpeg() {
    for bit_depth in [BitDepth::Eight, BitDepth::Ten] {
        let capabilities = capabilities_with_playable_outputs(vec![
            current_vaapi_output(),
            ffmpeg_av1_main_output(bit_depth),
        ]);
        let stream_requirement = av1_main_requirement(bit_depth);

        let error = select_video_pipeline_plan(
            VideoBackendPreference::Hardware,
            Some(&capabilities),
            PlayerVideoDecoderThreadConfig::default(),
            Some(&stream_requirement),
        )
        .expect_err("strict hardware AV1 must not fall back to FFmpeg");

        match error {
            VideoPipelineSelectionError::HardwareOutputDoesNotServeStream { requirement } => {
                assert!(
                    requirement.contains("AV1"),
                    "AV1 rejection must describe the rejected requirement, got {requirement}"
                )
            }
            unexpected_error => panic!(
                "expected AV1-specific hardware rejection for {bit_depth:?}, got {unexpected_error:?}"
            ),
        }
    }
}

/// Строит точный AV1 Main/YUV420 format для проверяемой bit-depth ветки.
fn av1_main_format(bit_depth: BitDepth) -> SupportedVideoDecodeFormat {
    SupportedVideoDecodeFormat {
        codec: VideoCodec::Av1,
        profile: VideoProfile::Av1(Av1Profile::Main),
        bit_depth,
        chroma: ChromaSubsampling::Yuv420,
        max_width: Some(3840),
        max_height: Some(2160),
        max_fps: Some(60.0),
        hdr_input: bit_depth == BitDepth::Ten,
    }
}

/// Строит renderer-intersected VA-API output для AV1 Main 8/10-bit.
fn vaapi_av1_main_output(bit_depth: BitDepth) -> SupportedVideoOutput {
    let frame_contract = match bit_depth {
        BitDepth::Eight => VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        BitDepth::Ten => VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        unsupported_bit_depth => {
            panic!("test helper supports AV1 Main 8/10-bit only, got {unsupported_bit_depth:?}")
        }
    };

    SupportedVideoOutput {
        backend: DecodeBackendId::vaapi(),
        decode_format: av1_main_format(bit_depth),
        frame_contract,
    }
}

/// Строит renderer-intersected FFmpeg fallback для того же AV1 stream requirement.
fn ffmpeg_av1_main_output(bit_depth: BitDepth) -> SupportedVideoOutput {
    let frame_contract = match bit_depth {
        BitDepth::Eight => VideoFrameContract::host_yuv420_planar8(),
        BitDepth::Ten => VideoFrameContract::host_yuv420_planar10le(),
        unsupported_bit_depth => {
            panic!("test helper supports AV1 Main 8/10-bit only, got {unsupported_bit_depth:?}")
        }
    };

    SupportedVideoOutput {
        backend: DecodeBackendId::new(FFMPEG_SOFTWARE_BACKEND_ID).expect("valid FFmpeg backend id"),
        decode_format: av1_main_format(bit_depth),
        frame_contract,
    }
}

/// Строит stream requirement, которым selector обязан отфильтровать соседние codecs.
fn av1_main_requirement(bit_depth: BitDepth) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::Av1)
        .with_profile(VideoProfile::Av1(Av1Profile::Main))
        .with_bit_depth(bit_depth)
        .with_chroma(ChromaSubsampling::Yuv420)
}
