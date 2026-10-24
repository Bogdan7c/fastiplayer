//! Доставка текстур интерфейса на настоящем `egui::Context`: проверяется дельта, которую
//! получил бы рендер в `EguiFrameContent::Fresh`.

use egui::epaint::TextureId;
use render_shell_api::{EguiFrameContent, UiTextureFullSetReason, UiTextureSync};

use super::{PreparedEguiContent, egui_content_for_renderer};

/// Один UI-проход приложения: дельта — в синхронизатор, в кадре остаются примитивы.
fn ui_pass(
    egui_context: &egui::Context,
    ui_textures: &mut UiTextureSync,
    text: &str,
) -> PreparedEguiContent {
    let output = egui_context.run_ui(egui::RawInput::default(), |ui| {
        ui.label(text);
    });
    ui_textures.accept_ui_pass_delta(output.textures_delta);
    PreparedEguiContent::Fresh {
        paint_jobs: egui_context.tessellate(output.shapes, output.pixels_per_point),
    }
}

/// Дельта текстур, которую рендер получил бы с этим кадром (пустая у повтора интерфейса).
fn delivered_textures(content: EguiFrameContent) -> egui::TexturesDelta {
    match content {
        EguiFrameContent::Fresh { textures_delta, .. } => textures_delta,
        EguiFrameContent::ReusePrevious => egui::TexturesDelta::default(),
    }
}

/// Есть ли в дельте целая загрузка атласа шрифтов.
fn has_whole_font_atlas(delta: &egui::TexturesDelta) -> bool {
    delta
        .set
        .get(&TextureId::default())
        .is_some_and(|deltas| deltas.iter().any(egui::epaint::ImageDelta::is_whole))
}

/// Кадр без буфера swapchain выбрасывает подготовленный интерфейс; изменения его текстур
/// (здесь — первая целая загрузка атласа) приходят со следующим свежим кадром.
#[test]
fn frame_without_swapchain_buffer_delivers_its_textures_with_next_fresh_frame() {
    let egui_context = egui::Context::default();
    let mut ui_textures = UiTextureSync::new();

    let dropped_frame = ui_pass(&egui_context, &mut ui_textures, "first frame");
    // `acquire_surface_frame` вернул `Dropped`: кадр дальше не идёт.
    drop(dropped_frame);
    let next_frame = ui_pass(&egui_context, &mut ui_textures, "first frame");
    let mut delivered = delivered_textures(egui_content_for_renderer(
        &mut ui_textures,
        &egui_context,
        next_frame,
    ));

    assert!(
        has_whole_font_atlas(&delivered),
        "atlas upload of the dropped frame must reach the renderer: {delivered:?}"
    );
    delivered.clear();
}

/// Повтор удержанного интерфейса не забирает изменения: они ждут свежего кадра.
#[test]
fn reused_ui_frame_keeps_pending_textures_for_next_fresh_frame() {
    let egui_context = egui::Context::default();
    let mut ui_textures = UiTextureSync::new();
    let _not_presented = ui_pass(&egui_context, &mut ui_textures, "first frame");

    let reused = delivered_textures(egui_content_for_renderer(
        &mut ui_textures,
        &egui_context,
        PreparedEguiContent::ReusePrevious,
    ));
    assert!(reused.is_empty());
    let next_frame = ui_pass(&egui_context, &mut ui_textures, "first frame");
    let mut delivered = delivered_textures(egui_content_for_renderer(
        &mut ui_textures,
        &egui_context,
        next_frame,
    ));

    assert!(has_whole_font_atlas(&delivered), "{delivered:?}");
    delivered.clear();
}

/// Новый рендер в слоте: ближайший свежий кадр несёт полный атлас, хотя UI-проход ничего
/// не менял (без запроса рендер не получил бы ничего и рисовал бы без шрифтов).
#[test]
fn installed_renderer_gets_full_font_atlas_with_next_fresh_frame() {
    let egui_context = egui::Context::default();
    let mut ui_textures = UiTextureSync::new();
    let first_frame = ui_pass(&egui_context, &mut ui_textures, "same text");
    delivered_textures(egui_content_for_renderer(
        &mut ui_textures,
        &egui_context,
        first_frame,
    ))
    .clear();
    let unchanged_frame = ui_pass(&egui_context, &mut ui_textures, "same text");
    let without_request = delivered_textures(egui_content_for_renderer(
        &mut ui_textures,
        &egui_context,
        unchanged_frame,
    ));
    assert!(
        without_request.is_empty(),
        "test premise: unchanged UI has no texture changes: {without_request:?}"
    );

    ui_textures.request_full_set(UiTextureFullSetReason::RendererInstalled);
    let after_install = ui_pass(&egui_context, &mut ui_textures, "same text");
    let mut delivered = delivered_textures(egui_content_for_renderer(
        &mut ui_textures,
        &egui_context,
        after_install,
    ));

    assert!(has_whole_font_atlas(&delivered), "{delivered:?}");
    delivered.clear();
}
