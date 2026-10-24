use codec_core::{ColorPrimaries, TransferFunction};
use render_core::{ColorAdjustment as RenderColorAdjustment, PictureSize};
use video_frame_contract::VideoFramePixelLayout;

use super::*;
use crate::frame_metadata::renderable_frame;
use crate::high_bit_color::HighBitColorError;
use crate::test_support::{high_bit_host_frame, host_frame};

fn frame_with(
    size: [u32; 2],
    orientation: VideoDisplayOrientation,
    color: VideoColorMetadata,
) -> RenderableFrame {
    let mut test_frame = host_frame(
        VideoFramePixelLayout::Yuv420Planar8,
        size,
        0,
        [16, 128, 128],
    );
    test_frame.frame.display_orientation = orientation;
    test_frame.frame.color = color;
    renderable_frame(&test_frame.frame).expect("YUV frame")
}

fn sdr(range: ColorRange, matrix: MatrixCoefficients) -> VideoColorMetadata {
    VideoColorMetadata {
        range,
        matrix,
        ..VideoColorMetadata::sdr_bt709_limited()
    }
}

/// Живые настройки с цветом `color_pipeline` и HDR → SDR по умолчанию.
fn live(color_pipeline: ColorPipelineSettings) -> RenderLiveSettings {
    RenderLiveSettings {
        color_pipeline,
        ..RenderLiveSettings::default()
    }
}

fn placement(surface: (u32, u32), exclusions: &[RenderViewport]) -> VideoPlacement<'_> {
    VideoPlacement {
        surface_size: surface,
        video_viewport: RenderViewport::full_surface(surface.0, surface.1),
        exclusions,
    }
}

#[test]
fn letterbox_comes_from_render_core_picture_layout() {
    let frame = frame_with(
        [200, 100],
        VideoDisplayOrientation::Identity,
        sdr(ColorRange::Limited, MatrixCoefficients::Bt709),
    );
    let plan = VideoDrawPlan::for_frame(
        &frame,
        placement((100, 100), &[]),
        &live(ColorPipelineSettings::identity()),
    )
    .expect("plan");
    let expected = PictureLayout::fit(
        PictureSize::new(200, 100),
        RenderViewport::new(0, 0, 100, 100),
    )
    .expect("layout")
    .uv_scale_and_offset();
    let request = plan.request();
    assert_eq!(
        (request.picture.uv_scale, request.picture.uv_offset),
        expected
    );
    assert_eq!(request.picture.uv_scale, [1.0, 2.0]);
}

#[test]
fn rotated_frame_is_fitted_by_its_oriented_size() {
    let frame = frame_with(
        [200, 100],
        VideoDisplayOrientation::Rotate90Clockwise,
        sdr(ColorRange::Limited, MatrixCoefficients::Bt709),
    );
    let plan = VideoDrawPlan::for_frame(
        &frame,
        placement((100, 100), &[]),
        &live(ColorPipelineSettings::identity()),
    )
    .expect("plan");
    let request = plan.request();
    // Повёрнутая картинка 100×200 в квадрате: полосы слева и справа.
    assert_eq!(request.picture.uv_scale, [2.0, 1.0]);
    assert_eq!(request.orientation, QuarterTurn::Clockwise90);
}

#[test]
fn exclusions_become_visible_rects_of_the_clamped_viewport() {
    let frame = frame_with(
        [64, 36],
        VideoDisplayOrientation::Identity,
        sdr(ColorRange::Limited, MatrixCoefficients::Bt709),
    );
    let sidebar = RenderViewport::new(0, 0, 30, 90);
    let oversized = VideoPlacement {
        surface_size: (160, 90),
        video_viewport: RenderViewport::new(0, 0, 400, 400),
        exclusions: &[sidebar],
    };
    let plan =
        VideoDrawPlan::for_frame(&frame, oversized, &live(ColorPipelineSettings::identity()))
            .expect("plan");
    let request = plan.request();
    assert_eq!(
        request.viewport,
        PixelRect {
            x: 0,
            y: 0,
            width: 160,
            height: 90
        }
    );
    assert_eq!(
        request.visible_rects,
        [PixelRect {
            x: 30,
            y: 0,
            width: 130,
            height: 90
        }]
    );
}

#[test]
fn colour_encoding_follows_the_wgpu_fallback_rules() {
    let hdr = VideoColorMetadata {
        transfer: TransferFunction::Pq,
        primaries: ColorPrimaries::Bt2020,
        matrix: MatrixCoefficients::Bt2020,
        ..VideoColorMetadata::sdr_bt709_limited()
    };
    let cases = [
        // SDR без fallback — как есть.
        (
            sdr(ColorRange::Full, MatrixCoefficients::Bt601),
            YuvMatrix::Bt601,
            YuvRange::Full,
        ),
        (
            sdr(ColorRange::Limited, MatrixCoefficients::Bt601),
            YuvMatrix::Bt601,
            YuvRange::Limited,
        ),
        (
            sdr(ColorRange::Full, MatrixCoefficients::Bt709),
            YuvMatrix::Bt709,
            YuvRange::Full,
        ),
        // Неизвестная матрица — fallback: SDR BT.709 limited, даже при full-диапазоне.
        (
            sdr(ColorRange::Full, MatrixCoefficients::Unknown),
            YuvMatrix::Bt709,
            YuvRange::Limited,
        ),
        // Широкий охват и HDR на 8-битном SDR-пути — тоже BT.709 limited.
        (
            sdr(ColorRange::Full, MatrixCoefficients::Bt2020),
            YuvMatrix::Bt709,
            YuvRange::Limited,
        ),
        (hdr, YuvMatrix::Bt709, YuvRange::Limited),
    ];
    for (color, matrix, range) in cases {
        let frame = frame_with([16, 16], VideoDisplayOrientation::Identity, color.clone());
        let plan = VideoDrawPlan::for_frame(
            &frame,
            placement((16, 16), &[]),
            &live(ColorPipelineSettings::identity()),
        )
        .expect("plan");
        assert_eq!(
            plan.request().color,
            VideoColorConversion::Sdr {
                encoding: YuvColorEncoding { matrix, range },
                adjustment: ColorAdjustment::IDENTITY,
            },
            "{color:?}"
        );
    }
}

#[test]
fn colour_adjustment_is_passed_through() {
    let frame = frame_with(
        [16, 16],
        VideoDisplayOrientation::Identity,
        sdr(ColorRange::Limited, MatrixCoefficients::Bt709),
    );
    let mut settings = ColorPipelineSettings::identity();
    settings.adjustment = RenderColorAdjustment {
        brightness: 0.1,
        contrast: 1.2,
        saturation: 0.7,
        exposure: -0.5,
        rgb_gain: [1.1, 1.0, 0.9],
        rgb_offset: [0.0, 0.02, -0.01],
    };
    let plan =
        VideoDrawPlan::for_frame(&frame, placement((16, 16), &[]), &live(settings)).expect("plan");
    let VideoColorConversion::Sdr { adjustment, .. } = plan.request().color else {
        panic!("8-bit SDR frame draws with the SDR conversion");
    };
    assert_eq!(
        adjustment,
        ColorAdjustment {
            brightness: 0.1,
            contrast: 1.2,
            saturation: 0.7,
            exposure: -0.5,
            rgb_gain: [1.1, 1.0, 0.9],
            rgb_offset: [0.0, 0.02, -0.01],
        }
    );
}

#[test]
fn zero_display_size_is_an_error_not_a_black_frame() {
    let mut frame = frame_with(
        [16, 16],
        VideoDisplayOrientation::Identity,
        sdr(ColorRange::Limited, MatrixCoefficients::Bt709),
    );
    frame.render_height = 0;
    assert_eq!(
        VideoDrawPlan::for_frame(
            &frame,
            placement((16, 16), &[]),
            &live(ColorPipelineSettings::identity())
        )
        .err(),
        Some(VideoDrawPlanError::InvalidDisplaySize {
            width: 16,
            height: 0
        })
    );
}

fn high_bit_frame(layout: VideoFramePixelLayout, color: VideoColorMetadata) -> RenderableFrame {
    let mut test_frame = high_bit_host_frame(layout, [16, 16], 0, [500, 512, 512]);
    test_frame.frame.color = color;
    renderable_frame(&test_frame.frame).expect("YUV frame")
}

fn pq_bt2020(range: ColorRange) -> VideoColorMetadata {
    VideoColorMetadata {
        range,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        ..VideoColorMetadata::sdr_bt709_limited()
    }
}

fn knobs() -> ColorPipelineSettings {
    let mut settings = ColorPipelineSettings::identity();
    settings.adjustment.saturation = 1.35;
    settings.adjustment.brightness = 0.1;
    settings
}

/// 10/12 бит SDR: ручки цветокоррекции не передаются — wgpu не применяет их на 16-битном пути.
#[test]
fn high_bit_sdr_ignores_colour_knobs_like_wgpu() {
    let frame = high_bit_frame(
        VideoFramePixelLayout::Yuv422Planar12Le,
        sdr(ColorRange::Full, MatrixCoefficients::Bt709),
    );
    let plan =
        VideoDrawPlan::for_frame(&frame, placement((16, 16), &[]), &live(knobs())).expect("plan");
    assert_eq!(
        plan.request().color,
        VideoColorConversion::Sdr {
            encoding: YuvColorEncoding {
                matrix: YuvMatrix::Bt709,
                range: YuvRange::Full
            },
            adjustment: ColorAdjustment::IDENTITY,
        }
    );
}

/// 10 бит PQ: HDR → SDR с уровнями из живых настроек; цветовой путь диагностики — с HDR-to-SDR.
#[test]
fn high_bit_hdr_uses_live_hdr_to_sdr_settings() {
    let frame = high_bit_frame(
        VideoFramePixelLayout::Yuv420Planar10Le,
        pq_bt2020(ColorRange::Limited),
    );
    let mut settings = live(knobs());
    settings.hdr_to_sdr.sdr_reference_white_nits = 150.0;
    let plan = VideoDrawPlan::for_frame(&frame, placement((16, 16), &[]), &settings).expect("plan");
    let VideoColorConversion::HdrToSdr(conversion) = plan.request().color else {
        panic!(
            "HDR frame draws with HDR-to-SDR: {:?}",
            plan.request().color
        );
    };
    assert_eq!(conversion.transfer, gles2_video::HdrTransfer::Pq);
    assert_eq!(conversion.range, YuvRange::Limited);
    assert_eq!(conversion.levels.sdr_white_nits(), 150.0);
    assert_eq!(
        plan.active_color_path().hdr_to_sdr,
        Some(settings.hdr_to_sdr),
        "diagnostics name the HDR-to-SDR path"
    );
}

/// HDR без включённого HDR → SDR и BT.2020 SDR 10 бит — ошибки плана (у wgpu — ошибки рендера),
/// а не тихая подмена цвета.
#[test]
fn high_bit_frames_outside_the_wgpu_path_are_plan_errors() {
    let mut disabled = live(ColorPipelineSettings::identity());
    disabled.hdr_to_sdr.enabled = false;
    let hdr_frame = high_bit_frame(
        VideoFramePixelLayout::Yuv420Planar10Le,
        pq_bt2020(ColorRange::Full),
    );
    assert_eq!(
        VideoDrawPlan::for_frame(&hdr_frame, placement((16, 16), &[]), &disabled).err(),
        Some(VideoDrawPlanError::HighBitColor(
            HighBitColorError::HdrToSdrUnavailable
        ))
    );
    let wide_sdr = high_bit_frame(
        VideoFramePixelLayout::Yuv444Planar10Le,
        sdr(ColorRange::Limited, MatrixCoefficients::Bt2020),
    );
    assert!(matches!(
        VideoDrawPlan::for_frame(
            &wide_sdr,
            placement((16, 16), &[]),
            &live(ColorPipelineSettings::identity())
        ),
        Err(VideoDrawPlanError::HighBitColor(
            HighBitColorError::UnsupportedColor { .. }
        ))
    ));
}
