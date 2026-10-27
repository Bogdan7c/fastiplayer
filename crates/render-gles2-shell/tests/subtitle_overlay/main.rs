//! Порт GPU-тестов wgpu-прохода субтитров (`render-wgpu-shell/src/subtitle_overlay/tests*.rs`)
//! на GLES2 без окна (EGL surfaceless; llvmpipe в CI): те же названия, те же входы, те же
//! ожидания по пикселям и счётчикам. Настоящий текст/ASS/PGS → растеризатор → адаптер шелла →
//! слой оверлеев `gles2-video` → пиксели.
//!
//! Без EGL тест пропускается с причиной в stderr; `GLES2_EGUI_REQUIRE_EGL=1` превращает пропуск в
//! падение (CI ставит Mesa EGL, решение владельца 27).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "integration tests fail loudly on broken invariants; pixel math on small test sizes"
)]

use gles2_egui::{CreateError, FrameStart, Gles2Context, SurfaceSize};
use gles2_video::{AtlasSideLimit, OverlayLayerOptions};
use render_core::{PictureLayout, PictureSize, RenderViewport};
use render_gles2_shell::{
    Gles2SubtitleOverlayRenderer, SubtitleOverlayOutcome, SubtitleOverlayUploadStats,
};
use render_shell_api::{SubtitleLayerOffset, SubtitleLayerOffsets, SubtitleOverlayFrame};
use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};
use subtitle_raster::{
    AssRenderOptions, CanvasSize, CueKey, CueSourceId, FontLookupCacheLimits, RasterCacheLimits,
    RasterInputLimits, RasterLimits, RenderRequest, RenderableCue, SubtitleAppearance,
    SubtitleFontLibrary, SubtitleOverlay, SubtitleRasterizer, TextLanguagePreference,
};

const TARGET_WIDTH: u32 = 256;
const TARGET_HEIGHT: u32 = 144;
/// Допуск на округление GPU (unorm8) и целочисленного эталона растеризатора (как у wgpu).
const BLEND_TOLERANCE: i32 = 3;
/// `GLES2_EGUI_REQUIRE_EGL=1` — пропуск без EGL становится падением.
const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

/// Headless EGL + цель 256x144 + проход субтитров под тестом.
struct Harness {
    context: Gles2Context,
    /// `Some` всё время жизни стенда; `Drop` забирает слой, чтобы отдать GL-объекты контексту.
    renderer: Option<Gles2SubtitleOverlayRenderer>,
}

impl Harness {
    /// `None`, если нет EGL (пропуск с причиной).
    fn new() -> Option<Self> {
        Self::with_texture_limit(None)
    }

    /// То же, но с пределом стороны атласа (аналог сниженного `max_texture_dimension_2d` у wgpu:
    /// картинка крупнее предела пропускается).
    fn with_texture_limit(max_texture_side: Option<u32>) -> Option<Self> {
        let size = SurfaceSize::new(TARGET_WIDTH, TARGET_HEIGHT).expect("non-zero target");
        let context = match Gles2Context::offscreen(size) {
            Ok(context) => context,
            Err(
                error @ (CreateError::EglUnavailable { .. }
                | CreateError::PlatformExtensionMissing { .. }
                | CreateError::DisplayOpen(_)),
            ) => {
                assert!(
                    std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                    "EGL is required ({REQUIRE_EGL_VARIABLE}) but unavailable: {error}"
                );
                eprintln!("skip: no EGL surfaceless GLES2 context: {error}");
                return None;
            }
            Err(error) => panic!("offscreen GLES2 context creation failed: {error}"),
        };
        let options = OverlayLayerOptions {
            atlas_side_limit: max_texture_side
                .map_or(AtlasSideLimit::DriverMaximum, AtlasSideLimit::AtMost),
            ..OverlayLayerOptions::default()
        };
        let renderer =
            Gles2SubtitleOverlayRenderer::new(&context, options).expect("subtitle overlay layer");
        Some(Self {
            context,
            renderer: Some(renderer),
        })
    }

    /// Проход под тестом.
    fn renderer(&self) -> &Gles2SubtitleOverlayRenderer {
        self.renderer.as_ref().expect("renderer lives until drop")
    }

    /// Один кадр: заливка «видео» известным цветом, субтитры, показ, readback RGBA сверху вниз.
    fn frame(
        &mut self,
        overlay_frame: Option<&SubtitleOverlayFrame<'_>>,
        visible_area: RenderViewport,
    ) -> (SubtitleOverlayOutcome, Vec<u8>) {
        let outcome = match self.context.begin_frame().expect("frame begins") {
            FrameStart::Ready(frame) => {
                // Заливка с альфой 1 имитирует видеопроход (как Clear у wgpu-стенда).
                frame.clear([0.25, 0.5, 0.75, 1.0]);
                let outcome = self
                    .renderer
                    .as_mut()
                    .expect("renderer lives until drop")
                    .render(&frame, overlay_frame, visible_area)
                    .expect("subtitle overlay renders");
                frame.present().expect("frame presents");
                outcome
            }
            FrameStart::Skipped(reason) => panic!("offscreen frame skipped: {reason:?}"),
        };
        let pixels = self
            .context
            .read_offscreen_pixels()
            .expect("offscreen readback");
        (outcome, pixels.as_rgba().to_vec())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Слой отдаёт GL-объекты своему контексту до его разрушения.
        if let Some(renderer) = self.renderer.take() {
            renderer
                .destroy(&self.context)
                .expect("subtitle overlay is destroyed by its own context");
        }
    }
}

/// Растеризатор со встроенными шрифтами (как в тестах subtitle-raster).
fn rasterizer() -> SubtitleRasterizer {
    let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: 16 * 1024 * 1024,
                shaper_faces: 8,
                blur_scratch_retained_bytes: 32 * 1024 * 1024,
                glyph_bitmaps_bytes: 40 * 1024 * 1024,
                glyph_composites_bytes: 20 * 1024 * 1024,
                glyph_outline_items: 10_000,
                shaping_cache_bytes: 4 * 1024 * 1024,
            },
            input: RasterInputLimits {
                max_chars_per_cue: 1000,
                max_cues_per_frame: 8,
                max_ass_events_per_frame: 8,
                max_canvas_side: 8192,
            },
        },
    )
}

/// Растеризует одну реплику (или ни одной при `None`) на холсте `canvas`.
fn rasterize(
    rasterizer: &mut SubtitleRasterizer,
    source: u64,
    text: Option<&str>,
    canvas: CanvasSize,
) -> SubtitleOverlay {
    let mut timeline = SubtitleTimeline::new();
    let cue = SubtitleTextCue {
        text: StyledText::new(vec![TextSpan::text(
            text.unwrap_or(""),
            SpanStyle::default(),
        )]),
        placement: CuePlacement::default(),
    };
    let event = SubtitleEvent::new(
        SubtitleTime::from_millis(0),
        SubtitleTime::from_millis(5),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(cue.clone()),
    )
    .expect("interval");
    let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
        panic!("event must be inserted");
    };
    let renderable = [RenderableCue::text(
        CueKey::new(CueSourceId::new(source), id),
        &cue,
    )];
    let cues: &[RenderableCue<'_>] = if text.is_some() { &renderable } else { &[] };
    rasterizer
        .render(&RenderRequest {
            moment: subtitle_core::SubtitleTime::ZERO,
            cues,
            canvas,
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}

/// Картинка 2.39:1 в окне 256x144: прямоугольник (с полосами) и холст под него.
fn letterboxed_picture() -> (render_core::PictureRect, CanvasSize) {
    let layout = PictureLayout::fit(
        PictureSize::new(2390, 1000),
        RenderViewport::full_surface(TARGET_WIDTH, TARGET_HEIGHT),
    )
    .expect("picture layout");
    let rect = layout.rect();
    let canvas =
        CanvasSize::new(rect.width.round() as u32, rect.height.round() as u32).expect("canvas");
    (rect, canvas)
}

fn full_view() -> RenderViewport {
    RenderViewport::full_surface(TARGET_WIDTH, TARGET_HEIGHT)
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * TARGET_WIDTH + x) * 4) as usize;
    pixels[offset..offset + 4].try_into().expect("pixel")
}

macro_rules! gpu_or_skip {
    () => {
        match Harness::new() {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no EGL for subtitle overlay test");
                return;
            }
        }
    };
}

#[test]
fn hello_cue_is_blended_exactly_on_the_bottom_of_the_letterboxed_picture() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let overlay = rasterize(&mut rasterizer(), 1, Some("Hello"), canvas);
    assert!(!overlay.is_empty());
    let (_, video_only) = harness.frame(None, full_view());

    let (outcome, pixels) = harness.frame(
        Some(&SubtitleOverlayFrame {
            overlay: &overlay,
            picture_rect: rect,
            layer_offsets: SubtitleLayerOffsets::NONE,
        }),
        full_view(),
    );
    assert_eq!(
        outcome,
        SubtitleOverlayOutcome::Drawn {
            bitmaps: overlay.bitmaps().len()
        }
    );

    // Эталон: CPU-композит растеризатора поверх «видео»; холст лежит от округлённого origin.
    let reference = overlay.composite_rgba_over_transparent();
    let origin_x = rect.x.round() as u32;
    let origin_y = rect.y.round() as u32;
    let mut changed_in_bottom_half = 0;
    for y in 0..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            let video = pixel(&video_only, x, y);
            let inside_canvas = x >= origin_x
                && y >= origin_y
                && x < origin_x + canvas.width()
                && y < origin_y + canvas.height();
            let expected = if inside_canvas {
                let offset = (((y - origin_y) * canvas.width() + (x - origin_x)) * 4) as usize;
                let [r, g, b, a] = [0, 1, 2, 3].map(|i| i32::from(reference[offset + i]));
                [
                    (r * a + i32::from(video[0]) * (255 - a)) / 255,
                    (g * a + i32::from(video[1]) * (255 - a)) / 255,
                    (b * a + i32::from(video[2]) * (255 - a)) / 255,
                ]
            } else {
                // Чёрные полосы и поля вне холста обязаны остаться нетронутыми.
                [0, 1, 2].map(|i| i32::from(video[i]))
            };
            let actual = pixel(&pixels, x, y);
            for channel in 0..3 {
                assert!(
                    (i32::from(actual[channel]) - expected[channel]).abs() <= BLEND_TOLERANCE,
                    "pixel ({x},{y}) channel {channel}: actual {} expected {}",
                    actual[channel],
                    expected[channel]
                );
            }
            assert_eq!(actual[3], 255, "alpha must stay opaque at ({x},{y})");
            if inside_canvas && y > origin_y + canvas.height() / 2 && actual != video {
                changed_in_bottom_half += 1;
            }
        }
    }
    assert!(
        changed_in_bottom_half > 100,
        "text plate must be visible at the bottom"
    );
    // Верх картинки (над плашкой) остаётся чистым видео.
    assert_eq!(
        pixel(&pixels, 128, origin_y + 2),
        pixel(&video_only, 128, origin_y + 2)
    );
}

#[test]
fn same_revision_uploads_nothing_and_new_revision_uploads_once_more() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let mut rasterizer = rasterizer();
    let hello = rasterize(&mut rasterizer, 1, Some("Hello"), canvas);
    let hello_again = rasterize(&mut rasterizer, 1, Some("Hello"), canvas);
    assert_eq!(
        hello.revision(),
        hello_again.revision(),
        "rasterizer fast path"
    );
    let frame_of = |overlay| SubtitleOverlayFrame {
        overlay,
        picture_rect: rect,
        layer_offsets: SubtitleLayerOffsets::NONE,
    };

    let (_, first) = harness.frame(Some(&frame_of(&hello)), full_view());
    let after_first = harness.renderer().upload_stats();
    assert_eq!(after_first.atlas_uploads, 1);
    assert_eq!(after_first.atlas_recreations, 1);
    assert_eq!(after_first.bitmap_uploads, hello.bitmaps().len() as u64);
    assert_eq!(after_first.uniform_uploads, 1);

    let (_, second) = harness.frame(Some(&frame_of(&hello_again)), full_view());
    assert_eq!(
        harness.renderer().upload_stats(),
        after_first,
        "same revision: zero uploads"
    );
    assert_eq!(first, second, "same revision draws the same pixels");

    let world = rasterize(&mut rasterizer, 2, Some("World, and more text"), canvas);
    assert_ne!(world.revision(), hello.revision());
    let (_, third) = harness.frame(Some(&frame_of(&world)), full_view());
    let stats = harness.renderer().upload_stats();
    assert_eq!(stats.atlas_uploads, 2);
    assert_eq!(
        stats.atlas_recreations, 1,
        "atlas is reused when it is big enough"
    );
    assert_ne!(first, third, "new content must change pixels");
}

#[test]
fn recreated_rasterizer_with_equal_revision_is_still_uploaded() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let first = rasterize(&mut rasterizer(), 1, Some("Hello"), canvas);
    let second = rasterize(&mut rasterizer(), 1, Some("Different"), canvas);
    assert_eq!(
        first.revision(),
        second.revision(),
        "fresh rasterizers restart revisions"
    );
    let (_, pixels_first) = harness.frame(
        Some(&SubtitleOverlayFrame {
            overlay: &first,
            picture_rect: rect,
            layer_offsets: SubtitleLayerOffsets::NONE,
        }),
        full_view(),
    );
    let (_, pixels_second) = harness.frame(
        Some(&SubtitleOverlayFrame {
            overlay: &second,
            picture_rect: rect,
            layer_offsets: SubtitleLayerOffsets::NONE,
        }),
        full_view(),
    );
    assert_eq!(harness.renderer().upload_stats().atlas_uploads, 2);
    assert_ne!(pixels_first, pixels_second);
}

#[test]
fn missing_or_empty_overlay_draws_nothing_and_touches_no_counters() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let (_, video_only) = harness.frame(None, full_view());
    assert_eq!(
        harness.renderer().upload_stats(),
        SubtitleOverlayUploadStats::default()
    );

    let empty = rasterize(&mut rasterizer(), 1, None, canvas);
    assert!(empty.is_empty());
    let (outcome, pixels) = harness.frame(
        Some(&SubtitleOverlayFrame {
            overlay: &empty,
            picture_rect: rect,
            layer_offsets: SubtitleLayerOffsets::NONE,
        }),
        full_view(),
    );
    assert_eq!(outcome, SubtitleOverlayOutcome::NothingToDraw);
    assert_eq!(pixels, video_only);
    assert_eq!(
        harness.renderer().upload_stats(),
        SubtitleOverlayUploadStats::default()
    );
}

#[test]
fn moving_the_picture_rewrites_only_the_uniform_and_shifts_the_pixels() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let overlay = rasterize(&mut rasterizer(), 1, Some("Hello"), canvas);
    let at = |picture_rect| SubtitleOverlayFrame {
        overlay: &overlay,
        picture_rect,
        layer_offsets: SubtitleLayerOffsets::NONE,
    };

    let (_, original) = harness.frame(Some(&at(rect)), full_view());
    let moved_rect = render_core::PictureRect {
        x: rect.x + 8.0,
        ..rect
    };
    let (_, moved) = harness.frame(Some(&at(moved_rect)), full_view());
    let stats = harness.renderer().upload_stats();
    assert_eq!(
        stats.atlas_uploads, 1,
        "placement change must not reupload masks"
    );
    assert_eq!(stats.uniform_uploads, 2);
    assert_ne!(original, moved);

    // Сдвиг ровно на 8 px: пиксель центра плашки теперь на 8 px правее.
    let y = rect.y.round() as u32 + canvas.height() - 14;
    assert_eq!(pixel(&original, 100, y), pixel(&moved, 108, y));

    // Возврат на прежнее место восстанавливает исходные пиксели.
    let (_, back) = harness.frame(Some(&at(rect)), full_view());
    assert_eq!(back, original);
}

#[test]
fn subtitles_never_paint_outside_the_visible_video_area() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let overlay = rasterize(&mut rasterizer(), 1, Some("Hello"), canvas);
    let frame = SubtitleOverlayFrame {
        overlay: &overlay,
        picture_rect: rect,
        layer_offsets: SubtitleLayerOffsets::NONE,
    };
    let (_, video_only) = harness.frame(None, full_view());
    let (_, unclipped) = harness.frame(Some(&frame), full_view());

    let half = RenderViewport::new(0, 0, TARGET_WIDTH / 2, TARGET_HEIGHT);
    let (outcome, clipped) = harness.frame(Some(&frame), half);
    assert!(matches!(outcome, SubtitleOverlayOutcome::Drawn { .. }));
    let mut changed_inside = 0;
    for y in 0..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            if x >= TARGET_WIDTH / 2 {
                assert_eq!(
                    pixel(&clipped, x, y),
                    pixel(&video_only, x, y),
                    "({x},{y}) outside"
                );
            } else {
                assert_eq!(
                    pixel(&clipped, x, y),
                    pixel(&unclipped, x, y),
                    "({x},{y}) inside"
                );
                changed_inside += usize::from(pixel(&clipped, x, y) != pixel(&video_only, x, y));
            }
        }
    }
    assert!(changed_inside > 0, "left half must still show the plate");

    let (outcome, pixels) = harness.frame(Some(&frame), RenderViewport::new(0, 0, 0, 0));
    assert_eq!(outcome, SubtitleOverlayOutcome::NothingToDraw);
    assert_eq!(pixels, video_only);
}

#[test]
fn layer_offset_shifts_pixels_onto_the_bar_by_rewriting_only_the_uniform() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let overlay = rasterize(&mut rasterizer(), 1, Some("Hello"), canvas);
    let with_offset = |layer_offset| SubtitleOverlayFrame {
        overlay: &overlay,
        picture_rect: rect,
        layer_offsets: SubtitleLayerOffsets::uniform(layer_offset),
    };
    let (_, video_only) = harness.frame(None, full_view());
    let (_, original) = harness.frame(Some(&with_offset(SubtitleLayerOffset::NONE)), full_view());
    let before = harness.renderer().upload_stats();

    let (shift_x, shift_y) = (-12_i32, 24_i32);
    let (outcome, shifted) = harness.frame(
        Some(&with_offset(SubtitleLayerOffset {
            x: shift_x as f32,
            y: shift_y as f32,
        })),
        full_view(),
    );
    assert!(matches!(outcome, SubtitleOverlayOutcome::Drawn { .. }));

    // Только uniform: маски в атлас заново не грузились.
    let after = harness.renderer().upload_stats();
    assert_eq!(after.atlas_uploads, before.atlas_uploads);
    assert_eq!(after.bitmap_uploads, before.bitmap_uploads);
    assert_eq!(after.uniform_uploads, before.uniform_uploads + 1);

    // Каждый изменённый пиксель исходного кадра переехал ровно на (shift_x, shift_y),
    // а нижняя полоса под картинкой в исходном кадре чистая и в сдвинутом — нет.
    let bar_top = (rect.y + rect.height).round() as u32;
    let mut changed_pixels = 0;
    let mut changed_in_bar_before = 0;
    let mut changed_in_bar_after = 0;
    for y in 0..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            let in_bar = y >= bar_top;
            if pixel(&original, x, y) != pixel(&video_only, x, y) {
                changed_pixels += 1;
                changed_in_bar_before += usize::from(in_bar);
                let (moved_x, moved_y) = (x as i32 + shift_x, y as i32 + shift_y);
                assert_eq!(
                    pixel(&shifted, moved_x as u32, moved_y as u32),
                    pixel(&original, x, y),
                    "pixel ({x},{y}) must land at ({moved_x},{moved_y})"
                );
            }
            changed_in_bar_after +=
                usize::from(in_bar && pixel(&shifted, x, y) != pixel(&video_only, x, y));
        }
    }
    assert!(changed_pixels > 0);
    assert_eq!(
        changed_in_bar_before, 0,
        "default placement stays on the picture"
    );
    assert!(changed_in_bar_after > 0, "shifted layer must reach the bar");
}

#[test]
fn layer_offset_is_still_clipped_by_the_visible_video_area() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let overlay = rasterize(&mut rasterizer(), 1, Some("Hello"), canvas);
    let frame = SubtitleOverlayFrame {
        overlay: &overlay,
        picture_rect: rect,
        layer_offsets: SubtitleLayerOffsets::uniform(SubtitleLayerOffset { x: 0.0, y: 24.0 }),
    };
    let (_, video_only) = harness.frame(None, full_view());
    // Видимая область заканчивается на нижнем крае картинки: бар под ней недоступен.
    let bar_top = (rect.y + rect.height).round() as u32;
    let picture_only = RenderViewport::new(0, 0, TARGET_WIDTH, bar_top);
    let (_, pixels) = harness.frame(Some(&frame), picture_only);
    for y in bar_top..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            assert_eq!(pixel(&pixels, x, y), pixel(&video_only, x, y), "({x},{y})");
        }
    }
}

mod edge_cut;
mod group_offsets;
mod picture;
