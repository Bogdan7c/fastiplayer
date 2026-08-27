use std::time::Duration;

use codec_core::{
    ColorMetadataConfidence, ColorMetadataOrigin, ColorPrimaries, ColorRange, HdrMetadata,
    MatrixCoefficients, TransferFunction, VideoColorMetadata,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::*;

#[test]
fn p010_frame_dispatches_to_p010_renderer_path() {
    let dispatch = select_renderer_dispatch(VideoFramePixelLayout::P010, WgpuFramePlaneKind::P010)
        .expect("P010 frame dispatches");

    assert_eq!(dispatch, RendererDispatch::P010);
}

#[test]
fn nv12_frame_dispatches_to_nv12_renderer_path() {
    let dispatch = select_renderer_dispatch(VideoFramePixelLayout::Nv12, WgpuFramePlaneKind::Nv12)
        .expect("NV12 frame dispatches");

    assert_eq!(dispatch, RendererDispatch::Nv12);
}

#[test]
fn host_planar_yuv_frame_dispatches_to_host_planar_yuv_renderer_path() {
    for format in [
        VideoFramePixelLayout::Yuv420Planar8,
        VideoFramePixelLayout::Yuv420Planar10Le,
        VideoFramePixelLayout::Yuv420Planar12Le,
        VideoFramePixelLayout::Yuv422Planar8,
        VideoFramePixelLayout::Yuv422Planar10Le,
        VideoFramePixelLayout::Yuv422Planar12Le,
        VideoFramePixelLayout::Yuv444Planar8,
        VideoFramePixelLayout::Yuv444Planar10Le,
    ] {
        let dispatch = select_renderer_dispatch(format, WgpuFramePlaneKind::HostYuvPlanar)
            .expect("HostPlanar YUV frame dispatches");

        assert_eq!(dispatch, RendererDispatch::HostYuvPlanar);
    }
}

#[test]
fn renderable_metadata_tracks_sdr_hdr_across_hardware_and_software_paths() {
    let mut hardware_hdr = decoded_p010_test_frame();
    hardware_hdr.color = hdr_bt2020_pq_color_for_transition_test();
    let mut software_hdr = decoded_host_planar10_test_frame();
    software_hdr.color = hdr_bt2020_pq_color_for_transition_test();
    let cases = [
        (
            decoded_nv12_test_frame(),
            VideoFramePixelLayout::Nv12,
            false,
        ),
        (hardware_hdr, VideoFramePixelLayout::P010, true),
        (
            decoded_host_planar8_test_frame(),
            VideoFramePixelLayout::Yuv420Planar8,
            false,
        ),
        (software_hdr, VideoFramePixelLayout::Yuv420Planar10Le, true),
    ];

    for (decoded_frame, pixel_layout, expected_hdr) in cases {
        let metadata = renderable_metadata_from_decoded(&decoded_frame, pixel_layout);

        assert_eq!(metadata.color, decoded_frame.color);
        assert_eq!(metadata.color.requires_hdr_processing(), expected_hdr);
    }
}

#[test]
fn live_settings_adapter_accepts_current_color_and_hdr_fields() {
    let baseline = RenderLiveSettings::default();
    let mut settings = baseline.clone();

    settings.color_pipeline.adjustment.contrast = 1.1;
    settings.hdr_to_sdr.hdr_reference_peak_nits = 900.0;

    let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
    let unsupported_fields =
        unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

    assert!(unsupported_fields.is_empty());
}

#[test]
fn live_settings_adapter_rejects_unimplemented_shader_parameters() {
    let baseline = RenderLiveSettings::default();
    let mut settings = baseline.clone();
    let shader_parameter_id = render_core::ShaderParameterId::new("render.shader.test_gain");

    settings
        .shader_parameters
        .parameters
        .push(render_core::ShaderParameter::new(
            shader_parameter_id.clone(),
            render_core::ShaderParameterValue::Float(0.5),
        ));

    let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
    let unsupported_fields =
        unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

    assert_eq!(
        unsupported_fields,
        vec![RenderLiveSettingId::ShaderParameter(shader_parameter_id)]
    );
}

#[test]
fn live_settings_adapter_rejects_future_color_pipeline_modes() {
    let baseline = RenderLiveSettings::default();
    let mut settings = baseline.clone();

    settings.color_pipeline.tone_mapping = ToneMappingMode::Reinhard;
    settings.color_pipeline.swapchain_transfer = SwapchainTransferMode::SrgbRenderTarget;

    let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
    let unsupported_fields =
        unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

    assert_eq!(
        unsupported_fields,
        vec![
            RenderLiveSettingId::ColorPipelineToneMapping,
            RenderLiveSettingId::ColorPipelineSwapchainTransfer,
        ]
    );
}

#[test]
fn live_settings_adapter_rejects_hdr_to_sdr_values_outside_phase10_contract() {
    let baseline = RenderLiveSettings::default();
    let mut settings = baseline.clone();

    settings.hdr_to_sdr.enabled = false;

    let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
    let unsupported_fields =
        unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

    assert_eq!(
        unsupported_fields,
        vec![RenderLiveSettingId::HdrToSdrEnabled]
    );
}

#[test]
fn metadata_plane_mismatch_is_rejected_before_renderer_call() {
    let error = select_renderer_dispatch(VideoFramePixelLayout::P010, WgpuFramePlaneKind::Nv12)
        .expect_err("P010 metadata must not use NV12 planes");

    assert!(
        error.to_string().contains("metadata/plane mismatch"),
        "unexpected error: {error}"
    );
}

#[test]
fn p010_boundary_rejects_non_zero_copy_memory_path() {
    let frame = decoded_host_planar10_test_frame();

    let error = validate_decoded_p010_frame(&frame).expect_err("P010 host-upload path rejected");

    assert!(
        error.to_string().contains("zero-copy"),
        "unexpected error: {error}"
    );
}

#[test]
fn host_planar_yuv_boundary_accepts_ready_host_upload_contracts() {
    for frame in [
        decoded_host_planar8_test_frame(),
        decoded_host_planar10_test_frame(),
        decoded_host_planar12_test_frame(),
        decoded_host_planar422_8_test_frame(),
        decoded_host_planar422_10_test_frame(),
        decoded_host_planar422_12_test_frame(),
        decoded_host_planar444_8_test_frame(),
        decoded_host_planar444_10_test_frame(),
    ] {
        validate_decoded_host_yuv_frame(&frame)
            .expect("HostPlanar YUV software upload boundary accepts frame");
    }
}

#[test]
fn host_planar_yuv_boundary_rejects_dma_buf_contract() {
    let frame = decoded_nv12_test_frame();

    let error =
        validate_decoded_host_yuv_frame(&frame).expect_err("HostPlanar YUV rejects DMA-BUF frame");

    assert!(
        error.to_string().contains("software host-upload"),
        "unexpected error: {error}"
    );
}

#[test]
fn nv12_boundary_rejects_non_zero_copy_memory_path() {
    let frame = decoded_host_planar8_test_frame();

    let error = validate_decoded_nv12_frame(&frame).expect_err("NV12 host-upload path rejected");

    assert!(
        error.to_string().contains("zero-copy"),
        "unexpected error: {error}"
    );
}

#[test]
fn dma_buf_import_failure_is_renderer_error_without_cpu_fallback() {
    let lookup = texture_view_lookup_after_import_failure(
        FrameResourceHandle(77),
        anyhow::anyhow!("synthetic import failure"),
        Duration::from_millis(9),
    );

    assert!(matches!(
        lookup,
        WgpuFrameTextureViewLookup::Error {
            texture_pool_lock_wait
        } if texture_pool_lock_wait == Duration::from_millis(9)
    ));
}

#[test]
fn dma_buf_materializer_returns_unsupported_for_host_planar_descriptor() {
    let descriptor = FrameResourceDescriptor::HostPlanar(
        video_core::HostPlanarFrameDescriptor::from_owned_buffer(
            vec![0_u8; 6],
            vec![
                video_core::HostPlaneDescriptor {
                    role: video_core::HostPlaneRole::Luma,
                    offset: 0,
                    stride: 2,
                    visible_width: 2,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
                video_core::HostPlaneDescriptor {
                    role: video_core::HostPlaneRole::ChromaU,
                    offset: 4,
                    stride: 1,
                    visible_width: 1,
                    visible_height: 1,
                    bytes_per_sample: 1,
                },
                video_core::HostPlaneDescriptor {
                    role: video_core::HostPlaneRole::ChromaV,
                    offset: 5,
                    stride: 1,
                    visible_width: 1,
                    visible_height: 1,
                    bytes_per_sample: 1,
                },
            ],
        ),
    );

    let lookup =
        unsupported_lookup_for_non_dma_buf_descriptor(&descriptor, Duration::from_millis(4))
            .expect("host-planar descriptor must be classified before DMA-BUF import");

    assert!(matches!(
        lookup,
        WgpuFrameTextureViewLookup::Unsupported {
            reason:
                WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresUploadMaterializer,
            texture_pool_lock_wait,
        } if texture_pool_lock_wait == Duration::from_millis(4)
    ));
}

#[test]
fn p010_dma_buf_image_layouts_map_to_same_renderer_plane_kind() {
    let baseline_separate_layer_kind = p010_storage_layout_renderer_plane_kind(
        video_frame_contract::DmaBufImageLayout::SeparateLayers,
    );
    let compatibility_composed_kind = p010_storage_layout_renderer_plane_kind(
        video_frame_contract::DmaBufImageLayout::ComposedLayers,
    );

    assert_eq!(baseline_separate_layer_kind, WgpuFramePlaneKind::P010);
    assert_eq!(compatibility_composed_kind, WgpuFramePlaneKind::P010);
    assert_eq!(baseline_separate_layer_kind, compatibility_composed_kind);
}

/// Документирует, что renderer видит только P010 Y/UV pair, а не storage layout.
const fn p010_storage_layout_renderer_plane_kind(
    _storage_layout: video_frame_contract::DmaBufImageLayout,
) -> WgpuFramePlaneKind {
    WgpuFramePlaneKind::P010
}

#[test]
fn visible_video_draw_rects_exclude_sidebar_without_changing_viewport_size() {
    let video_viewport = RenderViewport::full_surface(1280, 720);
    let sidebar = RenderViewport::new(0, 64, 420, 576);

    let draw_rects = visible_video_draw_rects(video_viewport, &[sidebar]);

    assert_eq!(
        draw_rects,
        vec![
            RenderViewport::new(0, 0, 1280, 64),
            RenderViewport::new(0, 640, 1280, 80),
            RenderViewport::new(420, 64, 860, 576),
        ]
    );
    assert_eq!(video_viewport.size(), (1280, 720));
}

#[test]
fn visible_video_draw_rects_keep_full_viewport_without_exclusions() {
    let video_viewport = RenderViewport::full_surface(1280, 720);

    assert_eq!(
        visible_video_draw_rects(video_viewport, &[]),
        vec![video_viewport]
    );
}

#[test]
fn overlay_video_target_load_preserves_existing_surface_contents() {
    assert!(matches!(
        VideoRenderTargetLoad::LoadExisting.as_wgpu_load_op(),
        wgpu::LoadOp::Load
    ));
    assert!(matches!(
        VideoRenderTargetLoad::ClearBlack.as_wgpu_load_op(),
        wgpu::LoadOp::Clear(_)
    ));
}

#[test]
fn overlay_video_pass_uses_distinct_uniform_slot_from_main_pass() {
    // Регрессия: main и overlay pass писали letterbox в один uniform buffer,
    // и submission-time `write_buffer` overlay-я ломал пропорции main видео.
    assert_ne!(
        VideoRenderTargetLoad::ClearBlack.uniform_slot(),
        VideoRenderTargetLoad::LoadExisting.uniform_slot()
    );
    assert!(
        VideoRenderTargetLoad::ClearBlack.uniform_slot()
            < VideoRenderTargetLoad::UNIFORM_SLOT_COUNT
    );
    assert!(
        VideoRenderTargetLoad::LoadExisting.uniform_slot()
            < VideoRenderTargetLoad::UNIFORM_SLOT_COUNT
    );
}

/// Строит renderer-neutral frame с заданным render-размером для letterbox тестов.
fn renderable_test_frame(render_width: u32, render_height: u32) -> RenderableFrame {
    let mut decoded = decoded_nv12_test_frame();
    decoded.render_width = render_width;
    decoded.render_height = render_height;
    renderable_metadata_from_decoded(&decoded, VideoFramePixelLayout::Nv12)
}

/// Долю оси, занятую видео, восстанавливаем из uv_scale: видимая полоса = 1 / scale.
fn visible_axis_fraction(scale: f32) -> f32 {
    1.0 / scale
}

#[test]
fn letterbox_wide_video_uses_viewport_size_for_top_bottom_bars() {
    // Видео 16:9 (1.778) в почти квадратном viewport-е (1.0): кадр шире области.
    let frame = renderable_test_frame(1920, 1080);
    let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1000, 1000));

    // По горизонтали кадр занимает весь viewport, по вертикали — сжимается с полосами.
    assert_eq!(uv_scale[0], 1.0);
    assert_eq!(uv_offset[0], 0.0);
    // Масштаб > 1 и отрицательное смещение => шейдер уводит края за [0, 1] и красит чёрным.
    assert!(
        uv_scale[1] > 1.0,
        "ожидали scale_y > 1, получили {}",
        uv_scale[1]
    );
    assert!(
        uv_offset[1] < 0.0,
        "ожидали offset_y < 0, получили {}",
        uv_offset[1]
    );
    // Видимая по вертикали доля = viewport_aspect / video_aspect = 1.0 / 1.778.
    let video_aspect = 1920.0_f32 / 1080.0;
    let viewport_aspect = 1.0_f32;
    assert!((visible_axis_fraction(uv_scale[1]) - viewport_aspect / video_aspect).abs() < 1e-4);
}

#[test]
fn letterbox_portrait_video_uses_wide_viewport_size_for_left_right_bars() {
    // Портретное видео 9:16 (0.5625) в широком viewport-е 16:9 (1.778).
    let frame = renderable_test_frame(1080, 1920);
    let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1920, 1080));

    // По вертикали кадр занимает весь viewport, по горизонтали — полосы слева и справа.
    assert_eq!(uv_scale[1], 1.0);
    assert_eq!(uv_offset[1], 0.0);
    assert!(
        uv_scale[0] > 1.0,
        "ожидали scale_x > 1, получили {}",
        uv_scale[0]
    );
    assert!(
        uv_offset[0] < 0.0,
        "ожидали offset_x < 0, получили {}",
        uv_offset[0]
    );
    let video_aspect = 1080.0_f32 / 1920.0;
    let viewport_aspect = 1920.0_f32 / 1080.0;
    assert!((visible_axis_fraction(uv_scale[0]) - video_aspect / viewport_aspect).abs() < 1e-4);
}

#[test]
fn letterbox_rotated_phone_video_uses_oriented_display_size_inside_viewport() {
    let mut frame = renderable_test_frame(3840, 2160);
    frame.display_orientation = VideoDisplayOrientation::Rotate270Clockwise;

    let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1920, 1080));

    assert_eq!(uv_scale[1], 1.0);
    assert_eq!(uv_offset[1], 0.0);
    assert!(
        uv_scale[0] > 1.0,
        "rotated portrait video should add left/right bars, got scale_x={}",
        uv_scale[0]
    );
}

#[test]
fn rotate270_clockwise_orientation_maps_display_uv_to_source_uv() {
    let transform = display_orientation_uv_transform(VideoDisplayOrientation::Rotate270Clockwise);

    assert_eq!(transform[0], [0.0, -1.0, 1.0, 0.0]);
    assert_eq!(transform[1], [1.0, 0.0, 0.0, 0.0]);
}

#[test]
fn letterbox_matching_aspect_is_identity() {
    // Кадр 16:9 во viewport-е 16:9: ни полос, ни масштабирования.
    let frame = renderable_test_frame(1920, 1080);
    let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1280, 720));

    assert!((uv_scale[0] - 1.0).abs() < 1e-4);
    assert!((uv_scale[1] - 1.0).abs() < 1e-4);
    assert!(uv_offset[0].abs() < 1e-4);
    assert!(uv_offset[1].abs() < 1e-4);
}

#[test]
fn letterbox_keeps_video_centered_in_clip_space() {
    // Центр экрана (uv = 0.5) должен отображаться в центр текстуры (0.5) на обеих осях.
    let frame = renderable_test_frame(1920, 1080);
    let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (800, 1200));

    let center_x = 0.5 * uv_scale[0] + uv_offset[0];
    let center_y = 0.5 * uv_scale[1] + uv_offset[1];
    assert!((center_x - 0.5).abs() < 1e-4, "center_x = {center_x}");
    assert!((center_y - 0.5).abs() < 1e-4, "center_y = {center_y}");
}

fn decoded_nv12_test_frame() -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: video_core::FrameResourceHandle(1),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

fn decoded_p010_test_frame() -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        width: 1920,
        height: 1080,
        render_width: 1920,
        render_height: 1080,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: video_core::FrameResourceHandle(7),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

fn decoded_host_planar8_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
        ..decoded_nv12_test_frame()
    }
}

fn decoded_host_planar10_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract::host_yuv420_planar10le(),
        ..decoded_p010_test_frame()
    }
}

fn hdr_bt2020_pq_color_for_transition_test() -> VideoColorMetadata {
    VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        hdr_metadata: Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt2020,
            transfer_function: TransferFunction::Pq,
            max_luminance_nits: Some(1_000.0),
            min_luminance_nits: Some(0.01),
            max_content_light_level_nits: Some(1_000),
            max_frame_average_light_level_nits: Some(400),
        }),
        origin: ColorMetadataOrigin::Bitstream,
        confidence: ColorMetadataConfidence::Confirmed,
    }
}

fn decoded_host_planar12_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract::host_yuv420_planar12le(),
        ..decoded_p010_test_frame()
    }
}

fn decoded_host_planar422_8_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        ..decoded_nv12_test_frame()
    }
}

fn decoded_host_planar422_10_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv422Planar10Le,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        ..decoded_p010_test_frame()
    }
}

fn decoded_host_planar422_12_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv422Planar12Le,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        ..decoded_p010_test_frame()
    }
}

fn decoded_host_planar444_8_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv444Planar8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        ..decoded_nv12_test_frame()
    }
}

fn decoded_host_planar444_10_test_frame() -> DecodedFrame {
    DecodedFrame {
        frame_contract: VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv444Planar10Le,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        ..decoded_p010_test_frame()
    }
}
