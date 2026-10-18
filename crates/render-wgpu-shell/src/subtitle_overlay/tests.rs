//! Функциональные тесты overlay-а: настоящий текст -> растеризатор -> GPU pass -> пиксели.
//!
//! Тесты пропускаются (с сообщением), если на машине нет GPU adapter-а, как и тесты угловой маски.

use std::sync::mpsc;
use std::time::Duration;

use render_core::{PictureLayout, PictureSize, RenderViewport};
use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};
use subtitle_raster::{
    AssRenderOptions, CanvasSize, CueKey, CueSourceId, FontLookupCacheLimits, RasterCacheLimits,
    RasterInputLimits, RasterLimits, RenderRequest, RenderableCue, SubtitleAppearance,
    SubtitleFontLibrary, SubtitleOverlay, SubtitleRasterizer, TextLanguagePreference,
};

use super::*;

const TARGET_WIDTH: u32 = 256;
const TARGET_HEIGHT: u32 = 144;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const GPU_TEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Допуск на округление GPU (unorm8) и целочисленного эталона растеризатора.
const BLEND_TOLERANCE: i32 = 3;

/// Headless GPU + целевая текстура + рендерер под тестом.
struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    renderer: SubtitleOverlayRenderer,
}

impl Harness {
    /// `None`, если нет GPU adapter-а.
    fn new() -> Option<Self> {
        Self::with_texture_limit(None)
    }

    /// То же, но с искусственно сниженным лимитом стороны текстуры (чтобы проверять поведение
    /// на границе лимита GPU без огромных картинок); лимит не меньше размера цели.
    fn with_texture_limit(max_texture_side: Option<u32>) -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .ok()?;
        let mut required_limits = wgpu::Limits::default().using_resolution(adapter.limits());
        if let Some(side) = max_texture_side {
            required_limits.max_texture_dimension_2d = side;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("subtitle overlay test device"),
            required_features: wgpu::Features::empty(),
            required_limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("create subtitle overlay test device");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("subtitle overlay test target"),
            size: wgpu::Extent3d {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let renderer = SubtitleOverlayRenderer::new(&device, TARGET_FORMAT);
        Some(Self {
            device,
            queue,
            texture,
            view,
            renderer,
        })
    }

    /// Один кадр: очистка «видео» известным цветом, overlay, readback RGBA.
    fn frame(
        &mut self,
        overlay_frame: Option<&SubtitleOverlayFrame<'_>>,
        visible_area: RenderViewport,
    ) -> (SubtitleOverlayOutcome, Vec<u8>) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("subtitle overlay test encoder"),
            });
        // Пустой pass с Clear имитирует video pass (альфа 1.0, как у настоящего видео).
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("subtitle overlay test video"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.25,
                        g: 0.5,
                        b: 0.75,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        let outcome = self.renderer.render(
            SubtitleOverlayTarget {
                device: &self.device,
                queue: &self.queue,
                encoder: &mut encoder,
                target: &self.view,
                screen_size: [TARGET_WIDTH, TARGET_HEIGHT],
                visible_area,
            },
            overlay_frame,
        );
        (outcome, self.read_back(encoder))
    }

    fn read_back(&self, mut encoder: wgpu::CommandEncoder) -> Vec<u8> {
        let bytes_per_row = TARGET_WIDTH * 4;
        let padded = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("subtitle overlay test readback"),
            size: u64::from(padded) * u64::from(TARGET_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(TARGET_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("send readback result");
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_TEST_TIMEOUT),
            })
            .expect("wait for GPU");
        receiver
            .recv_timeout(GPU_TEST_TIMEOUT)
            .expect("readback callback")
            .expect("map readback");
        let mapped = slice.get_mapped_range().expect("mapped range");
        let pixels = mapped
            .chunks_exact(padded as usize)
            .flat_map(|row| row[..bytes_per_row as usize].iter().copied())
            .collect::<Vec<_>>();
        drop(mapped);
        readback.unmap();
        pixels
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
                eprintln!("skip: no GPU adapter for subtitle overlay test");
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
    let after_first = harness.renderer.upload_stats();
    assert_eq!(after_first.atlas_uploads, 1);
    assert_eq!(after_first.atlas_recreations, 1);
    assert_eq!(after_first.bitmap_uploads, hello.bitmaps().len() as u64);
    assert_eq!(after_first.uniform_uploads, 1);

    let (_, second) = harness.frame(Some(&frame_of(&hello_again)), full_view());
    assert_eq!(
        harness.renderer.upload_stats(),
        after_first,
        "same revision: zero uploads"
    );
    assert_eq!(first, second, "same revision draws the same pixels");

    let world = rasterize(&mut rasterizer, 2, Some("World, and more text"), canvas);
    assert_ne!(world.revision(), hello.revision());
    let (_, third) = harness.frame(Some(&frame_of(&world)), full_view());
    let stats = harness.renderer.upload_stats();
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
    assert_eq!(harness.renderer.upload_stats().atlas_uploads, 2);
    assert_ne!(pixels_first, pixels_second);
}

#[test]
fn missing_or_empty_overlay_draws_nothing_and_touches_no_counters() {
    let mut harness = gpu_or_skip!();
    let (rect, canvas) = letterboxed_picture();
    let (_, video_only) = harness.frame(None, full_view());
    assert_eq!(
        harness.renderer.upload_stats(),
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
        harness.renderer.upload_stats(),
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
    let stats = harness.renderer.upload_stats();
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
    let before = harness.renderer.upload_stats();

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
    let after = harness.renderer.upload_stats();
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
