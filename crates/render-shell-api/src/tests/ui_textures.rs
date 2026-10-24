//! Синхронизатор текстур интерфейса на настоящем `egui::Context`: проверяются дельты, которые
//! получил бы рендер (их состав, порядок и содержимое), а не вызовы.

use egui::TextureOptions;
use egui::epaint::{ImageData, ImageDelta, TextureId};
use egui::{Color32, ColorImage, TexturesDelta};

use super::{UiTextureFullSetReason, UiTextureSync};

/// Один UI-проход с текстом; возвращает его дельту текстур.
fn ui_pass(egui_context: &egui::Context, text: &str) -> TexturesDelta {
    let output = egui_context.run_ui(egui::RawInput::default(), |ui| {
        ui.label(text);
    });
    output.textures_delta
}

/// Все дельты рендера для id (пусто, если id нет в `set`).
fn deltas_for(delta: &TexturesDelta, texture_id: TextureId) -> Vec<ImageDelta> {
    delta
        .set
        .get(&texture_id)
        .map(|deltas| deltas.to_vec())
        .unwrap_or_default()
}

/// Ровно одна целая дельта id; возвращает её изображение.
fn single_full_image(delta: &TexturesDelta, texture_id: TextureId) -> ColorImage {
    let deltas = deltas_for(delta, texture_id);
    assert_eq!(
        deltas.len(),
        1,
        "{texture_id:?} must have one delta: {delta:?}"
    );
    assert!(
        deltas[0].is_whole(),
        "{texture_id:?} must be a whole upload: {delta:?}"
    );
    let ImageData::Color(image) = &deltas[0].image;
    (**image).clone()
}

/// Рендер в тестах «применяет» дельту: egui требует явно очистить её перед уничтожением.
fn consume(mut delta: TexturesDelta) {
    delta.clear();
}

#[test]
fn without_request_renderer_gets_ui_pass_delta_unchanged() {
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    let first_pass = ui_pass(&egui_context, "first");
    let expected = format!("{first_pass:?}");

    sync.accept_ui_pass_delta(first_pass);
    let delivered = sync.take_delta_for_renderer(&egui_context);

    assert_eq!(format!("{delivered:?}"), expected);
    consume(delivered);
    let nothing_new = sync.take_delta_for_renderer(&egui_context);
    assert!(
        nothing_new.is_empty(),
        "delivered changes must not repeat: {nothing_new:?}"
    );
}

#[test]
fn full_set_contains_current_font_atlas_and_live_images_with_partial_edits() {
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    consume(ui_pass(&egui_context, "warm up"));

    let mut kept = egui_context.load_texture(
        "kept",
        ColorImage::filled([4, 4], Color32::RED),
        TextureOptions::NEAREST,
    );
    let freed = egui_context.load_texture(
        "freed",
        ColorImage::filled([2, 2], Color32::BLUE),
        TextureOptions::NEAREST,
    );
    let freed_id = freed.id();
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "images loaded"));
    kept.set_partial(
        [1, 2],
        ColorImage::filled([2, 1], Color32::GREEN),
        TextureOptions::LINEAR,
    );
    drop(freed);
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "images edited"));
    // Прежний рендер получил всё это; новый рендер не знает ничего.
    consume(sync.take_delta_for_renderer(&egui_context));

    sync.request_full_set(UiTextureFullSetReason::RendererInstalled);
    let full_set = sync.take_delta_for_renderer(&egui_context);

    let atlas = single_full_image(&full_set, TextureId::default());
    assert_eq!(
        atlas.size,
        egui_context.fonts(|fonts| fonts.font_image_size()),
        "font atlas must be the current egui atlas"
    );
    let kept_image = single_full_image(&full_set, kept.id());
    assert_eq!(kept_image.size, [4, 4]);
    for (index, pixel) in kept_image.pixels.iter().enumerate() {
        let [x, y] = [index % 4, index / 4];
        let expected = if y == 2 && (1..=2).contains(&x) {
            Color32::GREEN
        } else {
            Color32::RED
        };
        assert_eq!(*pixel, expected, "kept image pixel ({x}, {y})");
    }
    assert_eq!(
        deltas_for(&full_set, kept.id())[0].options,
        TextureOptions::LINEAR,
        "latest sampling options must be restored"
    );
    assert!(
        deltas_for(&full_set, freed_id).is_empty(),
        "freed image must not come back: {full_set:?}"
    );
    assert_eq!(sync.retained_texture_count(), 1);
    assert_eq!(sync.pending_full_set(), None);
    consume(full_set);
}

#[test]
fn undelivered_delta_including_free_rides_with_next_frame() {
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    consume(ui_pass(&egui_context, "warm up"));
    let image = egui_context.load_texture(
        "image",
        ColorImage::filled([2, 2], Color32::RED),
        TextureOptions::NEAREST,
    );
    let image_id = image.id();
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "uploaded"));
    consume(sync.take_delta_for_renderer(&egui_context));

    // Кадр с освобождением не дошёл до рендера (нет буфера swapchain): take не вызывается.
    drop(image);
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "freed, frame dropped"));
    let second_image = egui_context.load_texture(
        "second",
        ColorImage::filled([1, 1], Color32::WHITE),
        TextureOptions::NEAREST,
    );
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "next frame"));
    let delivered = sync.take_delta_for_renderer(&egui_context);

    assert!(
        delivered.free.contains(&image_id),
        "free of the dropped frame must reach the renderer: {delivered:?}"
    );
    assert_eq!(
        single_full_image(&delivered, second_image.id()).size,
        [1, 1]
    );
    consume(delivered);
}

#[test]
fn repeated_request_delivers_one_full_set_once() {
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "first"));
    consume(sync.take_delta_for_renderer(&egui_context));

    sync.request_full_set(UiTextureFullSetReason::RendererInstalled);
    sync.request_full_set(UiTextureFullSetReason::RendererReportedMissingTexture);
    assert_eq!(
        sync.pending_full_set(),
        Some(UiTextureFullSetReason::RendererReportedMissingTexture)
    );
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "second"));
    let full_set = sync.take_delta_for_renderer(&egui_context);
    // Атлас — одна целая дельта, даже если проход добавил свою частичную правку.
    let _atlas = single_full_image(&full_set, TextureId::default());
    consume(full_set);

    sync.accept_ui_pass_delta(ui_pass(&egui_context, "second"));
    let after = sync.take_delta_for_renderer(&egui_context);
    assert!(
        deltas_for(&after, TextureId::default())
            .iter()
            .all(|delta| !delta.is_whole()),
        "full set must not repeat without a new request: {after:?}"
    );
    consume(after);
}

#[test]
fn full_set_overrides_partial_atlas_edit_of_the_same_frame() {
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    sync.accept_ui_pass_delta(ui_pass(&egui_context, "abc"));
    consume(sync.take_delta_for_renderer(&egui_context));

    // Новые глифы дают частичную правку атласа; новому рендеру она не к чему применить.
    let new_glyphs_pass = ui_pass(&egui_context, "Съешь же ещё этих мягких булок 0123456789");
    let has_partial_atlas_edit = deltas_for(&new_glyphs_pass, TextureId::default())
        .iter()
        .any(|delta| !delta.is_whole());
    assert!(
        has_partial_atlas_edit,
        "test premise: new glyphs must give a partial atlas edit, got {new_glyphs_pass:?}"
    );
    sync.accept_ui_pass_delta(new_glyphs_pass);
    sync.request_full_set(UiTextureFullSetReason::RendererInstalled);
    let delivered = sync.take_delta_for_renderer(&egui_context);

    let atlas = single_full_image(&delivered, TextureId::default());
    assert_eq!(atlas, egui_context.fonts(|fonts| fonts.image()));
    consume(delivered);
}

#[test]
fn full_set_waits_for_the_first_ui_pass() {
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    sync.request_full_set(UiTextureFullSetReason::RendererInstalled);

    // До первого прохода у egui нет шрифтов: набор не собирается и не теряется.
    let before_pass = sync.take_delta_for_renderer(&egui_context);
    assert!(before_pass.is_empty());
    assert_eq!(
        sync.pending_full_set(),
        Some(UiTextureFullSetReason::RendererInstalled)
    );

    sync.accept_ui_pass_delta(ui_pass(&egui_context, "first"));
    let delivered = sync.take_delta_for_renderer(&egui_context);
    let _atlas = single_full_image(&delivered, TextureId::default());
    consume(delivered);
}

#[test]
fn partial_edit_outside_known_image_drops_the_untrusted_copy() {
    let egui_context = egui::Context::default();
    let mut sync = UiTextureSync::new();
    let image_id = TextureId::Managed(4242);
    let mut upload = TexturesDelta::default();
    upload.push(
        image_id,
        ImageDelta::full(
            ColorImage::filled([2, 2], Color32::RED),
            TextureOptions::NEAREST,
        ),
    );
    sync.accept_ui_pass_delta(upload);
    assert_eq!(sync.retained_texture_count(), 1);

    let mut overflow = TexturesDelta::default();
    overflow.push(
        image_id,
        ImageDelta::partial(
            [1, 1],
            ColorImage::filled([2, 2], Color32::GREEN),
            TextureOptions::NEAREST,
        ),
    );
    sync.accept_ui_pass_delta(overflow);

    assert_eq!(sync.retained_texture_count(), 0);
    consume(sync.take_delta_for_renderer(&egui_context));
}
