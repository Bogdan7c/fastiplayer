//! Частичный показ с настоящими слоями: видео (SW YUV), субтитры (bitmap-оверлей) и egui.
//!
//! Эталон — контекст, который каждый кадр перерисовывает целиком. Тестовый контекст имеет кольцо
//! задних буферов и сообщает возраст как EGL. Каждый кадр сравнивается побайтно. Доказательство
//! выигрыша — счётчик `shaded_video_pixels` видеослоя: на кадрах только с интерфейсом шейдер
//! видео обрабатывает заметно меньше пикселей, чем при полной перерисовке.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use std::sync::Arc;

use egui::{Color32, Context, Rect, pos2};
use gles2_egui::{
    ActiveFrame, EguiPainterOptions, FrameCompositor, FrameLayerError, FrameLayerStack,
    FrameLayers, Gles2Context, LayersDamage, PixelExtent, RepaintPlan, UiContent, UiFrameOutcome,
    UiFrameRequest, UiScreen,
};
use gles2_video::{
    BitmapOverlayRenderer, ColorAdjustment, OverlayDrawRequest, OverlayLayerOptions,
    PictureMapping, PixelRect, QuarterTurn, VideoColorConversion, VideoDrawRequest,
    VideoLayerOptions, VideoLayerRenderer, VideoUnderlay, YuvColorEncoding, YuvMatrix, YuvRange,
};
use support::overlay::{TestBitmap, origins_at, overlay_renderer, rect, tint};
use support::{FramePattern, OwnedFrame, extent, offscreen_or_skip, size};

const TARGET: (u32, u32) = (192, 128);

/// Что видно под интерфейсом в кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayerScene {
    video_key: u64,
    subtitle: Option<i32>,
    /// Правая граница области видео (letterbox справа залит чёрным).
    viewport_width: u32,
}

#[derive(Debug, Clone, Copy)]
struct Step {
    moving_x: u32,
    tooltip: bool,
    layers: LayerScene,
}

/// Субтитры как слой оверлеев кадра (prepare и draw — внутри кадра).
struct SubtitleLayer<'renderer> {
    renderer: &'renderer mut BitmapOverlayRenderer<TestBitmap>,
    bitmaps: Vec<TestBitmap>,
    clip: PixelRect,
}

impl FrameLayers for SubtitleLayer<'_> {
    fn draw_overlays(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        let fail = |what: &str, error: &dyn std::fmt::Display| FrameLayerError {
            message: format!("{what}: {error}"),
        };
        self.renderer
            .prepare(frame, &self.bitmaps)
            .map_err(|error| fail("overlay prepare", &error))?;
        self.renderer
            .draw(
                frame,
                &OverlayDrawRequest {
                    origins: origins_at(0.0, 0.0),
                    clip: self.clip,
                },
            )
            .map_err(|error| fail("overlay draw", &error))?;
        Ok(())
    }
}

fn scenario() -> Vec<Step> {
    let base = LayerScene {
        video_key: 1,
        subtitle: None,
        viewport_width: 160,
    };
    let mut steps = Vec::new();
    let mut push = |moving_x: u32, tooltip: bool, layers: LayerScene| {
        steps.push(Step {
            moving_x,
            tooltip,
            layers,
        });
    };
    for index in 0..8 {
        push(index * 7, false, base);
    }
    push(56, true, base);
    push(56, true, base);
    push(56, false, base);
    let subtitled = LayerScene {
        subtitle: Some(10),
        ..base
    };
    push(56, false, subtitled);
    push(63, false, subtitled);
    push(70, false, subtitled);
    let next_video = LayerScene {
        video_key: 2,
        ..subtitled
    };
    push(70, false, next_video);
    push(77, false, next_video);
    push(84, true, next_video);
    let narrower = LayerScene {
        viewport_width: 128,
        ..next_video
    };
    push(84, true, narrower);
    push(91, false, narrower);
    push(98, false, narrower);
    steps
}

/// Кадр видео: яркостный градиент по двум осям, ключ сдвигает узор.
fn video_frame(key: u64) -> OwnedFrame {
    let shift = (key as u32) * 40;
    OwnedFrame::from_pattern(
        gles2_video::ChromaSubsampling::Yuv420,
        extent(64, 48),
        0,
        &FramePattern {
            luma: &move |x, y| ((x * 3 + y * 2 + shift) % 220 + 16) as u8,
            chroma_u: &|x, y| (96 + (x * 5 + y) % 64) as u8,
            chroma_v: &|x, y| (96 + (x + y * 5) % 64) as u8,
        },
    )
}

struct Rig {
    context: Gles2Context,
    compositor: FrameCompositor,
    video: VideoLayerRenderer,
    overlay: BitmapOverlayRenderer<TestBitmap>,
}

fn rig_from(context: Gles2Context) -> Rig {
    let compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let video = VideoLayerRenderer::new(&context, VideoLayerOptions::default()).unwrap();
    let overlay = overlay_renderer(&context, OverlayLayerOptions::default());
    Rig {
        context,
        compositor,
        video,
        overlay,
    }
}

struct Run {
    frames: Vec<Vec<u8>>,
    plans: Vec<RepaintPlan>,
    shaded_video_pixels: u64,
}

fn run_scenario(rig: &mut Rig, partial: bool) -> Run {
    let egui_context = Context::default();
    let encoding = YuvColorEncoding {
        matrix: YuvMatrix::Bt709,
        range: YuvRange::Limited,
    };
    let mut previous: Option<LayerScene> = None;
    let mut uploaded_key = None;
    let mut frames = Vec::new();
    let mut plans = Vec::new();
    for step in scenario() {
        if uploaded_key != Some(step.layers.video_key) {
            let frame = video_frame(step.layers.video_key);
            rig.video
                .upload(&rig.context, &frame.upload(step.layers.video_key))
                .unwrap();
            uploaded_key = Some(step.layers.video_key);
        }
        let layers_damage = if partial && previous == Some(step.layers) {
            LayersDamage::Unchanged
        } else {
            LayersDamage::Changed
        };
        previous = Some(step.layers);

        let viewport = PixelRect {
            x: 0,
            y: 0,
            width: step.layers.viewport_width,
            height: TARGET.1,
        };
        let visible = [viewport];
        let mut underlay = VideoUnderlay::new(
            &mut rig.video,
            VideoDrawRequest {
                viewport,
                visible_rects: &visible,
                picture: PictureMapping::FILL,
                orientation: QuarterTurn::Identity,
                color: VideoColorConversion::Sdr {
                    encoding,
                    adjustment: ColorAdjustment::IDENTITY,
                },
            },
        );
        let bitmaps = step
            .layers
            .subtitle
            .map(|x| {
                vec![TestBitmap::solid_coverage(
                    rect(x, 100, 60, 10),
                    200,
                    tint(255, 255, 255, 255),
                )]
            })
            .unwrap_or_default();
        let mut subtitles = SubtitleLayer {
            renderer: &mut rig.overlay,
            bitmaps,
            clip: viewport,
        };
        let mut stack = FrameLayerStack::new(&mut underlay, &mut subtitles);

        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_max(
                pos2(0.0, 0.0),
                pos2(TARGET.0 as f32, TARGET.1 as f32),
            )),
            max_texture_side: Some(4096),
            ..egui::RawInput::default()
        };
        let output = egui_context.run_ui(input, |ui| {
            let painter = ui.painter();
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(4.0 + step.moving_x as f32, 10.0),
                    egui::vec2(12.0, 12.0),
                ),
                2.0,
                Color32::from_rgb(230, 70, 70),
            );
            painter.rect_filled(
                Rect::from_min_max(pos2(0.0, 112.0), pos2(192.0, 128.0)),
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 140),
            );
            if step.tooltip {
                painter.rect_filled(
                    Rect::from_min_max(pos2(40.0, 50.0), pos2(100.0, 70.0)),
                    4.0,
                    Color32::from_rgba_unmultiplied(250, 250, 200, 220),
                );
            }
        });
        let primitives = egui_context.tessellate(output.shapes, output.pixels_per_point);
        let outcome = rig
            .compositor
            .render_with_layers_damage(
                &mut rig.context,
                UiFrameRequest {
                    screen: UiScreen {
                        size_in_pixels: [TARGET.0, TARGET.1],
                        pixels_per_point: 1.0,
                    },
                    content: UiContent::Fresh {
                        primitives,
                        textures_delta: output.textures_delta,
                    },
                    background: [0.0, 0.0, 0.0, 1.0],
                    layers: &mut stack,
                },
                layers_damage,
            )
            .unwrap();
        let UiFrameOutcome::Presented(presented) = outcome else {
            panic!("frame must be presented: {outcome:?}");
        };
        plans.push(presented.repaint);
        rig.video
            .retire_submitted_work(Arc::new(rig.context.insert_fence().unwrap()));
        frames.push(
            rig.context
                .read_offscreen_pixels()
                .unwrap()
                .as_rgba()
                .to_vec(),
        );
    }
    Run {
        frames,
        plans,
        shaded_video_pixels: rig.video.stats().shaded_video_pixels,
    }
}

fn check_ring(back_buffers: usize, name: &str) {
    let Some(reference_context) = offscreen_or_skip(name, size(TARGET.0, TARGET.1)) else {
        return;
    };
    let mut reference_rig = rig_from(reference_context);
    let reference = run_scenario(&mut reference_rig, false);
    let context =
        Gles2Context::offscreen_with_back_buffers(size(TARGET.0, TARGET.1), back_buffers).unwrap();
    let mut rig = rig_from(context);
    let run = run_scenario(&mut rig, true);
    for (index, (got, want)) in run.frames.iter().zip(&reference.frames).enumerate() {
        if got != want {
            let at = got.iter().zip(want).position(|(a, b)| a != b).unwrap();
            panic!(
                "ring of {back_buffers}: frame {index} differs at byte {at} (plan {:?})",
                run.plans[index]
            );
        }
    }
    // Выигрыш доказан счётчиком работы шейдера видео, а не вызовом функции.
    let stats = rig.compositor.partial_present_stats();
    assert!(stats.partial_presents > 3, "{stats:?}");
    if back_buffers <= 2 {
        assert!(
            run.shaded_video_pixels * 10 < reference.shaded_video_pixels * 9,
            "ring of {back_buffers}: video shader work {} vs full {}",
            run.shaded_video_pixels,
            reference.shaded_video_pixels
        );
    }
    let _ = PixelExtent {
        width: TARGET.0,
        height: TARGET.1,
    };
}

#[test]
fn video_subtitles_and_ui_match_full_repaint_with_one_back_buffer() {
    check_ring(1, "video_layers_one_back_buffer");
}

#[test]
fn video_subtitles_and_ui_match_full_repaint_with_two_back_buffers() {
    check_ring(2, "video_layers_two_back_buffers");
}

#[test]
fn video_subtitles_and_ui_match_full_repaint_with_three_back_buffers() {
    check_ring(3, "video_layers_three_back_buffers");
}
