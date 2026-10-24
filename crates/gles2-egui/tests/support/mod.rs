//! Общий стенд пиксельных тестов на настоящем EGL без окна (llvmpipe в CI).
//!
//! Без EGL тест пропускается с причиной в stderr; `GLES2_EGUI_REQUIRE_EGL=1` превращает пропуск
//! в падение (CI ставит Mesa EGL, решение владельца 27).

#![allow(
    dead_code,
    reason = "каждый тестовый бинарь использует свою часть стенда"
)]

use egui::epaint::{ClippedPrimitive, ImageDelta};
use egui::{Color32, ColorImage, Context, Pos2, RawInput, Rect, TextureId, TextureOptions};
use gles2_egui::{
    CornerRadius, CreateError, FrameCompositor, Gles2Context, NoFrameLayers, OffscreenAlpha,
    OffscreenPixels, SurfaceSize, UiContent, UiFrameOutcome, UiFrameRequest, UiScreen,
};

pub const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

pub fn size(width: u32, height: u32) -> SurfaceSize {
    SurfaceSize::new(width, height).expect("test sizes are non-zero")
}

/// Контекст без окна или пропуск теста с причиной.
pub fn offscreen_or_skip(
    test_name: &str,
    target_size: SurfaceSize,
    alpha: OffscreenAlpha,
) -> Option<Gles2Context> {
    match Gles2Context::offscreen_with_alpha(target_size, alpha) {
        Ok(context) => Some(context),
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required ({REQUIRE_EGL_VARIABLE}) but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL surfaceless GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: offscreen GLES2 context creation failed: {error}"),
    }
}

/// Экран 1:1 по размеру цели.
pub fn screen_for(context: &Gles2Context, pixels_per_point: f32) -> UiScreen {
    let extent = context.extent();
    UiScreen {
        size_in_pixels: [extent.width, extent.height],
        pixels_per_point,
    }
}

/// Настоящий проход egui: `run_ui` + `tessellate`.
pub fn egui_frame(
    egui_context: &Context,
    size_in_points: [f32; 2],
    pixels_per_point: f32,
    paint: impl FnMut(&mut egui::Ui),
) -> (Vec<ClippedPrimitive>, egui::TexturesDelta) {
    let raw_input = RawInput {
        screen_rect: Some(Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(size_in_points[0], size_in_points[1]),
        )),
        max_texture_side: Some(4096),
        ..Default::default()
    };
    egui_context.set_pixels_per_point(pixels_per_point);
    let output = egui_context.run_ui(raw_input, paint);
    let primitives = egui_context.tessellate(output.shapes, output.pixels_per_point);
    (primitives, output.textures_delta)
}

/// Кадр через сборщик без слоёв, итог — показанный.
pub fn render(
    compositor: &mut FrameCompositor,
    context: &mut Gles2Context,
    screen: UiScreen,
    content: UiContent,
    background: [f32; 4],
    corner_radius: CornerRadius,
) -> UiFrameOutcome {
    compositor
        .render(
            context,
            UiFrameRequest {
                screen,
                content,
                background,
                layers: &mut NoFrameLayers,
                corner_radius,
            },
        )
        .expect("frame renders")
}

pub fn read(context: &Gles2Context) -> OffscreenPixels {
    context.read_offscreen_pixels().expect("offscreen readback")
}

pub fn pixel(pixels: &OffscreenPixels, x: u32, y: u32) -> [u8; 4] {
    pixels.pixel(x, y).expect("pixel inside target")
}

/// Сравнение каналов с допуском (дизеринг/растеризация).
pub fn assert_close(actual: [u8; 4], expected: [u8; 4], tolerance: u8, what: &str) {
    for channel in 0..4 {
        let difference = actual[channel].abs_diff(expected[channel]);
        assert!(
            difference <= tolerance,
            "{what}: {actual:?} vs expected {expected:?} (±{tolerance})"
        );
    }
}

/// Полная загрузка текстуры одним цветом (как conformance 10/11).
pub fn solid_texture(
    texture_id: TextureId,
    size: [usize; 2],
    color: Color32,
) -> egui::TexturesDelta {
    let mut delta = egui::TexturesDelta::default();
    delta.push(
        texture_id,
        ImageDelta::full(
            ColorImage::new(size, vec![color; size[0] * size[1]]),
            TextureOptions::NEAREST,
        ),
    );
    delta
}

/// Частичная правка текстуры одним цветом.
pub fn patch_texture(
    texture_id: TextureId,
    position: [usize; 2],
    size: [usize; 2],
    color: Color32,
) -> egui::TexturesDelta {
    let mut delta = egui::TexturesDelta::default();
    delta.push(
        texture_id,
        ImageDelta::partial(
            position,
            ColorImage::new(size, vec![color; size[0] * size[1]]),
            TextureOptions::NEAREST,
        ),
    );
    delta
}

/// Прямоугольник, залитый текстурой `texture_id` целиком (uv 0..1), белая вершина.
pub fn textured_rect(texture_id: TextureId, rect: Rect) -> ClippedPrimitive {
    let mut mesh = egui::Mesh::with_texture(texture_id);
    mesh.add_rect_with_uv(
        rect,
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    ClippedPrimitive {
        clip_rect: Rect::EVERYTHING,
        primitive: egui::epaint::Primitive::Mesh(mesh),
    }
}
