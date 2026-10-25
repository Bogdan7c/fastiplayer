//! Кадр, начавший рисоваться и вернувший ошибку, всё равно прикрывает работу GPU fence-ом:
//! кадр декодера не должен вернуться, пока GPU может его читать.

use std::time::Duration;

use gles2_egui::{
    ContextLoss, ContextLossDetection, ContextLossReason, FrameLayerError, UiFrameError,
};

use render_shell_api::RenderShell;

use super::{connect_software_decoder, materialize_key, offscreen_or_skip};
use crate::shell::frame_fence::FrameVideoUse;
use crate::test_support::fake_decoder::SplitPicture;

const TARGET: [u32; 2] = [64, 32];
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

fn layer_failure() -> UiFrameError {
    UiFrameError::Layer(FrameLayerError {
        message: "overlay draw failed after the video was drawn".to_owned(),
    })
}

fn picture() -> SplitPicture {
    SplitPicture {
        handle: 5,
        size: TARGET,
        left_luma: 180,
        right_luma: 60,
    }
}

/// Ошибка кадра с видео после начала рисования: fence поставлен и опубликован. Без него привязка
/// не знала бы ни одного fence и вернула бы кадр «без fence» (`released_without_fence`).
#[test]
fn a_failed_video_frame_publishes_a_fence_before_the_decoder_frame_is_returned() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_failed_video_frame_publishes_a_fence_before_the_decoder_frame_is_returned",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, lease) = materialize_key(&mut renderer, &decoder, picture());
    let fences_before = renderer.context.fence_stats();

    renderer.retire_work_of_failed_frame(&layer_failure(), FrameVideoUse::WithVideo);

    let fences_after = renderer.context.fence_stats();
    assert_eq!(
        fences_after.egl_fences_created + fences_after.fence_fallback_finishes,
        fences_before.egl_fences_created + fences_before.fence_fallback_finishes + 1,
        "exactly one fence for the failed frame"
    );
    assert_eq!(renderer.frame_counters().failed_frames_fenced, 1);
    assert_eq!(renderer.frame_counters().frame_fence_failures, 0);

    drop(key);
    drop(lease);
    renderer
        .wait_for_gpu_idle(IDLE_TIMEOUT)
        .expect("the GPU becomes idle");
    assert_eq!(decoder.state.release_count(5), 1, "returned exactly once");
    assert_eq!(
        renderer.frame_release_stats().released_without_fence,
        0,
        "the frame was returned by a published fence, not blindly"
    );
}

/// Кадр без видео не читает кадров декодера: fence не ставится, ничего не считается.
#[test]
fn a_failed_frame_without_video_sets_no_fence() {
    let Some(mut renderer) =
        offscreen_or_skip("a_failed_frame_without_video_sets_no_fence", TARGET)
    else {
        return;
    };
    let fences_before = renderer.context.fence_stats();

    renderer.retire_work_of_failed_frame(&layer_failure(), FrameVideoUse::WithoutVideo);

    assert_eq!(renderer.context.fence_stats(), fences_before);
    assert_eq!(renderer.frame_counters().failed_frames_fenced, 0);
}

/// Потеря контекста — прежний путь (`release_after_device_lost`): fence не ставится и не
/// считается защитой.
#[test]
fn a_failed_frame_on_a_lost_context_leaves_release_to_the_device_lost_path() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_failed_frame_on_a_lost_context_leaves_release_to_the_device_lost_path",
        TARGET,
    ) else {
        return;
    };
    renderer.context.simulate_context_loss(ContextLoss {
        reason: ContextLossReason::GuiltyReset,
        detection: ContextLossDetection::ResetStatusQuery,
    });
    let fences_before = renderer.context.fence_stats();

    renderer.retire_work_of_failed_frame(&layer_failure(), FrameVideoUse::WithVideo);

    assert_eq!(renderer.context.fence_stats(), fences_before);
    assert_eq!(renderer.frame_counters().failed_frames_fenced, 0);
    assert_eq!(renderer.frame_counters().frame_fence_failures, 0);
}
