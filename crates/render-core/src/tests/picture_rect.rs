use std::time::Duration;

use codec_core::{BitDepth, ChromaSubsampling, VideoDisplayOrientation};
use video_frame_contract::VideoFramePixelLayout;

use super::*;
use crate::RenderColorMetadata;

fn frame(width: u32, height: u32, orientation: VideoDisplayOrientation) -> RenderableFrame {
    RenderableFrame {
        handle: 1,
        pts: Duration::ZERO,
        format: VideoFramePixelLayout::Nv12,
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        coded_width: width,
        coded_height: height,
        render_width: width,
        render_height: height,
        display_orientation: orientation,
        color: RenderColorMetadata::sdr_bt709_limited(),
    }
}

fn fit(picture: (u32, u32), viewport: RenderViewport) -> PictureLayout {
    PictureLayout::fit(PictureSize::new(picture.0, picture.1), viewport)
        .expect("невырожденный fit должен существовать")
}

fn assert_rect_close(actual: PictureRect, expected: [f32; 4]) {
    let actual = [actual.x, actual.y, actual.width, actual.height];
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 0.01, "{actual:?} != {expected:?}");
    }
}

/// Эталон: старая формула `letterbox_scale_and_offset`, переписанная буквально.
fn legacy_formula(picture: (u32, u32), viewport: (u32, u32)) -> ([f32; 2], [f32; 2]) {
    let video_aspect = picture.0 as f32 / picture.1.max(1) as f32;
    let viewport_aspect = viewport.0 as f32 / viewport.1.max(1) as f32;
    if video_aspect > viewport_aspect {
        let scale_y = video_aspect / viewport_aspect;
        ([1.0, scale_y], [0.0, (1.0 - scale_y) * 0.5])
    } else {
        let scale_x = viewport_aspect / video_aspect;
        ([scale_x, 1.0], [(1.0 - scale_x) * 0.5, 0.0])
    }
}

#[test]
fn wide_picture_gets_bars_top_and_bottom() {
    // 2:1 в 1600x1200 (4:3): высота картинки 800, полосы по 200.
    let rect = fit((2000, 1000), RenderViewport::new(0, 0, 1600, 1200)).rect();

    assert_eq!(
        rect,
        PictureRect {
            x: 0.0,
            y: 200.0,
            width: 1600.0,
            height: 800.0
        }
    );
}

#[test]
fn cinemascope_in_16_9_viewport_has_letterbox() {
    let rect = fit((2390, 1000), RenderViewport::new(0, 0, 1920, 1080)).rect();

    assert_eq!(rect.x, 0.0);
    assert_eq!(rect.width, 1920.0);
    assert!((rect.height - 1920.0 / 2.39).abs() < 0.01, "{rect:?}");
    // Полосы симметричны.
    assert!((rect.y - (1080.0 - rect.height) * 0.5).abs() < 0.01);
}

#[test]
fn four_by_three_in_16_9_is_pillarboxed() {
    let rect = fit((640, 480), RenderViewport::new(0, 0, 1920, 1080)).rect();

    assert_rect_close(rect, [240.0, 0.0, 1440.0, 1080.0]);
}

#[test]
fn exact_fit_covers_whole_viewport() {
    let layout = fit((1920, 1080), RenderViewport::new(0, 0, 1280, 720));

    assert_eq!(
        layout.rect(),
        PictureRect {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0
        }
    );
    assert_eq!(layout.uv_scale_and_offset(), ([1.0, 1.0], [0.0, 0.0]));
}

#[test]
fn viewport_origin_shifts_rect() {
    let rect = fit((640, 480), RenderViewport::new(100, 50, 1920, 1080)).rect();

    assert_rect_close(rect, [340.0, 50.0, 1440.0, 1080.0]);
}

#[test]
fn quarter_turn_orientation_swaps_axes() {
    let viewport = RenderViewport::new(0, 0, 1000, 1000);
    // Кадр 1920x1080, повёрнутый на 90: картинка 1080x1920 (портрет).
    let rotated = PictureLayout::fit_frame(
        &frame(1920, 1080, VideoDisplayOrientation::Rotate90Clockwise),
        viewport,
    )
    .expect("fit")
    .rect();
    let upright = PictureLayout::fit_frame(
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        viewport,
    )
    .expect("fit")
    .rect();

    assert_eq!(rotated.height, 1000.0);
    assert!(rotated.width < 1000.0);
    assert_eq!(upright.width, 1000.0);
    assert!(upright.height < 1000.0);
}

#[test]
fn degenerate_sizes_yield_no_layout() {
    let good = RenderViewport::new(0, 0, 100, 100);
    let empty = RenderViewport::new(0, 0, 0, 100);

    assert_eq!(PictureLayout::fit(PictureSize::new(0, 10), good), None);
    assert_eq!(PictureLayout::fit(PictureSize::new(10, 0), good), None);
    assert_eq!(PictureLayout::fit(PictureSize::new(10, 10), empty), None);
    assert_eq!(
        PictureLayout::fit(PictureSize::new(10, 10), RenderViewport::new(0, 0, 100, 0)),
        None
    );
}

#[test]
fn uv_scale_and_offset_match_legacy_formula_bit_for_bit() {
    let sizes = [
        1_u32, 2, 3, 240, 480, 640, 720, 1080, 1280, 1920, 2390, 3840,
    ];
    for &picture_width in &sizes {
        for &picture_height in &sizes {
            for &viewport_width in &sizes {
                for &viewport_height in &sizes {
                    let layout = fit(
                        (picture_width, picture_height),
                        RenderViewport::new(7, 9, viewport_width, viewport_height),
                    );
                    let expected = legacy_formula(
                        (picture_width, picture_height),
                        (viewport_width, viewport_height),
                    );
                    assert_eq!(
                        layout.uv_scale_and_offset(),
                        expected,
                        "{picture_width}x{picture_height} in {viewport_width}x{viewport_height}"
                    );
                }
            }
        }
    }
}
