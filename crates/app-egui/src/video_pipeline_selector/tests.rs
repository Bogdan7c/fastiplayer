use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
    VideoProfile, Vp9Profile,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::*;

mod av1;

fn current_vaapi_output() -> SupportedVideoOutput {
    SupportedVideoOutput {
        backend: DecodeBackendId::vaapi(),
        decode_format: vp9_profile0_format(),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
    }
}

fn non_vaapi_dma_buf_output() -> SupportedVideoOutput {
    SupportedVideoOutput {
        backend: DecodeBackendId::new("future_backend").expect("valid test backend id"),
        decode_format: vp9_profile0_format(),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
    }
}

fn host_upload_vaapi_output() -> SupportedVideoOutput {
    SupportedVideoOutput {
        backend: DecodeBackendId::vaapi(),
        decode_format: vp9_profile0_format(),
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    }
}

fn ffmpeg_host_upload_output() -> SupportedVideoOutput {
    SupportedVideoOutput {
        backend: DecodeBackendId::new(FFMPEG_SOFTWARE_BACKEND_ID).expect("valid FFmpeg backend id"),
        decode_format: vp9_profile0_format(),
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    }
}

fn future_software_output() -> SupportedVideoOutput {
    SupportedVideoOutput {
        backend: DecodeBackendId::new("future_sw").expect("valid future software backend id"),
        decode_format: vp9_profile0_format(),
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    }
}

fn ffmpeg_vp9_profile2_output() -> SupportedVideoOutput {
    SupportedVideoOutput {
        backend: DecodeBackendId::new(FFMPEG_SOFTWARE_BACKEND_ID).expect("valid FFmpeg backend id"),
        decode_format: SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile2),
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(3840),
            max_height: Some(2160),
            max_fps: Some(60.0),
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    }
}

#[test]
fn auto_with_stream_requirement_prefers_hardware_then_falls_back_to_ffmpeg() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let capabilities = capabilities_with_playable_outputs(vec![
        current_vaapi_output(),
        ffmpeg_host_upload_output(),
        ffmpeg_vp9_profile2_output(),
    ]);

    // Стрим, который тянет hardware (VP9 profile0 8-bit) → hardware zero-copy.
    let hardware_stream = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420);
    let hardware_plan = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        Some(&capabilities),
        decoder_thread_config,
        Some(&hardware_stream),
    )
    .expect("Auto must select hardware for hw-decodable streaming");
    assert_eq!(
        hardware_plan.backend_kind(),
        VideoBackendKind::HardwareZeroCopy
    );

    // Стрим, который hardware не тянет (VP9 profile2 10-bit, только software) → ffmpeg.
    let software_stream = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420);
    let software_plan = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        Some(&capabilities),
        decoder_thread_config,
        Some(&software_stream),
    )
    .expect("Auto should fall on ffmpeg for software-only streaming");
    assert_eq!(
        software_plan.backend_kind(),
        VideoBackendKind::FfmpegSoftware
    );
}

/// Dynamic на старте файла выбирает ровно тот же план, что Auto: HW для HW-тянущего потока и
/// ffmpeg для потока, который HW не тянет.
#[test]
fn dynamic_preference_selects_same_plan_as_auto_at_file_start() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let capabilities = capabilities_with_playable_outputs(vec![
        current_vaapi_output(),
        ffmpeg_host_upload_output(),
        ffmpeg_vp9_profile2_output(),
    ]);
    let hardware_stream = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
        .with_bit_depth(BitDepth::Eight)
        .with_chroma(ChromaSubsampling::Yuv420);
    let software_stream = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420);

    for stream in [&hardware_stream, &software_stream] {
        let select = |preference| {
            select_video_pipeline_plan(
                preference,
                Some(&capabilities),
                decoder_thread_config,
                Some(stream),
            )
        };
        assert_eq!(
            select(VideoBackendPreference::Dynamic).expect("Dynamic must select a plan"),
            select(VideoBackendPreference::Auto).expect("Auto must select a plan"),
        );
    }
    let dynamic_hardware_plan = select_video_pipeline_plan(
        VideoBackendPreference::Dynamic,
        Some(&capabilities),
        decoder_thread_config,
        Some(&hardware_stream),
    )
    .expect("Dynamic must select a plan");
    assert_eq!(
        dynamic_hardware_plan.backend_kind(),
        VideoBackendKind::HardwareZeroCopy
    );
}

#[test]
fn hardware_preference_rejects_stream_no_hardware_output_can_decode() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let capabilities = capabilities_with_playable_outputs(vec![
        current_vaapi_output(),
        ffmpeg_vp9_profile2_output(),
    ]);

    let software_only_stream = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420);
    let error = select_video_pipeline_plan(
        VideoBackendPreference::Hardware,
        Some(&capabilities),
        decoder_thread_config,
        Some(&software_only_stream),
    )
    .expect_err("hardware preference should not fall software streamingly");
    match error {
        VideoPipelineSelectionError::HardwareOutputDoesNotServeStream { requirement } => {
            assert!(
                requirement.contains("VP9"),
                "diagnostic must name the stream requirement, got {requirement}"
            );
        }
        unexpected_error => {
            panic!("Expected stream-specific hardware rejection, got {unexpected_error:?}");
        }
    }
}

fn vp9_profile0_format() -> SupportedVideoDecodeFormat {
    SupportedVideoDecodeFormat {
        codec: VideoCodec::Vp9,
        profile: VideoProfile::Vp9(Vp9Profile::Profile0),
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        max_width: Some(3840),
        max_height: Some(2160),
        max_fps: Some(60.0),
        hdr_input: false,
    }
}

fn capabilities_with_playable_outputs(
    playable_video_outputs: Vec<SupportedVideoOutput>,
) -> SystemCapabilities {
    capabilities_with_raw_and_playable_outputs(
        playable_video_outputs.clone(),
        playable_video_outputs,
    )
}

fn capabilities_with_raw_and_playable_outputs(
    raw_supported_outputs: Vec<SupportedVideoOutput>,
    playable_video_outputs: Vec<SupportedVideoOutput>,
) -> SystemCapabilities {
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id: DecodeBackendId::vaapi(),
            display_name: "Test VA-API".to_string(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs,
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs,
    }
}

#[test]
fn auto_prefers_vaapi_when_both_vaapi_and_ffmpeg_are_playable() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let capabilities = capabilities_with_playable_outputs(vec![
        ffmpeg_host_upload_output(),
        current_vaapi_output(),
    ]);

    let plan = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        Some(&capabilities),
        decoder_thread_config,
        None,
    )
    .expect("auto should select current VA-API path");

    assert_eq!(
        plan,
        VideoPipelinePlan::VaapiDmaBuf {
            decoder_thread_config,
        }
    );
}

#[test]
fn hardware_preference_requires_native_hardware_dma_buf_output() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let capabilities = capabilities_with_playable_outputs(vec![
        non_vaapi_dma_buf_output(),
        current_vaapi_output(),
    ]);

    let plan = select_video_pipeline_plan(
        VideoBackendPreference::Hardware,
        Some(&capabilities),
        decoder_thread_config,
        None,
    )
    .expect("hardware preference should select current native hardware path");

    assert_eq!(
        plan,
        VideoPipelinePlan::VaapiDmaBuf {
            decoder_thread_config,
        }
    );
}

#[test]
fn auto_falls_back_to_software_branch_when_no_playable_hardware_exists() {
    let capabilities = capabilities_with_playable_outputs(Vec::new());

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("auto should try software branch when no playable hardware exists");

    assert_eq!(error, VideoPipelineSelectionError::MissingSoftwareOutput);
}

#[test]
fn auto_selects_ffmpeg_when_hardware_cannot_play_but_software_can() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let capabilities = capabilities_with_playable_outputs(vec![ffmpeg_host_upload_output()]);

    let plan = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        Some(&capabilities),
        decoder_thread_config,
        None,
    )
    .expect("auto should use FFmpeg when no playable hardware output exists");

    assert_eq!(
        plan,
        VideoPipelinePlan::FfmpegHostUpload {
            decoder_thread_config,
        }
    );
}

#[test]
fn auto_does_not_recompute_playability_from_raw_vaapi_outputs() {
    let capabilities =
        capabilities_with_raw_and_playable_outputs(vec![current_vaapi_output()], Vec::new());

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("raw VA-API output must not bypass playable output policy");

    assert_eq!(error, VideoPipelineSelectionError::MissingSoftwareOutput);
}

#[test]
fn missing_native_hardware_in_hardware_preference_is_explicit_error() {
    let capabilities = capabilities_with_playable_outputs(Vec::new());

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Hardware,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("hardware preference must not fall back to another backend");

    assert_eq!(error, VideoPipelineSelectionError::MissingHardwareOutput);
}

#[test]
fn hardware_preference_rejects_software_only_stream() {
    let capabilities = capabilities_with_playable_outputs(vec![ffmpeg_host_upload_output()]);

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Hardware,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("hardware preference must never fall back to software");

    assert_eq!(error, VideoPipelineSelectionError::MissingHardwareOutput);
}

#[test]
fn unstartable_native_hardware_backend_is_typed_requested_backend_error() {
    let capabilities = capabilities_with_playable_outputs(vec![non_vaapi_dma_buf_output()]);

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Hardware,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("selector must not pretend future hardware backend is startable");

    assert_eq!(
        error,
        VideoPipelineSelectionError::UnsupportedCompositionBackend {
            preference: "hardware",
            backend_id: "future_backend".to_string(),
        }
    );
}

#[test]
fn auto_does_not_fall_back_to_software_when_unstartable_hardware_is_playable() {
    let capabilities = capabilities_with_playable_outputs(vec![non_vaapi_dma_buf_output()]);

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("auto can use software only when no playable hardware output exists");

    assert_eq!(
        error,
        VideoPipelineSelectionError::UnsupportedCompositionBackend {
            preference: "auto",
            backend_id: "future_backend".to_string(),
        }
    );
}

#[test]
fn hardware_preference_rejects_native_hardware_without_dma_buf_transfer() {
    let capabilities = capabilities_with_playable_outputs(vec![host_upload_vaapi_output()]);

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Hardware,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("hardware startup requires current DMA-BUF materializer path");

    assert_eq!(error, VideoPipelineSelectionError::MissingHardwareOutput);
}

#[test]
fn software_preference_selects_ffmpeg_when_ffmpeg_is_playable() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let capabilities = capabilities_with_playable_outputs(vec![
        current_vaapi_output(),
        ffmpeg_host_upload_output(),
    ]);

    let plan = select_video_pipeline_plan(
        VideoBackendPreference::Software,
        Some(&capabilities),
        decoder_thread_config,
        None,
    )
    .expect("software preference should start only FFmpeg software path");

    assert_eq!(
        plan,
        VideoPipelinePlan::FfmpegHostUpload {
            decoder_thread_config,
        }
    );
}

#[test]
fn software_preference_rejects_when_ffmpeg_unavailable() {
    let capabilities = capabilities_with_playable_outputs(vec![current_vaapi_output()]);

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Software,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("software preference must not silently use current VA-API path");

    assert_eq!(error, VideoPipelineSelectionError::MissingSoftwareOutput);
}

#[test]
fn software_preference_rejects_unstartable_software_backend() {
    let capabilities = capabilities_with_playable_outputs(vec![future_software_output()]);

    let error = select_video_pipeline_plan(
        VideoBackendPreference::Software,
        Some(&capabilities),
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("software preference must not treat a future backend as FFmpeg");

    assert_eq!(
        error,
        VideoPipelineSelectionError::UnsupportedCompositionBackend {
            preference: "software",
            backend_id: "future_sw".to_string(),
        }
    );
}

#[test]
fn missing_capability_snapshot_is_typed_error() {
    let error = select_video_pipeline_plan(
        VideoBackendPreference::Auto,
        None,
        PlayerVideoDecoderThreadConfig::default(),
        None,
    )
    .expect_err("selector cannot run before shell capability probe");

    assert_eq!(error, VideoPipelineSelectionError::MissingCapabilities);
}

/// Метки плана для wgpu — байт-в-байт прежние: по `plan="ffmpeg-host-upload-wgpu"` инструменты
/// замера (`docs/benchmarks/tools/local/evidence.py`, `t480s/collect.py`) классифицируют прогоны.
/// Метки GLES2 отличаются, чтобы прогоны двух рендеров не смешались (сессия 02 gles2-renderer).
#[test]
fn plan_labels_combine_decode_route_and_renderer_family() {
    use render_shell_api::RendererFamily;

    let decoder_thread_config = PlayerVideoDecoderThreadConfig::default();
    let hardware = VideoPipelinePlan::VaapiDmaBuf {
        decoder_thread_config,
    };
    let software = VideoPipelinePlan::FfmpegHostUpload {
        decoder_thread_config,
    };

    assert_eq!(
        hardware.diagnostic_label(RendererFamily::WgpuVulkan),
        "vaapi-dmabuf-wgpu"
    );
    assert_eq!(
        software.diagnostic_label(RendererFamily::WgpuVulkan),
        "ffmpeg-host-upload-wgpu"
    );
    assert_eq!(
        hardware.diagnostic_label(RendererFamily::Gles2),
        "vaapi-dmabuf-gles2"
    );
    assert_eq!(
        software.diagnostic_label(RendererFamily::Gles2),
        "ffmpeg-host-upload-gles2"
    );
}

/// Маршрут декода превращается в план импорта рендера: путь передачи и размер пула декодера
/// (предел кэша импорта DMA-BUF) берутся из плана, а не задаются рендером.
#[test]
fn import_plan_carries_transfer_path_and_decoder_surface_pool() {
    use render_shell_api::VideoImportPath;

    let decoder_thread_config = PlayerVideoDecoderThreadConfig {
        zero_copy_surface_pool_slots: 17,
        ..PlayerVideoDecoderThreadConfig::default()
    };
    let hardware = VideoPipelinePlan::VaapiDmaBuf {
        decoder_thread_config,
    }
    .import_plan();
    let software = VideoPipelinePlan::FfmpegHostUpload {
        decoder_thread_config,
    }
    .import_plan();

    assert_eq!(hardware.path, VideoImportPath::HardwareZeroCopy);
    assert_eq!(hardware.decoder_surface_pool.get(), 17);
    assert_eq!(software.path, VideoImportPath::SoftwareHostUpload);
    assert_eq!(software.decoder_surface_pool.get(), 17);
    assert_eq!(
        VideoPipelinePlan::FfmpegHostUpload {
            decoder_thread_config
        }
        .decoder_thread_config(),
        decoder_thread_config
    );
}

/// Путь GLES2 целиком: default config («авто» = 0) -> worker config с пулом GLES2 -> план
/// импорта рендера получает 16 слотов (а не 0 и не прежние 24); wgpu остаётся на 24.
#[test]
fn automatic_surface_pool_reaches_import_plan_per_renderer_family() {
    use render_shell_api::RendererFamily;

    let import_slots_for = |family: RendererFamily| {
        let decoder_thread_config = player_core::PlayerWorkerConfig::from_app_config(
            &fastiplayer_config::AppConfig::default(),
            crate::video_pipeline_candidate::hardware_surface_pool_default_or_wgpu(Some(family)),
        )
        .decoder_thread_config;
        VideoPipelinePlan::VaapiDmaBuf {
            decoder_thread_config,
        }
        .import_plan()
        .decoder_surface_pool
        .get()
    };

    assert_eq!(import_slots_for(RendererFamily::Gles2), 16);
    assert_eq!(import_slots_for(RendererFamily::WgpuVulkan), 24);
}
