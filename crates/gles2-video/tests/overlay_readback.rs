//! Слой bitmap-оверлеев на настоящем EGL без окна (llvmpipe в CI): пиксели после смешивания,
//! счётчики загрузок, группы, обрезка, порядок, отказы. Эталон — формула смешивания на CPU
//! (`support::overlay::blend_over`), а не константы слоя.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_egui::{FrameStart, OffscreenPixels};
use gles2_video::{
    AtlasSideLimit, OverlayAtlasOutcome, OverlayCanvasOrigins, OverlayDrawError,
    OverlayDrawOutcome, OverlayDrawRequest, OverlayLayerOptions, OverlayPixelFormat,
    OverlayPlacementGroup, OverlayPrepareError, OverlayRowUploadPreference, PixelRect,
};
use support::overlay::{
    BACKGROUND, TestBitmap, WHITE, blend_over, coverage_source, origins_at, overlay_frame,
    overlay_renderer, picture_source, rect, request_everywhere, tint,
};
use support::{assert_close, offscreen_or_skip, pixel, size};

const TARGET_WIDTH: u32 = 64;
const TARGET_HEIGHT: u32 = 48;
/// Допуск смешивания: округление нормированных байтов драйвером (±1) и fp32 шейдера.
const BLEND_TOLERANCE: u8 = 2;

/// Цвет фона цели в байтах (как его записал драйвер).
fn background_pixel(pixels: &OffscreenPixels) -> [u8; 4] {
    pixel(pixels, TARGET_WIDTH - 1, TARGET_HEIGHT - 1)
}

#[test]
fn coverage_mask_blends_premultiplied_over_the_target_pixel_for_pixel() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("coverage_mask_blends", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let color = tint(200, 100, 50, 200);
    // Градиент покрытия: каждый пиксель свой, так что сдвиг на тексель ловится.
    let mask = TestBitmap::from_fn(
        OverlayPixelFormat::Coverage,
        rect(5, 7, 16, 8),
        0,
        |x, y| [(x * 16 + y * 2) as u8; 4],
    )
    .tinted(color);
    let request = request_everywhere(target, origins_at(10.0, 4.0));
    let (report, outcome, pixels) =
        overlay_frame(&mut context, &mut renderer, Some(&[mask]), &request);
    assert_eq!(
        report.map(|report| report.coverage),
        Some(OverlayAtlasOutcome::Uploaded { bitmaps: 1 })
    );
    assert_eq!(
        outcome,
        OverlayDrawOutcome::Drawn {
            bitmaps: 1,
            draw_calls: 1
        }
    );
    let background = background_pixel(&pixels);
    for y in 0..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            // Холст в (10, 4), картинка на холсте в (5, 7): на цели (15..31, 11..19).
            let inside = (15..31).contains(&x) && (11..19).contains(&y);
            let expected = if inside {
                let coverage = ((x - 15) * 16 + (y - 11) * 2) as u8;
                blend_over(coverage_source(coverage, color), background)
            } else {
                background
            };
            assert_close(
                pixel(&pixels, x, y),
                expected,
                BLEND_TOLERANCE,
                &format!("pixel ({x}, {y})"),
            );
        }
    }
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn picture_blends_its_premultiplied_texels_scaled_by_the_tint_alpha() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("picture_blends", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let texel = [128, 0, 0, 128];
    let opaque = TestBitmap::solid_picture(rect(0, 0, 8, 8), texel);
    let faded = TestBitmap::solid_picture(rect(20, 0, 8, 8), texel).tinted(tint(0, 0, 0, 128));
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let (_, _, pixels) = overlay_frame(
        &mut context,
        &mut renderer,
        Some(&[opaque, faded]),
        &request,
    );
    let background = background_pixel(&pixels);
    assert_close(
        pixel(&pixels, 3, 3),
        blend_over(picture_source(texel, 255), background),
        BLEND_TOLERANCE,
        "opaque tint",
    );
    assert_close(
        pixel(&pixels, 23, 3),
        blend_over(picture_source(texel, 128), background),
        BLEND_TOLERANCE,
        "tint alpha 128",
    );
    assert_eq!(
        pixel(&pixels, 3, 3)[3],
        255,
        "непрозрачная цель остаётся непрозрачной"
    );
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn unchanged_pixels_upload_nothing_and_a_new_buffer_uploads_once_more() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("unchanged_pixels_upload_nothing", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let first = TestBitmap::solid_coverage(rect(1, 1, 10, 5), 255, WHITE);
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let (_, _, before) = overlay_frame(
        &mut context,
        &mut renderer,
        Some(std::slice::from_ref(&first)),
        &request,
    );
    let after_first = renderer.stats();
    assert_eq!(after_first.coverage.atlas_uploads, 1);
    assert_eq!(after_first.coverage.atlas_recreations, 1);
    assert_eq!(after_first.coverage.bitmap_uploads, 1);
    assert_eq!(after_first.uniform_updates, 1);

    // Тот же буфер, сдвинутый на холсте: атлас не грузится, пиксели едут (вершины обновлены).
    let moved = first.moved_to(21, 1);
    let (report, _, shifted) = overlay_frame(&mut context, &mut renderer, Some(&[moved]), &request);
    assert_eq!(
        report.map(|report| report.coverage),
        Some(OverlayAtlasOutcome::AlreadyResident { bitmaps: 1 })
    );
    assert_eq!(renderer.stats().coverage, after_first.coverage);
    assert_eq!(pixel(&shifted, 25, 3), pixel(&before, 5, 3));
    assert_eq!(pixel(&shifted, 5, 3), background_pixel(&shifted));

    // Новый буфер с теми же размерами — новая загрузка, атлас тот же.
    let replaced = TestBitmap::solid_coverage(rect(1, 1, 10, 5), 255, WHITE);
    overlay_frame(&mut context, &mut renderer, Some(&[replaced]), &request);
    let stats = renderer.stats().coverage;
    assert_eq!(stats.atlas_uploads, 2);
    assert_eq!(stats.bitmap_uploads, 2);
    assert_eq!(stats.atlas_recreations, 1, "атлас переиспользован");
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn group_origins_move_their_bitmaps_by_rewriting_only_uniforms() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("group_origins_move", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let main = TestBitmap::solid_coverage(rect(0, 30, 6, 4), 255, tint(255, 0, 0, 255));
    let fixed = TestBitmap::solid_coverage(rect(30, 2, 6, 4), 255, tint(0, 255, 0, 255))
        .in_group(OverlayPlacementGroup::Fixed);
    let bitmaps = [main, fixed];
    let still = request_everywhere(target, origins_at(0.0, 0.0));
    overlay_frame(&mut context, &mut renderer, Some(&bitmaps), &still);
    let uploads = renderer.stats().coverage;

    let lifted = request_everywhere(
        target,
        OverlayCanvasOrigins {
            main: [0.0, -20.0],
            fixed: [0.0, 0.0],
        },
    );
    let (_, _, pixels) = overlay_frame(&mut context, &mut renderer, None, &lifted);
    let background = background_pixel(&pixels);
    assert_eq!(
        pixel(&pixels, 2, 11),
        [255, 0, 0, 255],
        "основная поднята на 20"
    );
    assert_eq!(pixel(&pixels, 2, 31), background, "на старом месте пусто");
    assert_eq!(
        pixel(&pixels, 32, 3),
        [0, 255, 0, 255],
        "фиксированная на месте"
    );
    let stats = renderer.stats();
    assert_eq!(stats.coverage, uploads, "атлас и картинки не перезагружены");
    assert_eq!(stats.uniform_updates, 2);

    // Повтор того же сдвига — uniform-ы те же.
    overlay_frame(&mut context, &mut renderer, None, &lifted);
    assert_eq!(renderer.stats().uniform_updates, 2);
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn drawing_stays_inside_the_clip_and_an_empty_clip_touches_nothing() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("drawing_stays_inside_the_clip", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let wide = TestBitmap::solid_coverage(rect(0, 10, 64, 10), 255, WHITE);
    let clipped = OverlayDrawRequest {
        origins: origins_at(0.0, 0.0),
        clip: PixelRect {
            x: 0,
            y: 0,
            width: 32,
            height: TARGET_HEIGHT,
        },
    };
    let (_, _, pixels) = overlay_frame(&mut context, &mut renderer, Some(&[wide]), &clipped);
    let background = background_pixel(&pixels);
    assert_eq!(pixel(&pixels, 31, 15), [255, 255, 255, 255]);
    assert_eq!(
        pixel(&pixels, 32, 15),
        background,
        "справа от области не рисуем"
    );

    let outside = OverlayDrawRequest {
        origins: origins_at(0.0, 0.0),
        clip: PixelRect {
            x: TARGET_WIDTH,
            y: 0,
            width: 10,
            height: 10,
        },
    };
    let draws_before = renderer.stats().draw_calls;
    let (_, outcome, pixels) = overlay_frame(&mut context, &mut renderer, None, &outside);
    assert_eq!(outcome, OverlayDrawOutcome::NothingVisible);
    assert_eq!(pixel(&pixels, 10, 15), background);
    assert_eq!(renderer.stats().draw_calls, draws_before);
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn drawing_leaves_blending_and_scissor_off_for_the_next_pass() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("drawing_leaves_state_off", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let small = TestBitmap::solid_coverage(rect(0, 0, 4, 4), 128, WHITE);
    let clipped = OverlayDrawRequest {
        origins: origins_at(0.0, 0.0),
        clip: PixelRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        },
    };
    let FrameStart::Ready(frame) = context.begin_frame().expect("frame begins") else {
        panic!("offscreen frame skipped");
    };
    frame.clear(BACKGROUND);
    renderer.prepare(&frame, &[small]).expect("prepares");
    renderer.draw(&frame, &clipped).expect("draws");
    // Следующий проход заливает всю цель: если бы scissor остался включённым, залилась бы только
    // область субтитров; если бы осталось смешивание, заливка его не заметила бы, поэтому её
    // проверяет пиксель внутри области (без смешивания — ровно цвет заливки).
    frame.clear([1.0, 0.0, 0.0, 1.0]);
    frame.present().expect("presents");
    let pixels = context.read_offscreen_pixels().expect("readback");
    assert_eq!(pixel(&pixels, 60, 40), [255, 0, 0, 255], "scissor выключен");
    assert_eq!(pixel(&pixels, 1, 1), [255, 0, 0, 255]);
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn list_order_is_kept_across_kinds_with_one_draw_per_run() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("list_order_is_kept", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let red_text = TestBitmap::solid_coverage(rect(0, 0, 20, 10), 255, tint(255, 0, 0, 255));
    let blue_picture = TestBitmap::solid_picture(rect(5, 0, 10, 10), [0, 0, 255, 255]);
    let green_text = TestBitmap::solid_coverage(rect(8, 0, 4, 10), 255, tint(0, 255, 0, 255));
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let (report, outcome, pixels) = overlay_frame(
        &mut context,
        &mut renderer,
        Some(&[red_text, blue_picture, green_text]),
        &request,
    );
    assert_eq!(report.map(|report| report.drawable_bitmaps), Some(3));
    assert_eq!(
        outcome,
        OverlayDrawOutcome::Drawn {
            bitmaps: 3,
            draw_calls: 3
        },
        "маска, картинка, маска — три серии"
    );
    assert_eq!(pixel(&pixels, 2, 5), [255, 0, 0, 255], "нижняя маска");
    assert_eq!(
        pixel(&pixels, 6, 5),
        [0, 0, 255, 255],
        "картинка поверх маски"
    );
    assert_eq!(
        pixel(&pixels, 9, 5),
        [0, 255, 0, 255],
        "верхняя маска поверх картинки"
    );
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn bitmaps_larger_than_the_atlas_limit_are_skipped_while_neighbours_draw() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("bitmaps_larger_than_the_atlas_limit", target) else {
        return;
    };
    let options = OverlayLayerOptions {
        atlas_side_limit: AtlasSideLimit::AtMost(32),
        ..OverlayLayerOptions::default()
    };
    let mut renderer = overlay_renderer(&context, options);
    assert_eq!(renderer.max_atlas_side(), 32);
    let too_wide = TestBitmap::solid_picture(rect(0, 0, 40, 4), [255, 0, 0, 255]);
    let neighbour = TestBitmap::solid_picture(rect(0, 10, 6, 6), [0, 255, 0, 255]);
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let (report, outcome, pixels) = overlay_frame(
        &mut context,
        &mut renderer,
        Some(&[too_wide, neighbour]),
        &request,
    );
    let report = report.expect("prepared");
    assert_eq!(report.larger_than_atlas_limit, 1);
    assert_eq!(
        outcome,
        OverlayDrawOutcome::Drawn {
            bitmaps: 1,
            draw_calls: 1
        }
    );
    assert_eq!(pixel(&pixels, 2, 12), [0, 255, 0, 255]);
    assert_eq!(pixel(&pixels, 2, 2), background_pixel(&pixels));
    let stats = renderer.stats().picture;
    assert_eq!(stats.skipped_bitmaps, 1);
    assert_eq!(stats.skipped_prepares, 1);
    assert_eq!(stats.bitmap_uploads, 1);
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn atlas_grows_when_the_set_does_not_fit_and_never_shrinks() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("atlas_grows", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let big = TestBitmap::solid_coverage(rect(0, 0, 300, 20), 255, WHITE);
    overlay_frame(&mut context, &mut renderer, Some(&[big]), &request);
    let grown = renderer.stats().coverage;
    assert_eq!(grown.atlas_side, 512);
    assert_eq!(grown.atlas_recreations, 1);
    let small = TestBitmap::solid_coverage(rect(0, 0, 4, 4), 255, WHITE);
    let (_, _, pixels) = overlay_frame(&mut context, &mut renderer, Some(&[small]), &request);
    let stats = renderer.stats().coverage;
    assert_eq!(stats.atlas_side, 512, "атлас не уменьшается");
    assert_eq!(stats.atlas_recreations, 1);
    assert_eq!(pixel(&pixels, 1, 1), [255, 255, 255, 255]);
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn rows_with_tails_upload_the_same_pixels_on_both_row_paths() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("rows_with_tails", target) else {
        return;
    };
    let pattern = |x: u32, y: u32| [(x * 20) as u8, (y * 30) as u8, 90, 255];
    // Хвост 0xFF после каждой строки: попади он в атлас, цвет уедет далеко.
    let window = TestBitmap::from_fn(
        OverlayPixelFormat::PremultipliedRgba,
        rect(3, 2, 7, 5),
        12,
        pattern,
    );
    let mask = TestBitmap::from_fn(
        OverlayPixelFormat::Coverage,
        rect(20, 2, 9, 5),
        5,
        |x, _| [(x * 28) as u8; 4],
    );
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let mut results = Vec::new();
    for row_upload in [
        OverlayRowUploadPreference::BestAvailable,
        OverlayRowUploadPreference::RowByRow,
    ] {
        let options = OverlayLayerOptions {
            row_upload,
            ..OverlayLayerOptions::default()
        };
        let mut renderer = overlay_renderer(&context, options);
        let (_, _, pixels) = overlay_frame(
            &mut context,
            &mut renderer,
            Some(&[window.clone(), mask.clone()]),
            &request,
        );
        let calls = renderer.stats().picture.upload_calls + renderer.stats().coverage.upload_calls;
        assert!(renderer.destroy(&context).is_ok());
        results.push((pixels, calls));
    }
    let (best, best_calls) = &results[0];
    let (rows, row_calls) = &results[1];
    assert_eq!(best.as_rgba(), rows.as_rgba(), "оба пути дают одни пиксели");
    assert_eq!(*row_calls, 10, "по строке на вызов: 5 + 5");
    assert!(*best_calls <= *row_calls);
    for y in 0..5 {
        for x in 0..7 {
            assert_eq!(pixel(best, 3 + x, 2 + y), pattern(x, y), "texel ({x}, {y})");
        }
    }
}

#[test]
fn malformed_pixels_are_rejected_before_gl_and_nothing_is_drawn_after() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("malformed_pixels_are_rejected", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let valid = TestBitmap::solid_coverage(rect(0, 0, 4, 4), 255, WHITE);
    overlay_frame(&mut context, &mut renderer, Some(&[valid]), &request);
    let before = renderer.stats();

    let mut truncated = TestBitmap::solid_coverage(rect(0, 0, 8, 8), 255, WHITE);
    truncated.rect.height = 9; // буфер на 8 строк, заявлено 9
    let FrameStart::Ready(frame) = context.begin_frame().expect("frame begins") else {
        panic!("offscreen frame skipped");
    };
    let error = renderer
        .prepare(&frame, &[truncated])
        .expect_err("short buffer is rejected");
    assert!(matches!(
        error,
        OverlayPrepareError::MalformedBitmap { index: 0, .. }
    ));
    assert_eq!(renderer.stats(), before, "атлас и счётчики не тронуты");
    // Прежний подготовленный набор остаётся (отказ — до GL и до сброса серий).
    assert!(matches!(
        renderer.draw(&frame, &request).expect("draws"),
        OverlayDrawOutcome::Drawn { bitmaps: 1, .. }
    ));
    frame.present().expect("presents");
    assert!(renderer.destroy(&context).is_ok());
}

#[test]
fn foreign_context_frames_are_rejected_without_touching_the_layer() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(owner) = offscreen_or_skip("foreign_context_owner", target) else {
        return;
    };
    let Some(mut stranger) = offscreen_or_skip("foreign_context_stranger", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&owner, OverlayLayerOptions::default());
    let bitmap = TestBitmap::solid_coverage(rect(0, 0, 4, 4), 255, WHITE);
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let FrameStart::Ready(frame) = stranger.begin_frame().expect("frame begins") else {
        panic!("offscreen frame skipped");
    };
    assert_eq!(
        renderer.prepare(&frame, &[bitmap]),
        Err(OverlayPrepareError::ForeignContext)
    );
    assert_eq!(
        renderer.draw(&frame, &request),
        Err(OverlayDrawError::ForeignContext)
    );
    frame.present().expect("presents");
    assert_eq!(renderer.stats(), gles2_video::OverlayLayerStats::default());
    assert!(renderer.destroy(&owner).is_ok());
}

#[test]
fn nothing_prepared_draws_nothing_and_counts_nothing() {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip("nothing_prepared", target) else {
        return;
    };
    let mut renderer = overlay_renderer(&context, OverlayLayerOptions::default());
    let request = request_everywhere(target, origins_at(0.0, 0.0));
    let (_, outcome, pixels) = overlay_frame(&mut context, &mut renderer, None, &request);
    assert_eq!(outcome, OverlayDrawOutcome::NothingPrepared);
    assert_eq!(pixel(&pixels, 0, 0), background_pixel(&pixels));
    let (report, outcome, _) = overlay_frame(&mut context, &mut renderer, Some(&[]), &request);
    assert_eq!(
        report.map(|report| (report.coverage, report.picture)),
        Some((
            OverlayAtlasOutcome::NotNeeded,
            OverlayAtlasOutcome::NotNeeded
        ))
    );
    assert_eq!(outcome, OverlayDrawOutcome::NothingPrepared);
    assert_eq!(renderer.stats(), gles2_video::OverlayLayerStats::default());
    assert!(renderer.destroy(&context).is_ok());
}
