//! Геометрия субтитров от настоящего `RenderableFrame`: `SubmittedFrameForSubtitles::from_submission`.
//!
//! Остальные тесты строят описание кадра через `for_test*` и готовый прямоугольник. Здесь
//! проверяется именно путь точки submit: размер кадра с учётом поворота, вписывание в область
//! видео, зажим области к экрану и отказ для кадра без identity.
//!
//! Функция принимает уже физические пиксели (область видео и размер экрана), поэтому
//! масштаб HiDPI (ppp) сюда не доходит: его применяет вызывающий код до submit.

use std::time::Duration;

use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata, VideoDisplayOrientation};
use render_core::{PictureRect, PictureSize, RenderColorMetadata, RenderViewport, RenderableFrame};
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract, VideoFramePixelLayout};
use video_present_core::VideoPresentFrameIdentity;

use super::super::presentation::SubmittedFrameForSubtitles;

/// Допуск сравнения float-координат прямоугольника (в пикселях).
const PIXEL_TOLERANCE: f32 = 0.01;
/// Pts, по которому проверяется, что кадр передаёт именно свой момент.
const FRAME_PTS: Duration = Duration::from_millis(12_345);
/// Поколение ресурсов рендера, которое должно дойти до результата.
const RENDER_GENERATION: u64 = 9;

/// Кадр с заданным сохранённым размером и поворотом (как его отдаёт декодер в renderer).
fn frame(
    render_width: u32,
    render_height: u32,
    orientation: VideoDisplayOrientation,
) -> RenderableFrame {
    RenderableFrame {
        handle: 1,
        pts: FRAME_PTS,
        format: VideoFramePixelLayout::Nv12,
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        coded_width: render_width,
        coded_height: render_height,
        render_width,
        render_height,
        display_orientation: orientation,
        color: RenderColorMetadata::sdr_bt709_limited(),
    }
}

/// Identity живого кадра нужного поколения рендера.
fn identity() -> VideoPresentFrameIdentity {
    let decoded_frame = DecodedFrame {
        generation: 3,
        pts: FRAME_PTS,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 1920,
        height: 1080,
        render_width: 1920,
        render_height: 1080,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(7),
        diagnostics: VideoFrameDiagnostics::default(),
    };
    VideoPresentFrameIdentity::from_decoded_frame(RENDER_GENERATION, &decoded_frame)
}

/// Сравнивает прямоугольник с ожидаемым `[x, y, width, height]` с допуском на float.
fn assert_rect_close(actual: PictureRect, expected: [f32; 4]) {
    let actual_values = [actual.x, actual.y, actual.width, actual.height];
    for (index, (actual_value, expected_value)) in actual_values.iter().zip(expected).enumerate() {
        assert!(
            (actual_value - expected_value).abs() <= PIXEL_TOLERANCE,
            "компонента {index}: получили {actual_values:?}, ожидали {expected:?}"
        );
    }
}

/// Собирает описание кадра через точку submit; тест упадёт, если вернулось `None`.
fn submit(
    frame: &RenderableFrame,
    video_viewport: RenderViewport,
    screen_size_in_pixels: [u32; 2],
) -> SubmittedFrameForSubtitles {
    SubmittedFrameForSubtitles::from_submission(
        Some(identity()),
        frame,
        video_viewport,
        screen_size_in_pixels,
    )
    .expect("кадр с identity и невырожденной геометрией должен дать описание для субтитров")
}

#[test]
fn rotated_frame_is_fitted_by_oriented_size_with_pillarbox() {
    // Сохранённые 1920x1080 с поворотом на четверть оборота показываются как 1080x1920.
    // В окне 1280x720 это вертикальная картинка: высота 720, ширина 720 * 1080/1920 = 405,
    // по бокам поровну полосы: x = (1280 - 405) / 2 = 437.5.
    for orientation in [
        VideoDisplayOrientation::Rotate90Clockwise,
        VideoDisplayOrientation::Rotate270Clockwise,
    ] {
        let submitted = submit(
            &frame(1920, 1080, orientation),
            RenderViewport::new(0, 0, 1280, 720),
            [1280, 720],
        );

        assert_rect_close(submitted.picture_rect(), [437.5, 0.0, 405.0, 720.0]);
        // Размер картинки для ASS - ориентированный, а не сохранённый.
        assert_eq!(
            submitted.picture_size_for_test(),
            PictureSize::new(1080, 1920)
        );
    }
}

#[test]
fn rotate_180_keeps_landscape_geometry() {
    // Поворот на 180 градусов оси не меняет: кадр остаётся 1920x1080 и заполняет 16:9 окно.
    let submitted = submit(
        &frame(1920, 1080, VideoDisplayOrientation::Rotate180),
        RenderViewport::new(0, 0, 1280, 720),
        [1280, 720],
    );

    assert_rect_close(submitted.picture_rect(), [0.0, 0.0, 1280.0, 720.0]);
}

#[test]
fn landscape_frame_in_taller_window_gets_letterbox() {
    // 16:9 в окне 1280x1024: ширина на весь экран, высота 1280 * 9/16 = 720,
    // полосы сверху и снизу по (1024 - 720) / 2 = 152.
    let submitted = submit(
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 1280, 1024),
        [1280, 1024],
    );

    assert_rect_close(submitted.picture_rect(), [0.0, 152.0, 1280.0, 720.0]);
}

#[test]
fn vertical_frame_in_wide_window_gets_pillarbox() {
    // Вертикальное видео 1080x1920 без поворота в окне 1280x720: те же 405x720 по центру.
    let submitted = submit(
        &frame(1080, 1920, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 1280, 720),
        [1280, 720],
    );

    assert_rect_close(submitted.picture_rect(), [437.5, 0.0, 405.0, 720.0]);
}

#[test]
fn picture_is_fitted_inside_offset_video_viewport() {
    // Область видео не на весь экран (например, панель сбоку): картинка центрируется
    // внутри неё с учётом смещения области. 16:9 в 800x600 со смещением (100, 50):
    // высота 800 * 9/16 = 450, y = 50 + (600 - 450) / 2 = 125.
    let video_viewport = RenderViewport::new(100, 50, 800, 600);
    let submitted = submit(
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        video_viewport,
        [1280, 720],
    );

    assert_rect_close(submitted.picture_rect(), [100.0, 125.0, 800.0, 450.0]);
    assert_eq!(submitted.video_viewport(), video_viewport);
}

#[test]
fn identity_and_pts_of_submitted_frame_are_carried_over() {
    let submitted = submit(
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 1280, 720),
        [1280, 720],
    );

    assert_eq!(submitted.render_generation(), RENDER_GENERATION);
    assert_eq!(submitted.frame_pts_for_test(), FRAME_PTS);
}

#[test]
fn frame_without_identity_gives_no_subtitle_geometry() {
    // Stale-lease: старый кадр во время seek остаётся на экране, но субтитры на нём скрыты.
    let submitted = SubmittedFrameForSubtitles::from_submission(
        None,
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 1280, 720),
        [1280, 720],
    );

    assert_eq!(submitted, None);
}

#[test]
fn frame_with_zero_display_size_gives_no_subtitle_geometry() {
    let submitted = SubmittedFrameForSubtitles::from_submission(
        Some(identity()),
        &frame(0, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 1280, 720),
        [1280, 720],
    );

    assert_eq!(submitted, None);
}

#[test]
fn empty_video_viewport_falls_back_to_full_surface() {
    // Нулевая область видео зажимается к экрану так же, как в Renderer::render_frame:
    // получается весь экран, а не «нет субтитров».
    let submitted = submit(
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 0, 0),
        [1280, 1024],
    );

    assert_eq!(
        submitted.video_viewport(),
        RenderViewport::full_surface(1280, 1024)
    );
    assert_rect_close(submitted.picture_rect(), [0.0, 152.0, 1280.0, 720.0]);
}

#[test]
fn video_viewport_outside_screen_falls_back_to_full_surface() {
    let submitted = submit(
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(2000, 0, 640, 360),
        [1280, 720],
    );

    assert_eq!(
        submitted.video_viewport(),
        RenderViewport::full_surface(1280, 720)
    );
    assert_rect_close(submitted.picture_rect(), [0.0, 0.0, 1280.0, 720.0]);
}

#[test]
fn oversized_video_viewport_is_clamped_to_screen_before_fitting() {
    // Область шире экрана (например, ресайз ещё не догнал окно) режется до 1280x720,
    // и картинка вписывается уже в зажатую область: вертикальное видео даёт 405x720.
    let submitted = submit(
        &frame(1080, 1920, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 3000, 720),
        [1280, 720],
    );

    assert_eq!(
        submitted.video_viewport(),
        RenderViewport::new(0, 0, 1280, 720)
    );
    assert_rect_close(submitted.picture_rect(), [437.5, 0.0, 405.0, 720.0]);
}

#[test]
fn zero_sized_screen_gives_no_subtitle_geometry() {
    // Свёрнутое окно: вписывать некуда, даже fallback на весь экран пустой.
    let submitted = SubmittedFrameForSubtitles::from_submission(
        Some(identity()),
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 1280, 720),
        [0, 0],
    );

    assert_eq!(submitted, None);
}

#[test]
fn physical_pixel_viewport_from_fractional_scale_is_used_as_is() {
    // 1280x720 логических точек при масштабе 1.5 приходят сюда уже физическими 1920x1080.
    // Геометрия считается в физических пикселях без повторного масштаба.
    let submitted = submit(
        &frame(1920, 1080, VideoDisplayOrientation::Identity),
        RenderViewport::new(0, 0, 1920, 1080),
        [1920, 1080],
    );

    assert_rect_close(submitted.picture_rect(), [0.0, 0.0, 1920.0, 1080.0]);
}
