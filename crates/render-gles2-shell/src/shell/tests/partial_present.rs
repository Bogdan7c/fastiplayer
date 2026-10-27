//! Частичный показ через публичный `RenderShell`: настоящий кадр декодера → видеослой → egui.
//!
//! Эталон — тот же сценарий на рендере без кольца задних буферов (всегда полная перерисовка).
//! Кольцо из 1/2/3 буферов сообщает возраст как EGL. Каждый кадр сравнивается побайтно; выигрыш
//! доказывают счётчики: кадры «только интерфейс» идут частично и перерисовывают малую часть цели.

use egui::{Color32, Context, Pos2, RawInput, Rect, pos2};
use render_core::RenderViewport;
use render_shell_api::{
    EguiFrameContent, RenderFrameInput, RenderFrameOutcome, RenderShell, RenderSurfaceSize,
    RenderableFrameKey, SurfaceFrameAcquire, SurfacePresentSettings,
};

use super::{connect_software_decoder, materialize_key, screen_for};
use crate::Gles2Renderer;
use crate::test_support::fake_decoder::SplitPicture;

const TARGET: [u32; 2] = [160, 112];
const RESIZED: [u32; 2] = [144, 96];

fn picture(handle: u64, left_luma: u8, right_luma: u8) -> SplitPicture {
    SplitPicture {
        handle,
        size: [64, 32],
        left_luma,
        right_luma,
    }
}

/// Один проход egui: прямоугольник на позиции `x` и полупрозрачная панель внизу.
fn fresh_ui(egui_context: &Context, size: [u32; 2], x: f32) -> EguiFrameContent {
    egui_context.set_pixels_per_point(1.0);
    let output = egui_context.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_max(
                Pos2::ZERO,
                pos2(size[0] as f32, size[1] as f32),
            )),
            max_texture_side: Some(4096),
            ..RawInput::default()
        },
        |ui| {
            let painter = ui.painter();
            painter.rect_filled(
                Rect::from_min_max(pos2(x, 8.0), pos2(x + 12.0, 20.0)),
                2.0,
                Color32::from_rgb(230, 70, 70),
            );
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(0.0, size[1] as f32 - 14.0),
                    pos2(size[0] as f32, size[1] as f32),
                ),
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 150),
            );
        },
    );
    EguiFrameContent::Fresh {
        paint_jobs: egui_context.tessellate(output.shapes, output.pixels_per_point),
        textures_delta: output.textures_delta,
    }
}

fn present_with(
    renderer: &mut Gles2Renderer,
    size: [u32; 2],
    key: &RenderableFrameKey,
    viewport: RenderViewport,
    egui: EguiFrameContent,
) -> Vec<u8> {
    let screen = screen_for(size);
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen) else {
        panic!("the surface is expected to be acquired");
    };
    let outcome = renderer.render_frame(RenderFrameInput {
        surface,
        video_frame: Some(key),
        egui,
        screen,
        video_viewport: viewport,
        video_exclusion_rects: Vec::new(),
        subtitle_overlay: None,
    });
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    renderer
        .read_offscreen_pixels()
        .expect("offscreen readback")
        .as_rgba()
        .to_vec()
}

/// Прогнать сценарий и вернуть пиксели каждого кадра.
fn run_scenario(renderer: &mut Gles2Renderer) -> Vec<Vec<u8>> {
    let decoder = connect_software_decoder(renderer);
    let (key_a, _lease_a) = materialize_key(renderer, &decoder, picture(1, 180, 60));
    let egui_context = Context::default();
    let mut frames = Vec::new();
    let full = RenderViewport::full_surface(TARGET[0], TARGET[1]);
    // 1. Видео стоит, двигается только интерфейс (пауза и мышь).
    for index in 0..18 {
        let ui = fresh_ui(&egui_context, TARGET, 4.0 + index as f32 * 7.0);
        frames.push(present_with(renderer, TARGET, &key_a, full, ui));
    }
    // 2. Повтор удержанного интерфейса.
    frames.push(present_with(
        renderer,
        TARGET,
        &key_a,
        full,
        EguiFrameContent::ReusePrevious,
    ));
    // 3. Область видео сузилась (сайдбар): слои изменились.
    let narrow = RenderViewport {
        x: 0,
        y: 0,
        width: 112,
        height: TARGET[1],
    };
    for index in 0..4 {
        let ui = fresh_ui(&egui_context, TARGET, 10.0 + index as f32 * 9.0);
        frames.push(present_with(renderer, TARGET, &key_a, narrow, ui));
    }
    // 4. Новый кадр видео.
    let (key_b, _lease_b) = materialize_key(renderer, &decoder, picture(2, 40, 210));
    for index in 0..4 {
        let ui = fresh_ui(&egui_context, TARGET, 20.0 + index as f32 * 9.0);
        frames.push(present_with(renderer, TARGET, &key_b, narrow, ui));
    }
    // 5. Изменение размера окна.
    let resized_full = RenderViewport::full_surface(RESIZED[0], RESIZED[1]);
    for index in 0..4 {
        let ui = fresh_ui(&egui_context, RESIZED, 6.0 + index as f32 * 9.0);
        frames.push(present_with(renderer, RESIZED, &key_b, resized_full, ui));
    }
    frames
}

fn renderer_with_ring(back_buffers: Option<usize>) -> Option<Gles2Renderer> {
    let size = RenderSurfaceSize {
        width: TARGET[0],
        height: TARGET[1],
    };
    let created = match back_buffers {
        Some(count) => Gles2Renderer::new_offscreen_with_back_buffers(
            size,
            SurfacePresentSettings::default(),
            count,
        ),
        None => Gles2Renderer::new_offscreen(size, SurfacePresentSettings::default()),
    };
    match created {
        Ok(renderer) => Some(renderer),
        Err(error) if error.stage == render_shell_api::RendererCreateStage::ApiConnection => {
            assert!(
                std::env::var_os("GLES2_EGUI_REQUIRE_EGL").is_none(),
                "partial present shell test: EGL is required but unavailable: {error}"
            );
            eprintln!("skip partial present shell test: no EGL GLES2 context: {error}");
            None
        }
        Err(error) => panic!("renderer creation failed: {error}"),
    }
}

fn check_ring(back_buffers: usize) {
    let Some(mut reference) = renderer_with_ring(None) else {
        return;
    };
    let Some(mut renderer) = renderer_with_ring(Some(back_buffers)) else {
        return;
    };
    let want = run_scenario(&mut reference);
    let got = run_scenario(&mut renderer);
    assert_eq!(got.len(), want.len());
    for (index, (actual, expected)) in got.iter().zip(&want).enumerate() {
        assert!(
            actual == expected,
            "ring of {back_buffers}: frame {index} differs from the full repaint"
        );
    }
    // Эталон не умеет частично: все кадры полные.
    let reference_stats = reference.partial_present_stats();
    assert_eq!(reference_stats.partial_presents, 0);
    // Рендер с кольцом перерисовывал кадры «только интерфейс» частично и заметно меньше цели.
    let stats = renderer.partial_present_stats();
    assert!(
        stats.partial_presents >= 5,
        "ring of {back_buffers}: expected partial presents, got {stats:?}"
    );
    assert!(
        stats.repainted_pixels * 3 < stats.target_pixels * 2,
        "ring of {back_buffers}: repainted {} of {} pixels",
        stats.repainted_pixels,
        stats.target_pixels
    );
    let counters = renderer.frame_counters();
    assert!(counters.frames_with_unchanged_layers >= 12, "{counters:?}");
}

#[test]
fn shell_partial_present_matches_full_repaint_with_one_back_buffer() {
    check_ring(1);
}

#[test]
fn shell_partial_present_matches_full_repaint_with_two_back_buffers() {
    check_ring(2);
}

#[test]
fn shell_partial_present_matches_full_repaint_with_three_back_buffers() {
    check_ring(3);
}
