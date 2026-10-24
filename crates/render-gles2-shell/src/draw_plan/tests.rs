use codec_core::{ColorPrimaries, TransferFunction};
use render_core::{ColorAdjustment as RenderColorAdjustment, PictureSize};
use video_frame_contract::VideoFramePixelLayout;

use super::*;
use crate::frame_metadata::renderable_frame;
use crate::test_support::host_frame;

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
        &ColorPipelineSettings::identity(),
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
        &ColorPipelineSettings::identity(),
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
    let plan = VideoDrawPlan::for_frame(&frame, oversized, &ColorPipelineSettings::identity())
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
            &ColorPipelineSettings::identity(),
        )
        .expect("plan");
        assert_eq!(
            plan.request().color,
            YuvColorEncoding { matrix, range },
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
    let plan = VideoDrawPlan::for_frame(&frame, placement((16, 16), &[]), &settings).expect("plan");
    assert_eq!(
        plan.request().adjustment,
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
            &ColorPipelineSettings::identity()
        )
        .err(),
        Some(VideoDrawPlanError::InvalidDisplaySize {
            width: 16,
            height: 0
        })
    );
}
