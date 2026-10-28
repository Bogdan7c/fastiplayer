//! AV1 (libdav1d) без CPU-копии на GLES2 — сессия 13 плана gles2-renderer.
//!
//! Патч `ffmpeg-vendor` даёт обёртке libdav1d `AV_CODEC_CAP_DR1`, и dav1d пишет кадры прямо
//! в запечатанные memfd-блоки декодера, из которых рендер делает udmabuf → EGLImage. Без
//! патча кадры AV1 лежат в пуле FFmpeg, и тесты «без копии» падают на счётчиках
//! (`frames_without_cpu_copy == 0`).

use crate::harness::{
    FixtureStream, assert_frames_reach_screen_through_the_copy_path,
    assert_frames_reach_screen_without_cpu_copy, assert_zero_copy_matches_copy_path,
};

/// Обычная память (заказ wgpu) — прежний путь с одной копией, кадры те же.
#[test]
fn ffmpeg_av1_frames_are_presented_by_the_gles2_renderer_through_the_copy_path() {
    assert_frames_reach_screen_through_the_copy_path(
        "ffmpeg_av1_frames_are_presented_by_the_gles2_renderer_through_the_copy_path",
        FixtureStream::av1_8bit(),
    );
}

/// 8 бит: каждый кадр без копии, каждый пиксель сверен с CPU-формулой цвета.
#[test]
fn ffmpeg_av1_frames_in_memfd_blocks_reach_the_screen_without_a_cpu_copy() {
    assert_frames_reach_screen_without_cpu_copy(
        "ffmpeg_av1_frames_in_memfd_blocks_reach_the_screen_without_a_cpu_copy",
        FixtureStream::av1_8bit(),
    );
}

/// 8 бит: картинка пути без копии совпадает с путём копии (readback, допуск ±1).
#[test]
fn ffmpeg_av1_zero_copy_picture_matches_the_copy_path() {
    assert_zero_copy_matches_copy_path("ffmpeg_av1_8bit", FixtureStream::av1_8bit());
}

/// 10 бит (`GR88`): без копии и совпадает с путём копии.
#[test]
fn ffmpeg_av1_10bit_frames_reach_the_screen_without_a_cpu_copy_and_match_the_copy_path() {
    assert_zero_copy_matches_copy_path("ffmpeg_av1_10bit", FixtureStream::av1_10bit());
}

/// Зерно плёнки: dav1d кладёт картинку с зерном во второй буфер — тоже наш блок, без копии.
#[test]
fn ffmpeg_av1_film_grain_frames_reach_the_screen_without_a_cpu_copy() {
    assert_frames_reach_screen_without_cpu_copy(
        "ffmpeg_av1_film_grain_frames_reach_the_screen_without_a_cpu_copy",
        FixtureStream::av1_film_grain(),
    );
}
