//! Паритет субтитров wgpu ↔ GLES2 (сессия 08): один и тот же overlay растеризатора рисуют
//! настоящий wgpu-проход шелла (`SubtitleOverlayRenderer`, стенд его же GPU-тестов) и адаптер
//! GLES2 (`render_gles2_shell::Gles2SubtitleOverlayRenderer` + слой оверлеев `gles2-video`).
//!
//! Сцены — входы GPU-тестов wgpu: текст на нижнем крае letterbox, обрезка видимой областью,
//! сдвиг слоя на полосу, независимые группы ASS, обрез по краю холста, PGS-картинки поверх
//! текста. Пиксели сравниваются целиком с допуском ±2 на канал (округление unorm8 смешивания в
//! разных API); каждая сцена обязана реально менять пиксели «видео».

use gles2_egui::{FrameStart, Gles2Context, OffscreenAlpha};
use gles2_video::OverlayLayerOptions;
use render_core::{PictureRect, RenderViewport};
use render_gles2_shell::{Gles2SubtitleOverlayRenderer, SubtitleOverlayOutcome};
use subtitle_raster::{CanvasSize, SubtitleOverlay};

use super::{CHANNEL_TOLERANCE, compare, gles2_or_skip};
use crate::subtitle_overlay::tests::edge_cut::edge_overlay;
use crate::subtitle_overlay::tests::group_offsets::{full_picture, two_group_overlay};
use crate::subtitle_overlay::tests::picture::{render_scene, rgba, solid_cue, target_canvas};
use crate::subtitle_overlay::tests::{
    Harness, TARGET_HEIGHT, TARGET_WIDTH, full_view, letterboxed_picture, rasterize, rasterizer,
};
use render_shell_api::{SubtitleLayerOffset, SubtitleLayerOffsets, SubtitleOverlayFrame};

/// Заливка «видео» обоих стендов.
const VIDEO_COLOR: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

/// Одна сцена паритета.
struct SubtitleScene {
    name: &'static str,
    overlay: SubtitleOverlay,
    picture_rect: PictureRect,
    layer_offsets: SubtitleLayerOffsets,
    visible_area: RenderViewport,
}

impl SubtitleScene {
    fn frame(&self) -> SubtitleOverlayFrame<'_> {
        SubtitleOverlayFrame {
            overlay: &self.overlay,
            picture_rect: self.picture_rect,
            layer_offsets: self.layer_offsets,
        }
    }
}

fn scenes() -> Vec<SubtitleScene> {
    let (letterbox, letterbox_canvas) = letterboxed_picture();
    let hello = |rasterizer: &mut subtitle_raster::SubtitleRasterizer, canvas: CanvasSize| {
        rasterize(rasterizer, 1, Some("Hello"), canvas)
    };
    let mut text_rasterizer = rasterizer();
    let text_on_letterbox = hello(&mut text_rasterizer, letterbox_canvas);
    let mut pgs_rasterizer = rasterizer();
    let semi_transparent = solid_cue((40, 100), (60, 30), rgba(255, 0, 0, 128));
    let opaque = solid_cue((20, 30), (10, 6), rgba(10, 200, 30, 255));
    let pictures_over_text = render_scene(
        &mut pgs_rasterizer,
        target_canvas(),
        Some("Hello"),
        &[(100, &semi_transparent), (101, &opaque)],
    );
    let bar_top = (letterbox.y + letterbox.height).round() as u32;
    vec![
        SubtitleScene {
            name: "text on the bottom of the letterboxed picture",
            overlay: text_on_letterbox.clone(),
            picture_rect: letterbox,
            layer_offsets: SubtitleLayerOffsets::NONE,
            visible_area: full_view(),
        },
        SubtitleScene {
            name: "text clipped by the visible video half",
            overlay: text_on_letterbox.clone(),
            picture_rect: letterbox,
            layer_offsets: SubtitleLayerOffsets::NONE,
            visible_area: RenderViewport::new(0, 0, TARGET_WIDTH / 2, TARGET_HEIGHT),
        },
        SubtitleScene {
            name: "layer shifted onto the bar",
            overlay: text_on_letterbox.clone(),
            picture_rect: letterbox,
            layer_offsets: SubtitleLayerOffsets::uniform(SubtitleLayerOffset { x: -12.0, y: 24.0 }),
            visible_area: full_view(),
        },
        SubtitleScene {
            name: "shifted layer clipped at the picture bottom",
            overlay: text_on_letterbox,
            picture_rect: letterbox,
            // Сдвиг вниз на 10 px: нижняя часть плашки уходит на полосу и срезается (102 → 64 пикселя).
            layer_offsets: SubtitleLayerOffsets::uniform(SubtitleLayerOffset { x: 0.0, y: 10.0 }),
            visible_area: RenderViewport::new(0, 0, TARGET_WIDTH, bar_top),
        },
        SubtitleScene {
            name: "ASS main and fixed groups move independently",
            overlay: two_group_overlay(),
            picture_rect: full_picture(),
            layer_offsets: SubtitleLayerOffsets {
                main: SubtitleLayerOffset { x: 0.0, y: -20.0 },
                fixed: SubtitleLayerOffset { x: 3.0, y: 6.0 },
            },
            visible_area: full_view(),
        },
        SubtitleScene {
            name: "ASS glyphs cut by the canvas edge",
            overlay: edge_overlay(),
            picture_rect: full_picture(),
            layer_offsets: SubtitleLayerOffsets::NONE,
            visible_area: full_view(),
        },
        SubtitleScene {
            name: "PGS pictures over text, semi-transparent and opaque",
            overlay: pictures_over_text,
            picture_rect: full_picture(),
            layer_offsets: SubtitleLayerOffsets::NONE,
            visible_area: full_view(),
        },
    ]
}

/// Кадр GLES2: заливка «видео», субтитры, показ, пиксели.
fn gles2_frame(
    context: &mut Gles2Context,
    renderer: &mut Gles2SubtitleOverlayRenderer,
    overlay_frame: Option<&SubtitleOverlayFrame<'_>>,
    visible_area: RenderViewport,
) -> (SubtitleOverlayOutcome, Vec<u8>) {
    let FrameStart::Ready(frame) = context.begin_frame().expect("GLES2 frame begins") else {
        panic!("GLES2 offscreen frame skipped");
    };
    frame.clear(VIDEO_COLOR);
    let outcome = renderer
        .render(&frame, overlay_frame, visible_area)
        .expect("GLES2 subtitles render");
    frame.present().expect("GLES2 frame presents");
    let pixels = context
        .read_offscreen_pixels()
        .expect("GLES2 readback")
        .as_rgba()
        .to_vec();
    (outcome, pixels)
}

#[test]
fn subtitle_overlays_match_between_wgpu_and_gles2() {
    let test_name = "subtitle_overlays_match_between_wgpu_and_gles2";
    let Some(mut wgpu) = Harness::new() else {
        eprintln!("skip {test_name}: no wgpu adapter");
        return;
    };
    let Some(mut context) = gles2_or_skip(
        test_name,
        [TARGET_WIDTH, TARGET_HEIGHT],
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut gles2 = Gles2SubtitleOverlayRenderer::new(&context, OverlayLayerOptions::default())
        .expect("GLES2 subtitle overlay");
    let (_, wgpu_video) = wgpu.frame(None, full_view());
    let (_, gles2_video) = gles2_frame(&mut context, &mut gles2, None, full_view());
    let video_report = compare(&wgpu_video, &gles2_video, TARGET_WIDTH);
    assert_eq!(
        video_report.max_channel_difference, 0,
        "одинаковая заливка «видео»"
    );

    for scene in scenes() {
        let frame = scene.frame();
        let (wgpu_outcome, wgpu_pixels) = wgpu.frame(Some(&frame), scene.visible_area);
        let (gles2_outcome, gles2_pixels) =
            gles2_frame(&mut context, &mut gles2, Some(&frame), scene.visible_area);
        let crate::SubtitleOverlayOutcome::Drawn { bitmaps } = wgpu_outcome else {
            panic!("{}: wgpu must draw, got {wgpu_outcome:?}", scene.name);
        };
        assert_eq!(
            gles2_outcome,
            SubtitleOverlayOutcome::Drawn { bitmaps },
            "{}: same number of drawn bitmaps",
            scene.name
        );
        let changed = compare(&wgpu_video, &wgpu_pixels, TARGET_WIDTH).pixels_differing;
        let report = compare(&wgpu_pixels, &gles2_pixels, TARGET_WIDTH);
        eprintln!(
            "{test_name} {}: max Δ {}, differing {} / {}, over ±{CHANNEL_TOLERANCE}: {}, \
             subtitle pixels {changed}",
            scene.name,
            report.max_channel_difference,
            report.pixels_differing,
            TARGET_WIDTH * TARGET_HEIGHT,
            report.pixels_over_tolerance,
        );
        assert!(
            changed > 20,
            "{}: the scene must paint subtitles over the video ({changed} pixels)",
            scene.name
        );
        assert_eq!(
            report.pixels_over_tolerance, 0,
            "{}: first pixel over tolerance (x, y, wgpu, gles2) = {:?}",
            scene.name, report.first_over_tolerance
        );
    }
    gles2
        .destroy(&context)
        .expect("destroy GLES2 subtitle overlay");
}
