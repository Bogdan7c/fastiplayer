//! Пиксельный паритет интерфейса wgpu ↔ GLES2 (сессия 04 плана `user/gles2-renderer/`,
//! решение владельца 7 «строгий паритет», решение 28 «тест живёт в шелле wgpu»).
//!
//! Один и тот же проход egui (примитивы + изменения текстур) рисуют:
//! - настоящий wgpu-путь шелла (`EguiCompositor` + `WindowCornerMaskRenderer`);
//! - GLES2-сборщик `gles2_egui::FrameCompositor` на EGL без окна.
//!
//! Пиксели сравниваются целиком, допуск ±2 на канал (растеризация и дизеринг в разных API);
//! расхождения выше допуска — провал с координатой и обоими значениями. Без GPU-адаптера wgpu
//! или без EGL тест пропускается с причиной; `GLES2_EGUI_REQUIRE_EGL=1` делает отсутствие EGL
//! провалом.

mod dma_buf_video;
mod hdr_constants;
mod scenes;
mod udmabuf;
mod video;
mod video_high_bit;
mod wgpu_target;

use gles2_egui::{
    CornerRadius, CreateError, EguiPainterOptions, FrameCompositor, Gles2Context, NoFrameLayers,
    OffscreenAlpha, SurfaceSize, UiContent, UiFrameOutcome, UiFrameRequest, UiScreen,
};

use self::scenes::{ParityFrame, parity_frames};
use self::wgpu_target::WgpuParityTarget;

/// Допуск на канал (план сессии: ±1–2 из-за растеризации/дизеринга).
const CHANNEL_TOLERANCE: u8 = 2;
/// Не больше 1/100 пикселей кадра могут отличаться хоть на единицу.
const MAX_DIFFERING_PIXELS_DIVISOR: usize = 100;
const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

/// Итог сравнения одного кадра.
#[derive(Debug, Default)]
struct ParityReport {
    max_channel_difference: u8,
    pixels_differing: usize,
    pixels_over_tolerance: usize,
    first_over_tolerance: Option<(u32, u32, [u8; 4], [u8; 4])>,
    distinct_colors: usize,
}

fn compare(wgpu_pixels: &[u8], gles2_pixels: &[u8], width: u32) -> ParityReport {
    compare_with_tolerance(wgpu_pixels, gles2_pixels, width, CHANNEL_TOLERANCE)
}

/// Сравнение с допуском `tolerance` на канал (HDR — документированный допуск таблицы GLES2).
fn compare_with_tolerance(
    wgpu_pixels: &[u8],
    gles2_pixels: &[u8],
    width: u32,
    tolerance: u8,
) -> ParityReport {
    assert_eq!(wgpu_pixels.len(), gles2_pixels.len(), "same target size");
    let mut report = ParityReport::default();
    let mut colors = std::collections::HashSet::new();
    let wgpu_pixels = wgpu_pixels.as_chunks::<4>().0;
    let gles2_pixels = gles2_pixels.as_chunks::<4>().0;
    for (index, (&wgpu_pixel, &gles2_pixel)) in wgpu_pixels.iter().zip(gles2_pixels).enumerate() {
        colors.insert(wgpu_pixel);
        let difference = (0..4)
            .map(|channel| wgpu_pixel[channel].abs_diff(gles2_pixel[channel]))
            .max()
            .unwrap_or(0);
        report.max_channel_difference = report.max_channel_difference.max(difference);
        report.pixels_differing += usize::from(difference > 0);
        if difference > tolerance {
            report.pixels_over_tolerance += 1;
            if report.first_over_tolerance.is_none() {
                let index = index as u32;
                report.first_over_tolerance =
                    Some((index % width, index / width, wgpu_pixel, gles2_pixel));
            }
        }
    }
    report.distinct_colors = colors.len();
    report
}

/// GLES2-контекст без окна (premultiplied-цель, чтобы работала маска углов) или пропуск.
fn gles2_or_skip(test_name: &str, size: [u32; 2], alpha: OffscreenAlpha) -> Option<Gles2Context> {
    let target = SurfaceSize::new(size[0], size[1]).expect("non-zero parity target");
    match Gles2Context::offscreen_with_alpha(target, alpha) {
        Ok(context) => Some(context),
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required ({REQUIRE_EGL_VARIABLE}) but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL surfaceless GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: GLES2 context creation failed: {error}"),
    }
}

fn render_gles2(
    compositor: &mut FrameCompositor,
    context: &mut Gles2Context,
    size: [u32; 2],
    frame: &ParityFrame,
) -> (Vec<u8>, gles2_egui::PresentedUiFrame) {
    let content = match &frame.ui {
        Some((primitives, textures_delta)) => UiContent::Fresh {
            primitives: primitives.clone(),
            textures_delta: textures_delta.clone(),
        },
        None => UiContent::ReusePrevious,
    };
    let outcome = compositor
        .render(
            context,
            UiFrameRequest {
                screen: UiScreen {
                    size_in_pixels: size,
                    pixels_per_point: frame.pixels_per_point,
                },
                content,
                background: frame.background,
                layers: &mut NoFrameLayers,
                corner_radius: CornerRadius::from_points(frame.corner_radius_points),
            },
        )
        .expect("GLES2 frame renders");
    let UiFrameOutcome::Presented(presented) = outcome else {
        panic!(
            "{}: GLES2 frame must be presented, got {outcome:?}",
            frame.name
        );
    };
    let pixels = context
        .read_offscreen_pixels()
        .expect("GLES2 readback")
        .as_rgba()
        .to_vec();
    (pixels, presented)
}

/// Прогнать все кадры через оба рендера и сравнить.
fn assert_parity(test_name: &str, size: [u32; 2], pixels_per_point: f32) {
    let Some(mut wgpu_target) = WgpuParityTarget::new(size) else {
        eprintln!("skip {test_name}: no wgpu adapter");
        return;
    };
    let Some(mut context) = gles2_or_skip(test_name, size, OffscreenAlpha::PremultipliedLayer)
    else {
        return;
    };
    let mut compositor =
        FrameCompositor::new(&context, EguiPainterOptions::default()).expect("GLES2 compositor");
    for (index, frame) in parity_frames(size, pixels_per_point).iter().enumerate() {
        let wgpu_pixels = wgpu_target.render(frame);
        let (gles2_pixels, presented) = render_gles2(&mut compositor, &mut context, size, frame);
        assert_scene_exercised(index, frame, &presented);
        let report = compare(&wgpu_pixels, &gles2_pixels, size[0]);
        eprintln!(
            "{test_name} [{}] {}: max Δ {}, differing {} / {}, over ±{CHANNEL_TOLERANCE}: {}, \
             colors {}",
            index + 1,
            frame.name,
            report.max_channel_difference,
            report.pixels_differing,
            size[0] * size[1],
            report.pixels_over_tolerance,
            report.distinct_colors,
        );
        assert!(
            report.distinct_colors > 40,
            "{}: the scene must be rich (text, AA, shadows), got {} colors",
            frame.name,
            report.distinct_colors
        );
        assert_eq!(
            report.pixels_over_tolerance, 0,
            "{}: first pixel over tolerance (x, y, wgpu, gles2) = {:?}",
            frame.name, report.first_over_tolerance
        );
        // Допуск ±2 сам по себе не отличит потерянный дизеринг (это ±1 на градиентах). С тем же
        // шумом, что у egui-wgpu, различаются ~0,4 % пикселей (края растеризации); без шума или
        // с шумом по неперевёрнутой оси Y — 46–68 % (проверено, сессия 04).
        let differing_limit = (size[0] * size[1]) as usize / MAX_DIFFERING_PIXELS_DIVISOR;
        assert!(
            report.pixels_differing <= differing_limit,
            "{}: {} pixels differ (limit {differing_limit}): dithering or rasterization diverged",
            frame.name,
            report.pixels_differing
        );
    }
    compositor
        .destroy(&context)
        .expect("destroy GLES2 compositor");
}

/// Кадры действительно проверяют то, что заявлено: частичную правку, `free`, повтор, маску.
fn assert_scene_exercised(
    index: usize,
    frame: &ParityFrame,
    presented: &gles2_egui::PresentedUiFrame,
) {
    let work = presented.work;
    assert!(
        matches!(
            work.corner_mask,
            gles2_egui::CornerMaskOutcome::Drawn { .. }
        ),
        "{}: corner mask must be drawn",
        frame.name
    );
    match index {
        0 => assert!(
            work.textures
                .is_some_and(|textures| textures.full_uploads >= 1)
        ),
        1 => assert!(
            work.textures
                .is_some_and(|textures| textures.patch_uploads >= 1 && textures.full_uploads >= 1),
            "{}: new glyphs patch the atlas and the user texture uploads: {:?}",
            frame.name,
            work.textures
        ),
        2 => assert_eq!(
            work.texture_free,
            gles2_egui::TextureFreeOutcome::Applied { deleted: 1 },
            "{}: user texture is freed",
            frame.name
        ),
        _ => assert!(
            work.prepare.is_none() && work.textures.is_none(),
            "{}: reuse uploads nothing",
            frame.name
        ),
    }
}

#[test]
fn egui_frames_match_wgpu_pixel_for_pixel() {
    assert_parity("egui_frames_match_wgpu_pixel_for_pixel", [320, 200], 1.0);
}

/// Дробный масштаб и нечётный размер: округление scissor, позиции в точках, маска.
#[test]
fn scaled_odd_sized_egui_frames_match_wgpu() {
    assert_parity("scaled_odd_sized_egui_frames_match_wgpu", [301, 187], 1.5);
}
