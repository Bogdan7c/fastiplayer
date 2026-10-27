//! Остальное содержимое кадра вокруг видео: интерфейс egui поверх картинки, живые настройки
//! цвета в пикселях, субтитры, прозрачность кадра, возможности по драйверу.

use egui::{Color32, Context, Rect, pos2};
use render_core::{PictureLayout, RenderLiveSettings, RenderViewport};
use render_shell_api::{
    EguiFrameContent, RenderFrameOutcome, RenderShell, SubtitleLayerOffsets, SubtitleOverlayFrame,
    SurfacePresentSettings,
};
use subtitle_raster::CanvasSize;

use super::subtitle_support::{rasterize_text, rasterizer};
use super::{
    FrameExtras, assert_gray, connect_software_decoder, materialize_key, offscreen_or_skip, pixel,
    present,
};
use crate::test_support::fake_decoder::SplitPicture;
use crate::{Gles2Renderer, renderable_frame};

const TARGET: [u32; 2] = [64, 32];

fn picture(handle: u64, left_luma: u8, right_luma: u8) -> SplitPicture {
    SplitPicture {
        handle,
        size: TARGET,
        left_luma,
        right_luma,
    }
}

/// Кадр egui: заливка прямоугольника цветом (в точках, масштаб 1).
fn egui_rect_frame(rect: Rect, color: Color32) -> EguiFrameContent {
    let context = Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            pos2(0.0, 0.0),
            egui::vec2(TARGET[0] as f32, TARGET[1] as f32),
        )),
        max_texture_side: Some(4096),
        ..Default::default()
    };
    let output = context.run_ui(raw_input, |ui| {
        ui.painter().rect_filled(rect, 0.0, color);
    });
    let paint_jobs = context.tessellate(output.shapes, output.pixels_per_point);
    EguiFrameContent::Fresh {
        paint_jobs,
        textures_delta: output.textures_delta,
    }
}

/// Интерфейс рисуется поверх видео (синий прямоугольник над картинкой), а повтор прошлого
/// интерфейса даёт те же пиксели без нового прохода egui.
#[test]
fn the_interface_is_drawn_over_the_video_and_can_be_reused() {
    let Some(mut renderer) = offscreen_or_skip(
        "the_interface_is_drawn_over_the_video_and_can_be_reused",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    let ui = egui_rect_frame(
        Rect::from_min_max(pos2(0.0, 0.0), pos2(8.0, 8.0)),
        Color32::BLUE,
    );

    let outcome = present(
        &mut renderer,
        TARGET,
        Some(&key),
        FrameExtras {
            egui: Some(ui),
            ..FrameExtras::default()
        },
    );

    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    assert_eq!(
        pixel(&renderer, 4, 4),
        [0, 0, 255, 255],
        "interface over video"
    );
    assert_gray(pixel(&renderer, 20, 20), 180, "video around the interface");

    let screen = super::screen_for(TARGET);
    assert!(renderer.can_reuse_retained_ui(screen));
    let reused = present(
        &mut renderer,
        TARGET,
        Some(&key),
        FrameExtras {
            egui: Some(EguiFrameContent::ReusePrevious),
            ..FrameExtras::default()
        },
    );
    assert!(
        matches!(reused, RenderFrameOutcome::Presented(_)),
        "{reused:?}"
    );
    assert_eq!(pixel(&renderer, 4, 4), [0, 0, 255, 255], "reused interface");
}

/// Живые настройки видны в пикселях: яркость поднимает картинку, откат возвращает исходный
/// цвет; отказ неподдержанного поля не меняет картинку.
#[test]
fn live_color_settings_change_the_picture_and_rollback_restores_it() {
    let Some(mut renderer) = offscreen_or_skip(
        "live_color_settings_change_the_picture_and_rollback_restores_it",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 100, 60));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 100, "default settings");
    let baseline = renderer.live_settings();

    let mut brighter: RenderLiveSettings = baseline.clone();
    brighter.color_pipeline.adjustment.brightness = 0.2;
    renderer
        .live_settings_adapter()
        .commit_live_settings(&brighter)
        .expect("brightness is supported");
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    // 100/255 + 0,2 → ≈151 (допуск шейдера).
    let lifted = pixel(&renderer, 16, 16);
    assert!(
        lifted[0].abs_diff(151) <= 2,
        "brightness +0.2 must lift 100 to about 151, got {lifted:?}"
    );

    let mut refused = brighter.clone();
    refused.color_pipeline.tone_mapping = render_core::ToneMappingMode::Reinhard;
    assert!(
        renderer
            .live_settings_adapter()
            .commit_live_settings(&refused)
            .is_err()
    );
    assert_eq!(
        renderer.live_settings(),
        brighter,
        "a refusal changes nothing"
    );

    renderer
        .live_settings_adapter()
        .rollback_live_settings(&baseline)
        .expect("rollback");
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 100, "after the rollback");
}

/// Стартовые настройки (`set_*`) действуют на первый же кадр без проверки «поддерживается ли
/// поле».
#[test]
fn startup_color_settings_apply_to_the_first_frame() {
    let Some(mut renderer) =
        offscreen_or_skip("startup_color_settings_apply_to_the_first_frame", TARGET)
    else {
        return;
    };
    let mut pipeline = render_core::ColorPipelineSettings::default();
    pipeline.adjustment.brightness = 0.2;
    renderer.set_color_pipeline_settings(pipeline);
    let decoder = connect_software_decoder(&mut renderer);
    let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 100, 60));

    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));

    assert!(pixel(&renderer, 16, 16)[0].abs_diff(151) <= 2);
    assert_eq!(renderer.live_settings().color_pipeline, pipeline);
}

/// Субтитры ложатся поверх видео внутри картинки, а полосы letterbox остаются чёрными.
#[test]
fn subtitles_are_drawn_over_the_video_and_not_over_the_letterbox_bars() {
    let target = [256_u32, 192];
    let Some(mut renderer) = offscreen_or_skip(
        "subtitles_are_drawn_over_the_video_and_not_over_the_letterbox_bars",
        target,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let frame_picture = SplitPicture {
        handle: 1,
        size: [256, 108],
        left_luma: 90,
        right_luma: 90,
    };
    let (key, _lease) = materialize_key(&mut renderer, &decoder, frame_picture);
    let renderable = renderable_frame(key.decoded_frame()).expect("renderable");
    let rect = PictureLayout::fit_frame(
        &renderable,
        RenderViewport::full_surface(target[0], target[1]),
    )
    .expect("picture layout")
    .rect();
    let canvas =
        CanvasSize::new(rect.width.round() as u32, rect.height.round() as u32).expect("canvas");
    let overlay = rasterize_text(&mut rasterizer(), "Hello", canvas);
    assert!(!overlay.is_empty());

    let without = present(&mut renderer, target, Some(&key), FrameExtras::default());
    assert!(matches!(without, RenderFrameOutcome::Presented(_)));
    let video_only = renderer.read_offscreen_pixels().expect("readback");
    let with = present(
        &mut renderer,
        target,
        Some(&key),
        FrameExtras {
            subtitles: Some(SubtitleOverlayFrame {
                overlay: &overlay,
                picture_rect: rect,
                layer_offsets: SubtitleLayerOffsets::NONE,
            }),
            ..FrameExtras::default()
        },
    );
    assert!(matches!(with, RenderFrameOutcome::Presented(_)), "{with:?}");
    let subtitled = renderer.read_offscreen_pixels().expect("readback");

    let (top, bottom) = (rect.y.round() as u32, (rect.y + rect.height).round() as u32);
    let mut changed_inside = 0_u32;
    for y in 0..target[1] {
        for x in 0..target[0] {
            let before = video_only.pixel(x, y).expect("pixel");
            let after = subtitled.pixel(x, y).expect("pixel");
            if y < top || y >= bottom {
                assert_eq!(
                    after, before,
                    "letterbox bar pixel ({x},{y}) must stay as it was"
                );
            } else if after != before {
                changed_inside += 1;
            }
        }
    }
    assert!(
        changed_inside > 100,
        "the text plate must be visible over the picture, changed pixels: {changed_inside}"
    );
}

/// Окно плеера непрозрачное целиком: альфа кадра 1 и в углах, и в центре (скругления углов
/// нет, решение 57 плана gles2-renderer), даже при пустом интерфейсе над чёрным фоном.
#[test]
fn window_target_is_fully_opaque_including_corners() {
    let size = [64_u32, 64];
    let Ok(mut renderer) = Gles2Renderer::new_offscreen(
        render_shell_api::RenderSurfaceSize {
            width: size[0],
            height: size[1],
        },
        SurfacePresentSettings::default(),
    ) else {
        eprintln!("skip opaque corners: no EGL");
        assert!(std::env::var_os("GLES2_EGUI_REQUIRE_EGL").is_none());
        return;
    };
    let outcome = present(&mut renderer, size, None, FrameExtras::default());
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    for (x, y) in [(0, 0), (63, 0), (0, 63), (63, 63), (32, 32)] {
        assert_eq!(pixel(&renderer, x, y)[3], 255, "alpha at ({x}, {y})");
    }
}

/// Возможности объявлены по тому, что нашлось на драйвере: программные 8-бит контракты есть
/// всегда, высокая разрядность и HDR→SDR — ровно когда у шейдера есть `highp`.
#[test]
fn capabilities_follow_what_the_driver_offers() {
    let Some(renderer) = offscreen_or_skip("capabilities_follow_what_the_driver_offers", TARGET)
    else {
        return;
    };
    let capabilities = renderer.capabilities();
    for contract in crate::GLES2_SOFTWARE_FRAME_CONTRACTS {
        assert!(
            capabilities.supports_frame_contract(contract),
            "{contract:?}"
        );
    }
    let highp = gles2_video::ShaderPrecision::for_capabilities(renderer.context.capabilities())
        .supports_high_bit_depth();
    for contract in crate::GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS {
        assert_eq!(
            capabilities.supports_frame_contract(contract),
            highp,
            "{contract:?}"
        );
    }
    assert_eq!(capabilities.supports_hdr_to_sdr, highp);
    assert_eq!(
        capabilities.backend,
        render_core::RenderBackendKind::OpenGles
    );
    assert_eq!(renderer.family(), render_shell_api::RendererFamily::Gles2);
}
