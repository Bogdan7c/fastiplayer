//! Пересоздание рендера и текстуры интерфейса на НАСТОЯЩЕМ wgpu без окна (сессия 02a).
//!
//! Корень падения Apply настроек Vulkan: новый рендер получал только частичную правку атласа
//! шрифтов (новые глифы), а самого атласа не видел — egui-wgpu паниковал. Здесь egui-кадр с
//! текстом идёт через `UiTextureSync`, как в приложении: рендер A рисует текст, «пересоздание»
//! (свежий рендер B) получает полный набор и рисует тот же текст теми же пикселями.

use egui::epaint::TextureId;
use render_core::RenderViewport;
use render_shell_api::{
    EguiFrameContent, RenderFrameDropReason, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor, RenderShell, SurfaceFrameAcquire, UiTextureFullSetReason,
    UiTextureSync,
};

use crate::Renderer;
use crate::test_support::{RgbaFrame, pixel_matches};

/// Цель достаточного размера для строки текста (16×8 стенда видео для текста мало).
const UI_TARGET_SIZE: [u32; 2] = [256, 64];

/// Полоса первой строки текста: она одинакова в кадре до и после пересоздания.
const FIRST_LINE_BAND: ([u32; 2], [u32; 2]) = ([0, 0], [UI_TARGET_SIZE[0], 18]);

/// Первая строка: её глифы уже есть в атласе после первого прохода.
const FIRST_LINE: &str = "Fastiplayer";

/// Вторая строка с новыми глифами: даёт частичную правку атласа.
const NEW_GLYPHS_LINE: &str = "Ж42 Юля";

/// Ещё новые глифы для рендера без полного набора.
const MORE_NEW_GLYPHS_LINE: &str = "Щ987 ёлка";

/// Новые глифы кадра восстановления (полный набор обязан перекрыть их частичную правку).
const RECOVERY_GLYPHS_LINE: &str = "Ъ65 ЭФ";

const fn ui_screen() -> RenderScreenDescriptor {
    RenderScreenDescriptor {
        size_in_pixels: UI_TARGET_SIZE,
        pixels_per_point: 1.0,
    }
}

/// Рендер без окна нужного размера либо `None` с причиной пропуска.
fn ui_renderer_or_skip(test_name: &str) -> Option<Renderer> {
    match Renderer::new_headless(UI_TARGET_SIZE) {
        Ok(Some(renderer)) => Some(renderer),
        Ok(None) => {
            eprintln!("skip {test_name}: no Vulkan adapter for the headless wgpu renderer");
            None
        }
        Err(error) => panic!("{test_name}: headless wgpu renderer creation failed: {error:#}"),
    }
}

/// Один UI-проход приложения: строки текста сверху вниз, дельта — в синхронизатор.
fn ui_pass(
    egui_context: &egui::Context,
    sync: &mut UiTextureSync,
    lines: &[&str],
) -> (Vec<egui::epaint::ClippedPrimitive>, bool) {
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(UI_TARGET_SIZE[0] as f32, UI_TARGET_SIZE[1] as f32),
        )),
        ..egui::RawInput::default()
    };
    let output = egui_context.run_ui(raw_input, |ui| {
        for line in lines {
            ui.label(*line);
        }
    });
    let has_partial_atlas_edit = output
        .textures_delta
        .set
        .get(&TextureId::default())
        .is_some_and(|deltas| deltas.iter().any(|delta| !delta.is_whole()));
    sync.accept_ui_pass_delta(output.textures_delta);
    let paint_jobs = egui_context.tessellate(output.shapes, output.pixels_per_point);
    (paint_jobs, has_partial_atlas_edit)
}

/// Кадр без видео со свежим интерфейсом и дельтой текстур от синхронизатора.
fn present_ui(
    renderer: &mut Renderer,
    egui_context: &egui::Context,
    sync: &mut UiTextureSync,
    paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
) -> RenderFrameOutcome {
    let screen = ui_screen();
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen) else {
        panic!("offscreen target must always give a buffer");
    };
    renderer.render_frame(RenderFrameInput {
        surface,
        video_frame: None,
        egui: EguiFrameContent::Fresh {
            paint_jobs,
            textures_delta: sync.take_delta_for_renderer(egui_context),
        },
        screen,
        video_viewport: RenderViewport::full_surface(UI_TARGET_SIZE[0], UI_TARGET_SIZE[1]),
        video_exclusion_rects: Vec::new(),
        subtitle_overlay: None,
    })
}

fn read_frame(renderer: &Renderer) -> RgbaFrame {
    renderer
        .read_offscreen_frame()
        .expect("headless renderer has an offscreen target")
}

fn first_line_pixels(frame: &RgbaFrame) -> Vec<[u8; 4]> {
    let (min, max) = FIRST_LINE_BAND;
    frame.region(min, max)
}

/// Рендер A рисует текст; рендер B (= пересоздание) получает полный набор при следующем
/// проходе, где есть частичная правка атласа, — кадр показан, текст тот же до пикселя.
#[test]
fn recreated_renderer_draws_same_ui_text_after_full_texture_set() {
    let test_name = "recreated_renderer_draws_same_ui_text_after_full_texture_set";
    let Some(mut renderer_a) = ui_renderer_or_skip(test_name) else {
        return;
    };
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();

    let (paint_jobs, _) = ui_pass(&egui_context, &mut sync, &[FIRST_LINE]);
    let outcome = present_ui(&mut renderer_a, &egui_context, &mut sync, paint_jobs);
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "first UI frame must be presented, got {outcome:?}"
    );
    let before = first_line_pixels(&read_frame(&renderer_a));
    let text_pixels = before
        .iter()
        .filter(|pixel| !pixel_matches(**pixel, [0, 0, 0, 255]))
        .count();
    assert!(
        text_pixels > 20,
        "the first line must actually draw glyphs, got {text_pixels} non-black pixels"
    );
    drop(renderer_a);

    let Some(mut renderer_b) = ui_renderer_or_skip(test_name) else {
        return;
    };
    sync.request_full_set(UiTextureFullSetReason::RendererInstalled);
    let (paint_jobs, has_partial_atlas_edit) =
        ui_pass(&egui_context, &mut sync, &[FIRST_LINE, NEW_GLYPHS_LINE]);
    assert!(
        has_partial_atlas_edit,
        "test premise: new glyphs must give a partial atlas edit"
    );
    let outcome = present_ui(&mut renderer_b, &egui_context, &mut sync, paint_jobs);
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "recreated renderer must present the UI frame, got {outcome:?}"
    );
    let after = first_line_pixels(&read_frame(&renderer_b));
    let mismatched = before
        .iter()
        .zip(&after)
        .filter(|(before, after)| !pixel_matches(**before, **after))
        .count();
    assert_eq!(
        mismatched, 0,
        "first line must be pixel-identical before and after recreation"
    );
}

/// Анти-вакуум: свежий рендер без полного набора получает только частичную правку атласа и
/// отвечает типизированным `UiTexturesOutOfSync` (до сессии 02a здесь паниковал egui-wgpu);
/// полный набор следующим кадром возвращает показ.
#[test]
fn recreated_renderer_without_full_set_reports_out_of_sync_ui_textures() {
    let test_name = "recreated_renderer_without_full_set_reports_out_of_sync_ui_textures";
    let Some(mut renderer_a) = ui_renderer_or_skip(test_name) else {
        return;
    };
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    let (paint_jobs, _) = ui_pass(&egui_context, &mut sync, &[FIRST_LINE]);
    assert!(matches!(
        present_ui(&mut renderer_a, &egui_context, &mut sync, paint_jobs),
        RenderFrameOutcome::Presented(_)
    ));
    drop(renderer_a);

    let Some(mut renderer_b) = ui_renderer_or_skip(test_name) else {
        return;
    };
    let (paint_jobs, has_partial_atlas_edit) = ui_pass(
        &egui_context,
        &mut sync,
        &[FIRST_LINE, MORE_NEW_GLYPHS_LINE],
    );
    assert!(
        has_partial_atlas_edit,
        "test premise: new glyphs must give a partial atlas edit"
    );
    let outcome = present_ui(&mut renderer_b, &egui_context, &mut sync, paint_jobs);
    assert_eq!(
        outcome,
        RenderFrameOutcome::Dropped(RenderFrameDropReason::UiTexturesOutOfSync)
    );

    // Реакция приложения: полный набор следующим кадром со свежим интерфейсом. В этом проходе
    // снова новые глифы: без атласа у рендера кадр опять был бы отклонён.
    sync.request_full_set(UiTextureFullSetReason::RendererReportedMissingTexture);
    let (paint_jobs, has_partial_atlas_edit) = ui_pass(
        &egui_context,
        &mut sync,
        &[FIRST_LINE, MORE_NEW_GLYPHS_LINE, RECOVERY_GLYPHS_LINE],
    );
    assert!(
        has_partial_atlas_edit,
        "test premise: recovery pass must also edit the atlas partially"
    );
    let outcome = present_ui(&mut renderer_b, &egui_context, &mut sync, paint_jobs);
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "full UI texture set must restore presentation, got {outcome:?}"
    );
}
