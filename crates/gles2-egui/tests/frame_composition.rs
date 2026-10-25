//! Сборка кадра на настоящем EGL: порядок слоёв, пропуск кадра, отказ слоя, альфа
//! непрозрачной цели.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use egui::{Color32, Context, Rect, TextureId, pos2};
use gles2_egui::{
    ActiveFrame, CornerRadius, EguiPainterOptions, FrameCompositor, FrameLayerError,
    FrameLayerStack, FrameLayers, FrameSkipReason, OffscreenAlpha, PixelExtent, UiContent,
    UiFrameError, UiFrameOutcome, UiFrameRequest,
};
use glow::HasContext;
use support::{egui_frame, offscreen_or_skip, pixel, read, screen_for, size, solid_texture};

const TEST_TEXTURE: TextureId = TextureId::Managed(7001);

/// Слои, которые записывают порядок вызовов и рисуют заливками с scissor.
#[derive(Default)]
struct RecordingLayers {
    calls: Vec<&'static str>,
    fail_underlay: bool,
}

/// Залить прямоугольник (координаты сверху слева) цветом, не трогая остальное.
fn fill_rect(frame: &ActiveFrame<'_>, rect: [i32; 4], rgba: [f32; 4]) {
    let target_height = frame.size().height() as i32;
    let [x, y, width, height] = rect;
    let gl = frame.gl();
    // SAFETY: контекст кадра текущий; аргументы — константы GLES 2.0 и размеры внутри цели.
    unsafe {
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(x, target_height - (y + height), width, height);
        gl.clear_color(rgba[0], rgba[1], rgba[2], rgba[3]);
        gl.clear(glow::COLOR_BUFFER_BIT);
        // Слой оставляет scissor включённым: художник обязан задать своё состояние сам.
    }
}

impl FrameLayers for RecordingLayers {
    fn draw_underlay(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        self.calls.push("underlay");
        if self.fail_underlay {
            return Err(FrameLayerError {
                message: "video layer failed".to_owned(),
            });
        }
        let target = frame.size();
        fill_rect(
            frame,
            [0, 0, target.width() as i32, target.height() as i32],
            [0.0, 1.0, 0.0, 1.0],
        );
        Ok(())
    }

    fn draw_overlays(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        self.calls.push("overlays");
        fill_rect(frame, [0, 0, 16, 16], [1.0, 0.0, 0.0, 1.0]);
        Ok(())
    }
}

/// Порядок как у wgpu: фон → подложка (видео) → оверлеи (субтитры) → egui.
#[test]
fn layers_are_drawn_in_wgpu_order_under_egui() {
    let Some(mut context) = offscreen_or_skip(
        "layers_are_drawn_in_wgpu_order_under_egui",
        size(48, 32),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let egui_context = Context::default();
    let (primitives, textures_delta) = egui_frame(&egui_context, [48.0, 32.0], 1.0, |ui| {
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(8.0, 0.0), pos2(24.0, 8.0)),
            0.0,
            Color32::BLUE,
        );
    });
    let screen = screen_for(&context, 1.0);
    let mut layers = RecordingLayers::default();
    let outcome = compositor
        .render(
            &mut context,
            UiFrameRequest {
                screen,
                content: UiContent::Fresh {
                    primitives,
                    textures_delta,
                },
                background: [0.0, 0.0, 0.0, 1.0],
                layers: &mut layers,
                corner_radius: CornerRadius::square(),
            },
        )
        .unwrap();
    assert!(
        matches!(outcome, UiFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    assert_eq!(layers.calls, ["underlay", "overlays"]);
    let pixels = read(&context);
    assert_eq!(
        pixel(&pixels, 4, 12),
        [255, 0, 0, 255],
        "overlay above underlay"
    );
    assert_eq!(
        pixel(&pixels, 12, 4),
        [0, 0, 255, 255],
        "egui above overlay"
    );
    assert_eq!(
        pixel(&pixels, 40, 28),
        [0, 255, 0, 255],
        "underlay above background"
    );
    compositor.destroy(&context).unwrap();
}

/// Отказ слоя — типизированная ошибка; изменения текстур применены, `free` выполнен.
#[test]
fn failed_layer_is_reported_and_texture_changes_still_settle() {
    let Some(mut context) = offscreen_or_skip(
        "failed_layer_is_reported_and_texture_changes_still_settle",
        size(16, 16),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let screen = screen_for(&context, 1.0);
    let mut layers = RecordingLayers {
        fail_underlay: true,
        ..RecordingLayers::default()
    };
    let error = compositor
        .render(
            &mut context,
            UiFrameRequest {
                screen,
                content: UiContent::Fresh {
                    primitives: Vec::new(),
                    textures_delta: solid_texture(TEST_TEXTURE, [2, 2], Color32::RED),
                },
                background: [0.0, 0.0, 0.0, 1.0],
                layers: &mut layers,
                corner_radius: CornerRadius::square(),
            },
        )
        .unwrap_err();
    assert!(matches!(error, UiFrameError::Layer(_)), "{error:?}");
    assert_eq!(
        layers.calls,
        ["underlay"],
        "overlays are not drawn after a failed underlay"
    );
    assert_eq!(
        compositor.painter().texture_count(),
        1,
        "set applied before the failure"
    );

    let mut delta = egui::TexturesDelta::default();
    delta.free.insert(TEST_TEXTURE);
    let error = compositor
        .render(
            &mut context,
            UiFrameRequest {
                screen,
                content: UiContent::Fresh {
                    primitives: Vec::new(),
                    textures_delta: delta,
                },
                background: [0.0, 0.0, 0.0, 1.0],
                layers: &mut layers,
                corner_radius: CornerRadius::square(),
            },
        )
        .unwrap_err();
    assert!(matches!(error, UiFrameError::Layer(_)), "{error:?}");
    assert_eq!(
        compositor.painter().texture_count(),
        0,
        "free applied after the failure"
    );
    compositor.destroy(&context).unwrap();
}

/// Нет кадра (0×0): изменения текстур применены, `free` — сразу, удержанного кадра нет.
#[test]
fn skipped_frame_applies_texture_changes_and_frees() {
    let Some(mut context) = offscreen_or_skip(
        "skipped_frame_applies_texture_changes_and_frees",
        size(16, 16),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let screen = screen_for(&context, 1.0);
    context
        .resize(PixelExtent {
            width: 0,
            height: 0,
        })
        .unwrap();
    let mut render_fresh = |context: &mut gles2_egui::Gles2Context, delta| {
        compositor
            .render(
                context,
                UiFrameRequest {
                    screen,
                    content: UiContent::Fresh {
                        primitives: Vec::new(),
                        textures_delta: delta,
                    },
                    background: [0.0, 0.0, 0.0, 1.0],
                    layers: &mut gles2_egui::NoFrameLayers,
                    corner_radius: CornerRadius::square(),
                },
            )
            .unwrap()
    };
    let outcome = render_fresh(
        &mut context,
        solid_texture(TEST_TEXTURE, [2, 2], Color32::RED),
    );
    assert_eq!(outcome, UiFrameOutcome::Skipped(FrameSkipReason::ZeroSize));
    let mut delta = egui::TexturesDelta::default();
    delta.free.insert(TEST_TEXTURE);
    let outcome = render_fresh(&mut context, delta);
    assert_eq!(outcome, UiFrameOutcome::Skipped(FrameSkipReason::ZeroSize));
    assert_eq!(
        compositor.painter().texture_count(),
        0,
        "free of a skipped frame is immediate"
    );
    assert!(
        !compositor.can_reuse_retained_ui(screen),
        "skipped frame is not retained"
    );
    compositor.destroy(&context).unwrap();
}

/// Непрозрачная цель: альфа кадра 1 даже при прозрачной заливке (иначе композитор просветит
/// окно с альфа-каналом); прозрачная цель сохраняет прозрачность.
#[test]
fn opaque_target_always_gets_alpha_one() {
    for (alpha, expected_alpha) in [
        (OffscreenAlpha::Opaque, 255),
        (OffscreenAlpha::PremultipliedLayer, 0),
    ] {
        let Some(mut context) =
            offscreen_or_skip("opaque_target_always_gets_alpha_one", size(16, 16), alpha)
        else {
            return;
        };
        let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
        let egui_context = Context::default();
        let (primitives, textures_delta) = egui_frame(&egui_context, [16.0, 16.0], 1.0, |ui| {
            ui.painter().rect_filled(
                Rect::from_min_max(pos2(0.0, 0.0), pos2(8.0, 8.0)),
                0.0,
                Color32::from_white_alpha(128),
            );
        });
        let screen = screen_for(&context, 1.0);
        compositor
            .render(
                &mut context,
                UiFrameRequest {
                    screen,
                    content: UiContent::Fresh {
                        primitives,
                        textures_delta,
                    },
                    background: [0.0, 0.0, 0.0, 0.0],
                    layers: &mut gles2_egui::NoFrameLayers,
                    corner_radius: CornerRadius::square(),
                },
            )
            .unwrap();
        let pixels = read(&context);
        assert_eq!(
            pixel(&pixels, 12, 12)[3],
            expected_alpha,
            "{alpha:?} background alpha"
        );
        // Полупрозрачный egui над непрозрачным фоном альфу не опускает.
        if alpha == OffscreenAlpha::Opaque {
            assert_eq!(pixel(&pixels, 4, 4)[3], 255);
        } else {
            assert_eq!(pixel(&pixels, 4, 4)[3], 128);
        }
        compositor.destroy(&context).unwrap();
    }
}

/// Стек слоёв: подложка от одного владельца, оверлеи от другого; чужие методы не вызываются.
#[test]
fn layer_stack_takes_the_underlay_from_one_layer_and_overlays_from_another() {
    let Some(mut context) = offscreen_or_skip(
        "layer_stack_takes_the_underlay_from_one_layer",
        size(48, 32),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let egui_context = Context::default();
    let (primitives, textures_delta) = egui_frame(&egui_context, [48.0, 32.0], 1.0, |_| {});
    let screen = screen_for(&context, 1.0);
    let mut video = RecordingLayers::default();
    let mut subtitles = RecordingLayers::default();
    let mut stack = FrameLayerStack::new(&mut video, &mut subtitles);
    let outcome = compositor
        .render(
            &mut context,
            UiFrameRequest {
                screen,
                content: UiContent::Fresh {
                    primitives,
                    textures_delta,
                },
                background: [0.0, 0.0, 0.0, 1.0],
                layers: &mut stack,
                corner_radius: CornerRadius::square(),
            },
        )
        .unwrap();
    assert!(
        matches!(outcome, UiFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    assert_eq!(video.calls, ["underlay"], "у видео — только подложка");
    assert_eq!(
        subtitles.calls,
        ["overlays"],
        "у субтитров — только оверлеи"
    );
    let pixels = read(&context);
    assert_eq!(
        pixel(&pixels, 4, 4),
        [255, 0, 0, 255],
        "оверлей над подложкой"
    );
    assert_eq!(
        pixel(&pixels, 30, 20),
        [0, 255, 0, 255],
        "подложка вне оверлея"
    );
    compositor.destroy(&context).unwrap();
}
