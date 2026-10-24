//! Маска скруглённых углов на настоящем EGL: каждый пиксель против формулы wgpu
//! (`window_corner_mask.wgsl`, полноэкранный проход), обе кодировки альфы, квадратные углы.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_egui::{
    CornerAlphaEncoding, CornerMaskOutcome, CornerMaskRenderer, CornerRadius, EguiPainterOptions,
    FrameCompositor, FrameStart, Gles2Context, OffscreenAlpha, PixelExtent, UiContent,
    UiFrameOutcome,
};
use support::{offscreen_or_skip, pixel, read, render, screen_for, size};

/// Фон тестов wgpu-маски: (0,25; 0,5; 0,75; 1) = [64, 128, 191, 255].
const BACKGROUND: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
const BACKGROUND_PIXEL: [u8; 4] = [64, 128, 191, 255];

/// Покрытие по формуле wgpu (полноэкранный SDF от центра цели), центр пикселя.
fn wgpu_coverage(x: u32, y: u32, target: [u32; 2], radius: f32) -> f32 {
    let half = [target[0] as f32 * 0.5, target[1] as f32 * 0.5];
    let p = [x as f32 + 0.5, y as f32 + 0.5];
    let q = [
        (p[0] - half[0]).abs() - (half[0] - radius),
        (p[1] - half[1]).abs() - (half[1] - radius),
    ];
    let distance = q[0].max(0.0).hypot(q[1].max(0.0)) + q[0].max(q[1]).min(0.0) - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

fn scale(channel: u8, coverage: f32) -> f32 {
    f32::from(channel) * coverage
}

fn masked_frame(
    context: &mut Gles2Context,
    compositor: &mut FrameCompositor,
    radius: CornerRadius,
    pixels_per_point: f32,
) -> CornerMaskOutcome {
    let screen = screen_for(context, pixels_per_point);
    match render(
        compositor,
        context,
        screen,
        UiContent::Fresh {
            primitives: Vec::new(),
            textures_delta: egui::TexturesDelta::default(),
        },
        BACKGROUND,
        radius,
    ) {
        UiFrameOutcome::Presented(frame) => frame.work.corner_mask,
        other => panic!("frame must be presented, got {other:?}"),
    }
}

/// Premultiplied-цель: каждый пиксель = фон × покрытие (rgb и a) с допуском 1 на канал.
/// Эталон считает весь экран, маска рисует 4 угла — совпадение доказывает, что пропуск
/// середины экрана ничего не меняет. Цель не квадратная, чтобы поймать перепутанные оси, и
/// с радиусом, упёртым в половину меньшей стороны.
#[test]
fn premultiplied_mask_matches_full_screen_wgpu_formula_on_every_pixel() {
    for (target, radius_points, pixels_per_point) in [
        ([64_u32, 64_u32], 12.0_f32, 1.0_f32),
        ([64, 40], 24.0, 2.0),
        ([97, 51], 7.3, 1.25),
    ] {
        let Some(mut context) = offscreen_or_skip(
            "premultiplied_mask_matches_full_screen_wgpu_formula_on_every_pixel",
            size(target[0], target[1]),
            OffscreenAlpha::PremultipliedLayer,
        ) else {
            return;
        };
        let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
        let outcome = masked_frame(
            &mut context,
            &mut compositor,
            CornerRadius::from_points(radius_points),
            pixels_per_point,
        );
        assert!(
            matches!(outcome, CornerMaskOutcome::Drawn { .. }),
            "{outcome:?}"
        );
        let radius = (radius_points * pixels_per_point).min(target[0].min(target[1]) as f32 * 0.5);
        let pixels = read(&context);
        let mut partially_covered = 0;
        for y in 0..target[1] {
            for x in 0..target[0] {
                let coverage = wgpu_coverage(x, y, target, radius);
                let expected = BACKGROUND_PIXEL.map(|channel| scale(channel, coverage));
                let actual = pixel(&pixels, x, y);
                for channel in 0..4 {
                    let difference = (f32::from(actual[channel]) - expected[channel]).abs();
                    assert!(
                        difference <= 1.0,
                        "target {target:?} r {radius} pixel ({x}, {y}): {actual:?} vs {expected:?}"
                    );
                }
                partially_covered += usize::from(actual[3] > 0 && actual[3] < 255);
            }
        }
        assert_eq!(pixel(&pixels, 0, 0), [0, 0, 0, 0], "corner is transparent");
        assert_eq!(
            pixel(&pixels, target[0] / 2, target[1] / 2),
            BACKGROUND_PIXEL,
            "centre unchanged"
        );
        assert!(
            partially_covered > 0,
            "anti-aliased edge must have intermediate alpha"
        );
        compositor.destroy(&context).unwrap();
    }
}

/// Postmultiplied-цель: цвет не трогается, умножается только альфа.
#[test]
fn postmultiplied_mask_keeps_color_and_scales_alpha() {
    let Some(mut context) = offscreen_or_skip(
        "postmultiplied_mask_keeps_color_and_scales_alpha",
        size(64, 64),
        OffscreenAlpha::PremultipliedLayer,
    ) else {
        return;
    };
    let mut mask = CornerMaskRenderer::new(&context).unwrap();
    match context.begin_frame().unwrap() {
        FrameStart::Ready(frame) => {
            frame.clear(BACKGROUND);
            let outcome = mask
                .draw(
                    &frame,
                    CornerRadius::from_points(12.0),
                    1.0,
                    CornerAlphaEncoding::Postmultiplied,
                )
                .unwrap();
            assert_eq!(
                outcome,
                CornerMaskOutcome::Drawn {
                    geometry_uploaded: true
                }
            );
            frame.present().unwrap();
        }
        FrameStart::Skipped(reason) => panic!("offscreen frame skipped: {reason:?}"),
    }
    let pixels = read(&context);
    assert_eq!(pixel(&pixels, 0, 0), [64, 128, 191, 0]);
    assert_eq!(pixel(&pixels, 63, 63), [64, 128, 191, 0]);
    assert_eq!(pixel(&pixels, 32, 32), BACKGROUND_PIXEL);
    assert_eq!(
        pixel(&pixels, 24, 4),
        BACKGROUND_PIXEL,
        "safe area near the top edge"
    );
    mask.destroy(&context).unwrap();
}

/// Радиус 0, NaN, отрицательный — прохода нет, пиксели фона на месте.
#[test]
fn square_radius_skips_the_pass() {
    let Some(mut context) = offscreen_or_skip(
        "square_radius_skips_the_pass",
        size(32, 32),
        OffscreenAlpha::PremultipliedLayer,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    for radius in [
        CornerRadius::square(),
        CornerRadius::from_points(f32::NAN),
        CornerRadius::from_points(-4.0),
    ] {
        let outcome = masked_frame(&mut context, &mut compositor, radius, 1.0);
        assert_eq!(outcome, CornerMaskOutcome::SkippedSquare);
        assert_eq!(pixel(&read(&context), 0, 0), BACKGROUND_PIXEL);
    }
    // NaN в масштабе тоже не доходит до GPU.
    let outcome = masked_frame(
        &mut context,
        &mut compositor,
        CornerRadius::from_points(8.0),
        f32::NAN,
    );
    assert_eq!(outcome, CornerMaskOutcome::SkippedSquare);
    assert_eq!(compositor.corner_mask().stats().draws, 0);
    compositor.destroy(&context).unwrap();
}

/// Геометрия грузится только при смене размера или радиуса; непрозрачная цель масок не рисует.
#[test]
fn geometry_is_uploaded_only_on_change_and_opaque_target_stays_square() {
    let Some(mut context) = offscreen_or_skip(
        "geometry_is_uploaded_only_on_change_and_opaque_target_stays_square",
        size(48, 32),
        OffscreenAlpha::PremultipliedLayer,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let radius = CornerRadius::from_points(6.0);
    let first = masked_frame(&mut context, &mut compositor, radius, 1.0);
    let second = masked_frame(&mut context, &mut compositor, radius, 1.0);
    context
        .resize(PixelExtent {
            width: 40,
            height: 32,
        })
        .unwrap();
    let resized = masked_frame(&mut context, &mut compositor, radius, 1.0);
    assert_eq!(
        first,
        CornerMaskOutcome::Drawn {
            geometry_uploaded: true
        }
    );
    assert_eq!(
        second,
        CornerMaskOutcome::Drawn {
            geometry_uploaded: false
        }
    );
    assert_eq!(
        resized,
        CornerMaskOutcome::Drawn {
            geometry_uploaded: true
        }
    );
    assert_eq!(compositor.corner_mask().stats().geometry_uploads, 2);
    compositor.destroy(&context).unwrap();

    let Some(mut opaque) = offscreen_or_skip(
        "geometry_is_uploaded_only_on_change_and_opaque_target_stays_square",
        size(32, 32),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&opaque, EguiPainterOptions::default()).unwrap();
    let outcome = masked_frame(&mut opaque, &mut compositor, radius, 1.0);
    assert_eq!(outcome, CornerMaskOutcome::SkippedSquare);
    assert_eq!(pixel(&read(&opaque), 0, 0), BACKGROUND_PIXEL);
    compositor.destroy(&opaque).unwrap();
}
