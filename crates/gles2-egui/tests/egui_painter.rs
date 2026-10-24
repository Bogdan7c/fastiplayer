//! Художник egui на настоящем EGL без окна: пиксели, текстуры, повтор, отсечение, батчи `u16`.
//!
//! Каждый тест проверяет результат (пиксели readback, исходы кадра, счётчики), а не вызов.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use egui::epaint::{ClippedPrimitive, Primitive, Vertex};
use egui::{Color32, Context, Mesh, Rect, TextureId, pos2};
use gles2_egui::{
    CornerRadius, EguiPainterOptions, FrameCompositor, IndexWidthPreference, OffscreenAlpha,
    PainterError, RetainedUiUnavailable, TextureFreeOutcome, UiContent, UiFrameDropReason,
    UiFrameError, UiFrameOutcome, UiScreen, VertexArrayPreference,
};
use support::{
    assert_close, egui_frame, offscreen_or_skip, patch_texture, pixel, read, render, screen_for,
    size, solid_texture, textured_rect,
};

const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE_PIXEL: [u8; 4] = [0, 0, 255, 255];
const GREEN_PIXEL: [u8; 4] = [0, 255, 0, 255];
const TEST_TEXTURE: TextureId = TextureId::Managed(7001);

fn presented(outcome: UiFrameOutcome) -> gles2_egui::PresentedUiFrame {
    match outcome {
        UiFrameOutcome::Presented(presented) => presented,
        other => panic!("frame must be presented, got {other:?}"),
    }
}

fn fresh(primitives: Vec<ClippedPrimitive>, textures_delta: egui::TexturesDelta) -> UiContent {
    UiContent::Fresh {
        primitives,
        textures_delta,
    }
}

#[test]
fn fresh_frame_draws_opaque_and_premultiplied_blended_shapes_and_text() {
    let Some(mut context) = offscreen_or_skip(
        "fresh_frame_draws_opaque_and_premultiplied_blended_shapes_and_text",
        size(96, 48),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let egui_context = Context::default();
    let (primitives, textures_delta) = egui_frame(&egui_context, [96.0, 48.0], 1.0, |ui| {
        let painter = ui.painter();
        painter.rect_filled(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(16.0, 16.0)),
            0.0,
            Color32::RED,
        );
        // Premultiplied белый с альфой 128: поверх синего = 128 + 255·(1 − 128/255) = 255 в B.
        painter.rect_filled(
            Rect::from_min_max(pos2(32.0, 0.0), pos2(48.0, 16.0)),
            0.0,
            Color32::from_white_alpha(128),
        );
        painter.text(
            pos2(4.0, 24.0),
            egui::Align2::LEFT_TOP,
            "Fastiplayer",
            egui::FontId::proportional(16.0),
            Color32::WHITE,
        );
    });
    let screen = screen_for(&context, 1.0);
    let frame = presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(primitives, textures_delta),
        BLUE,
        CornerRadius::square(),
    ));

    let pixels = read(&context);
    assert_close(pixel(&pixels, 8, 8), [255, 0, 0, 255], 0, "opaque red");
    assert_close(
        pixel(&pixels, 40, 8),
        [128, 128, 255, 255],
        1,
        "white α128 over blue",
    );
    assert_eq!(pixel(&pixels, 70, 8), BLUE_PIXEL, "background");
    // Текст: шрифтовой атлас загружен и действительно дал светлые пиксели в строке текста.
    let textures = frame.work.textures.expect("fresh frame applies textures");
    assert!(
        textures.full_uploads >= 1,
        "font atlas upload: {textures:?}"
    );
    let bright_text_pixels = (0..90)
        .flat_map(|x| (24..44).map(move |y| (x, y)))
        .filter(|&(x, y)| pixel(&pixels, x, y)[0] > 200)
        .count();
    assert!(bright_text_pixels > 20, "text pixels: {bright_text_pixels}");
    compositor.destroy(&context).unwrap();
}

#[test]
fn partial_texture_update_changes_only_the_patch() {
    let Some(mut context) = offscreen_or_skip(
        "partial_texture_update_changes_only_the_patch",
        size(32, 16),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let screen = screen_for(&context, 1.0);
    // Текстура 4×4 на квадрат 16×16: один тексель = 4×4 пикселя (NEAREST).
    let rect = || {
        vec![textured_rect(
            TEST_TEXTURE,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(16.0, 16.0)),
        )]
    };
    presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(rect(), solid_texture(TEST_TEXTURE, [4, 4], Color32::RED)),
        BLUE,
        CornerRadius::square(),
    ));
    assert_eq!(pixel(&read(&context), 6, 6), [255, 0, 0, 255]);

    let frame = presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(
            rect(),
            patch_texture(TEST_TEXTURE, [1, 1], [2, 2], Color32::GREEN),
        ),
        BLUE,
        CornerRadius::square(),
    ));
    let textures = frame.work.textures.unwrap();
    assert_eq!((textures.full_uploads, textures.patch_uploads), (0, 1));
    let pixels = read(&context);
    assert_eq!(
        pixel(&pixels, 6, 6),
        GREEN_PIXEL,
        "inside patch (texel 1,1)"
    );
    assert_eq!(
        pixel(&pixels, 10, 10),
        GREEN_PIXEL,
        "inside patch (texel 2,2)"
    );
    assert_eq!(
        pixel(&pixels, 2, 2),
        [255, 0, 0, 255],
        "texel 0,0 untouched"
    );
    assert_eq!(
        pixel(&pixels, 14, 14),
        [255, 0, 0, 255],
        "texel 3,3 untouched"
    );
    assert_eq!(pixel(&pixels, 24, 8), BLUE_PIXEL, "background");
    compositor.destroy(&context).unwrap();
}

/// Наследство 02a (conformance 10): частичная правка неизвестной текстуры — `Dropped`, не
/// паника; остальные изменения кадра применены, `free` отклонённого кадра — сразу.
#[test]
fn unknown_texture_patch_drops_frame_but_other_texture_changes_apply() {
    let Some(mut context) = offscreen_or_skip(
        "unknown_texture_patch_drops_frame_but_other_texture_changes_apply",
        size(16, 16),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let screen = screen_for(&context, 1.0);
    let unknown = TextureId::Managed(7002);

    let mut delta = patch_texture(unknown, [1, 1], [2, 2], Color32::GREEN);
    delta.append(solid_texture(TEST_TEXTURE, [4, 4], Color32::RED));
    let outcome = render(
        &mut compositor,
        &mut context,
        screen,
        fresh(Vec::new(), delta),
        BLUE,
        CornerRadius::square(),
    );
    assert_eq!(
        outcome,
        UiFrameOutcome::Dropped(UiFrameDropReason::UiTexturesOutOfSync {
            texture_id: unknown
        })
    );
    assert_eq!(
        compositor.painter().texture_count(),
        1,
        "full upload of the known texture applied"
    );

    // Правка за границами существующей текстуры — тоже рассинхрон.
    let outcome = render(
        &mut compositor,
        &mut context,
        screen,
        fresh(
            Vec::new(),
            patch_texture(TEST_TEXTURE, [3, 3], [2, 2], Color32::GREEN),
        ),
        BLUE,
        CornerRadius::square(),
    );
    assert_eq!(
        outcome,
        UiFrameOutcome::Dropped(UiFrameDropReason::UiTexturesOutOfSync {
            texture_id: TEST_TEXTURE
        })
    );

    // Отклонённый кадр с `free`: текстура удаляется сразу (conformance 11, вторая половина).
    let mut delta = patch_texture(unknown, [0, 0], [1, 1], Color32::GREEN);
    delta.free.insert(TEST_TEXTURE);
    let outcome = render(
        &mut compositor,
        &mut context,
        screen,
        fresh(Vec::new(), delta),
        BLUE,
        CornerRadius::square(),
    );
    assert!(matches!(
        outcome,
        UiFrameOutcome::Dropped(UiFrameDropReason::UiTexturesOutOfSync { .. })
    ));
    assert_eq!(compositor.painter().texture_count(), 0);
    compositor.destroy(&context).unwrap();
}

/// `free` — после показа: кадр ещё рисует текстуру, следующий её уже не видит.
#[test]
fn texture_is_freed_after_presenting_the_frame_that_uses_it() {
    let Some(mut context) = offscreen_or_skip(
        "texture_is_freed_after_presenting_the_frame_that_uses_it",
        size(32, 16),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let screen = screen_for(&context, 1.0);
    let rect = || {
        vec![textured_rect(
            TEST_TEXTURE,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(16.0, 16.0)),
        )]
    };
    let mut delta = solid_texture(TEST_TEXTURE, [4, 4], Color32::RED);
    delta.free.insert(TEST_TEXTURE);
    let frame = presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(rect(), delta),
        BLUE,
        CornerRadius::square(),
    ));
    assert_eq!(
        frame.work.texture_free,
        TextureFreeOutcome::Applied { deleted: 1 }
    );
    assert_eq!(frame.work.paint.draw_calls, 1);
    assert_eq!(
        pixel(&read(&context), 8, 8),
        [255, 0, 0, 255],
        "drawn before free"
    );
    assert_eq!(compositor.painter().texture_count(), 0);

    let frame = presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(rect(), egui::TexturesDelta::default()),
        BLUE,
        CornerRadius::square(),
    ));
    assert_eq!(frame.work.paint.skipped_missing_texture, 1);
    assert_eq!(frame.work.paint.draw_calls, 0);
    assert_eq!(
        pixel(&read(&context), 8, 8),
        BLUE_PIXEL,
        "freed texture is not drawn"
    );
    compositor.destroy(&context).unwrap();
}

/// Повтор UI поверх нового фона: пиксели UI те же, что у полного прохода, а загрузок нет.
#[test]
fn reused_frame_draws_identical_ui_over_new_background_without_uploads() {
    let Some(mut context) = offscreen_or_skip(
        "reused_frame_draws_identical_ui_over_new_background_without_uploads",
        size(96, 48),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let egui_context = Context::default();
    let (primitives, textures_delta) = egui_frame(&egui_context, [96.0, 48.0], 1.0, |ui| {
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(40.0, 20.0)),
            6.0,
            Color32::RED,
        );
        ui.painter().text(
            pos2(2.0, 24.0),
            egui::Align2::LEFT_TOP,
            "Retained",
            egui::FontId::proportional(14.0),
            Color32::WHITE,
        );
    });
    let screen = screen_for(&context, 1.0);
    let fresh_frame = presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(primitives, textures_delta),
        BLUE,
        CornerRadius::square(),
    ));
    let fresh_pixels = read(&context);
    let stats_after_fresh = compositor.painter().stats();
    assert!(compositor.can_reuse_retained_ui(screen));

    let reused_frame = presented(render(
        &mut compositor,
        &mut context,
        screen,
        UiContent::ReusePrevious,
        GREEN,
        CornerRadius::square(),
    ));
    let reused_pixels = read(&context);
    let stats_after_reuse = compositor.painter().stats();

    assert_eq!(reused_frame.work.prepare, None);
    assert_eq!(reused_frame.work.textures, None);
    assert_eq!(
        reused_frame.work.paint.draw_calls,
        fresh_frame.work.paint.draw_calls
    );
    assert_eq!(
        stats_after_reuse.buffer_streams, stats_after_fresh.buffer_streams,
        "no buffer uploads"
    );
    assert_eq!(
        stats_after_reuse.texture_full_uploads,
        stats_after_fresh.texture_full_uploads
    );
    assert_eq!(
        stats_after_reuse.texture_patch_uploads,
        stats_after_fresh.texture_patch_uploads
    );
    assert_eq!(stats_after_reuse.paints, stats_after_fresh.paints + 1);

    // Непрозрачные пиксели UI совпадают побитово, фон сменился.
    assert_eq!(pixel(&fresh_pixels, 20, 10), [255, 0, 0, 255]);
    assert_eq!(pixel(&reused_pixels, 20, 10), [255, 0, 0, 255]);
    assert_eq!(pixel(&fresh_pixels, 80, 10), BLUE_PIXEL);
    assert_eq!(pixel(&reused_pixels, 80, 10), GREEN_PIXEL);
    // Белый текст: везде, где у полного прохода текст непрозрачен, повтор даёт тот же цвет.
    let mut compared_text_pixels = 0;
    for y in 24..44 {
        for x in 0..90 {
            let fresh_pixel = pixel(&fresh_pixels, x, y);
            if fresh_pixel == [255, 255, 255, 255] {
                assert_eq!(
                    pixel(&reused_pixels, x, y),
                    fresh_pixel,
                    "text pixel ({x}, {y})"
                );
                compared_text_pixels += 1;
            }
        }
    }
    assert!(
        compared_text_pixels > 0,
        "the frame must contain opaque text pixels"
    );
    compositor.destroy(&context).unwrap();
}

#[test]
fn reuse_is_refused_without_retained_frame_or_on_another_screen() {
    let Some(mut context) = offscreen_or_skip(
        "reuse_is_refused_without_retained_frame_or_on_another_screen",
        size(32, 16),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let screen = screen_for(&context, 1.0);
    let outcome = render(
        &mut compositor,
        &mut context,
        screen,
        UiContent::ReusePrevious,
        BLUE,
        CornerRadius::square(),
    );
    assert_eq!(
        outcome,
        UiFrameOutcome::Dropped(UiFrameDropReason::RetainedUiUnavailable(
            RetainedUiUnavailable::NothingRetained
        ))
    );
    presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(Vec::new(), egui::TexturesDelta::default()),
        BLUE,
        CornerRadius::square(),
    ));
    for other_screen in [
        UiScreen {
            pixels_per_point: 2.0,
            ..screen
        },
        UiScreen {
            size_in_pixels: [33, 16],
            ..screen
        },
    ] {
        assert!(!compositor.can_reuse_retained_ui(other_screen));
        let outcome = render(
            &mut compositor,
            &mut context,
            other_screen,
            UiContent::ReusePrevious,
            BLUE,
            CornerRadius::square(),
        );
        assert_eq!(
            outcome,
            UiFrameOutcome::Dropped(UiFrameDropReason::RetainedUiUnavailable(
                RetainedUiUnavailable::ScreenMismatch
            ))
        );
    }
    compositor.destroy(&context).unwrap();
}

/// Отсечение: ось Y у `glScissor` снизу — верхняя половина остаётся верхней.
#[test]
fn clip_rect_limits_drawing_to_the_top_half() {
    let Some(mut context) = offscreen_or_skip(
        "clip_rect_limits_drawing_to_the_top_half",
        size(32, 16),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let egui_context = Context::default();
    let (primitives, textures_delta) = egui_frame(&egui_context, [32.0, 16.0], 1.0, |ui| {
        let clipped = ui
            .painter()
            .with_clip_rect(Rect::from_min_max(pos2(0.0, 0.0), pos2(20.0, 8.0)));
        clipped.rect_filled(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(32.0, 16.0)),
            0.0,
            Color32::RED,
        );
    });
    let screen = screen_for(&context, 1.0);
    presented(render(
        &mut compositor,
        &mut context,
        screen,
        fresh(primitives, textures_delta),
        BLUE,
        CornerRadius::square(),
    ));
    let pixels = read(&context);
    assert_eq!(pixel(&pixels, 10, 2), [255, 0, 0, 255], "inside clip (top)");
    assert_eq!(pixel(&pixels, 10, 7), [255, 0, 0, 255], "last clipped row");
    assert_eq!(pixel(&pixels, 10, 8), BLUE_PIXEL, "below clip");
    assert_eq!(pixel(&pixels, 10, 14), BLUE_PIXEL, "bottom");
    assert_eq!(pixel(&pixels, 25, 2), BLUE_PIXEL, "right of clip");
    compositor.destroy(&context).unwrap();
}

/// Меш больше 65 535 вершин: путь `u16`-батчей без VAO рисует то же, что `u32` с VAO.
#[test]
fn large_mesh_draws_identically_with_u16_batches_and_without_vao() {
    let Some(mut context) = offscreen_or_skip(
        "large_mesh_draws_identically_with_u16_batches_and_without_vao",
        size(128, 136),
        OffscreenAlpha::Opaque,
    ) else {
        return;
    };
    let fastest = EguiPainterOptions::default();
    let fallback = EguiPainterOptions {
        index_width: IndexWidthPreference::U16Batches,
        vertex_array: VertexArrayPreference::ExplicitAttributes,
        ..EguiPainterOptions::default()
    };
    let screen = screen_for(&context, 1.0);
    let mut readbacks = Vec::new();
    for options in [fastest, fallback] {
        let mut compositor = FrameCompositor::new(&context, options).unwrap();
        let frame = presented(render(
            &mut compositor,
            &mut context,
            screen,
            fresh(
                vec![checker_mesh(17_000, 128)],
                solid_texture(TextureId::Managed(0), [1, 1], Color32::WHITE),
            ),
            BLUE,
            CornerRadius::square(),
        ));
        let prepare = frame.work.prepare.unwrap();
        if options == fallback {
            assert!(!compositor.painter().uses_u32_indices());
            assert!(!compositor.painter().uses_vertex_array());
            assert!(
                prepare.draw_commands >= 2,
                "68 000 vertices need ≥ 2 u16 batches: {prepare:?}"
            );
        }
        if compositor.painter().uses_u32_indices() {
            assert_eq!(
                prepare.vertex_bytes,
                17_000 * 4 * 20,
                "u32: every vertex once"
            );
        } else {
            // `split_to_u16` копирует вершины, общие для треугольников по обе стороны границы
            // батча, поэтому байт чуть больше.
            assert!(prepare.vertex_bytes >= 17_000 * 4 * 20, "{prepare:?}");
        }
        readbacks.push(read(&context));
        compositor.destroy(&context).unwrap();
    }
    assert_eq!(
        readbacks[0], readbacks[1],
        "u16/explicit path must match u32/VAO path"
    );
    // Квадрат 16 999 (последний) лежит в строке 132, столбце 103: нечётный → зелёный.
    assert_eq!(pixel(&readbacks[1], 103, 132), GREEN_PIXEL);
    assert_eq!(pixel(&readbacks[1], 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&readbacks[1], 120, 135), BLUE_PIXEL, "no quad there");
}

/// `count` квадратов 1×1 px построчно по `columns`; чётные красные, нечётные зелёные. Меш
/// ссылается на `Managed(0)`, кадр загружает под этот id белую текстуру 1×1.
fn checker_mesh(count: u32, columns: u32) -> ClippedPrimitive {
    let mut mesh = Mesh::with_texture(TextureId::Managed(0));
    for index in 0..count {
        let x = (index % columns) as f32;
        let y = (index / columns) as f32;
        let color = if index % 2 == 0 {
            Color32::RED
        } else {
            Color32::GREEN
        };
        let first = mesh.vertices.len() as u32;
        for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            mesh.vertices.push(Vertex {
                pos: pos2(x + dx, y + dy),
                uv: pos2(0.0, 0.0),
                color,
            });
        }
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first + 2, first + 1, first + 3);
    }
    ClippedPrimitive {
        clip_rect: Rect::EVERYTHING,
        primitive: Primitive::Mesh(mesh),
    }
}

/// GL-имена одного контекста в другом ничего не значат: типизированный отказ.
#[test]
fn compositor_refuses_a_foreign_context() {
    let (Some(context_a), Some(mut context_b)) = (
        offscreen_or_skip(
            "compositor_refuses_a_foreign_context",
            size(8, 8),
            OffscreenAlpha::Opaque,
        ),
        offscreen_or_skip(
            "compositor_refuses_a_foreign_context",
            size(8, 8),
            OffscreenAlpha::Opaque,
        ),
    ) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context_a, EguiPainterOptions::default()).unwrap();
    let screen = screen_for(&context_b, 1.0);
    let error = compositor
        .render(
            &mut context_b,
            gles2_egui::UiFrameRequest {
                screen,
                content: fresh(
                    Vec::new(),
                    solid_texture(TEST_TEXTURE, [2, 2], Color32::RED),
                ),
                background: BLUE,
                layers: &mut gles2_egui::NoFrameLayers,
                corner_radius: CornerRadius::square(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(error, UiFrameError::Painter(PainterError::ForeignContext)),
        "{error:?}"
    );
    compositor.destroy(&context_a).unwrap();
}
