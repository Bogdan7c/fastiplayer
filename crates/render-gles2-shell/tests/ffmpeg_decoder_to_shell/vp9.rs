//! VP9 из настоящего FFmpeg: обычная память (копия), memfd без копии, 10 бит против копии.

use crate::harness::{
    FixtureStream, assert_frames_reach_screen_through_the_copy_path,
    assert_frames_reach_screen_without_cpu_copy, assert_zero_copy_matches_copy_path,
};

/// Настоящий декодер → контракт рендера → экран, кадр за кадром, с возвратом кадров декодеру.
#[test]
fn ffmpeg_vp9_frames_are_presented_by_the_gles2_renderer_and_returned_to_the_decoder() {
    assert_frames_reach_screen_through_the_copy_path(
        "ffmpeg_vp9_frames_are_presented_by_the_gles2_renderer_and_returned_to_the_decoder",
        FixtureStream::vp9_8bit(),
    );
}

#[test]
fn ffmpeg_vp9_frames_in_memfd_blocks_reach_the_screen_without_a_cpu_copy() {
    assert_frames_reach_screen_without_cpu_copy(
        "ffmpeg_vp9_frames_in_memfd_blocks_reach_the_screen_without_a_cpu_copy",
        FixtureStream::vp9_8bit(),
    );
}

/// 10-битный VP9 доходит до экрана без CPU-копии (EGLImage `GR88`) и совпадает с путём копии.
#[test]
fn ffmpeg_vp9_10bit_frames_in_memfd_blocks_reach_the_screen_without_a_cpu_copy_and_match_the_copy_path()
 {
    assert_zero_copy_matches_copy_path("ffmpeg_vp9_10bit", FixtureStream::vp9_10bit());
}
