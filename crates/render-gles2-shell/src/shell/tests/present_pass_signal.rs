//! Сигнал «проход показа отправлен» (12a.2) на настоящем GLES2-рендере без окна.
//!
//! По этому сигналу такт аппаратного декодера ставит картинку в очередь GPU после прохода
//! показа. Звонок обязан быть ровно один на показанный кадр и не звучать, когда кадр не ушёл.

use render_shell_api::{RenderFrameOutcome, RenderShell};

use super::{FrameExtras, connect_software_decoder, materialize_key, offscreen_or_skip, present};
use crate::shell::frame_fence::FrameVideoUse;
use crate::test_support::fake_decoder::SplitPicture;

const TARGET: [u32; 2] = [64, 32];

fn picture(handle: u64) -> SplitPicture {
    SplitPicture {
        handle,
        size: TARGET,
        left_luma: 200,
        right_luma: 40,
    }
}

/// Каждый показанный кадр с видео звонит подписчику ровно один раз (по счётчику рендера), и
/// жетон лежит у подписчика после каждого кадра.
#[test]
fn every_presented_video_frame_signals_one_present_pass() {
    let Some(mut renderer) = offscreen_or_skip(
        "every_presented_video_frame_signals_one_present_pass",
        TARGET,
    ) else {
        return;
    };
    let signal = renderer
        .present_pass_signal()
        .expect("GLES2 renderer offers the signal");
    let subscription = signal
        .subscribe()
        .expect("fresh signal accepts a subscriber");
    let decoder = connect_software_decoder(&mut renderer);

    for handle in 1..=3 {
        let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(handle));
        let outcome = present(&mut renderer, TARGET, Some(&key), FrameExtras::default());
        assert!(
            matches!(outcome, RenderFrameOutcome::Presented(_)),
            "frame {handle}: {outcome:?}"
        );
        assert_eq!(
            subscription.pulse_receiver().try_recv(),
            Ok(()),
            "frame {handle} must signal its present pass"
        );
    }

    let counters = renderer.frame_counters();
    assert_eq!(counters.present_pass_signals, 3);
    assert_eq!(counters.present_pass_signals, counters.frames_presented);
    assert_eq!(counters.present_pass_signal_failures, 0);
}

/// Кадр, который начал рисоваться и упал, на экран не ушёл: прохода показа нет, сигнала нет.
#[test]
fn failed_frame_does_not_signal_a_present_pass() {
    let Some(mut renderer) =
        offscreen_or_skip("failed_frame_does_not_signal_a_present_pass", TARGET)
    else {
        return;
    };
    let signal = renderer
        .present_pass_signal()
        .expect("GLES2 renderer offers the signal");
    let subscription = signal
        .subscribe()
        .expect("fresh signal accepts a subscriber");
    let decoder = connect_software_decoder(&mut renderer);
    let (_key, _lease) = materialize_key(&mut renderer, &decoder, picture(7));

    renderer.retire_work_of_failed_frame(
        &gles2_egui::UiFrameError::Layer(gles2_egui::FrameLayerError {
            message: "overlay draw failed after the video was drawn".to_owned(),
        }),
        FrameVideoUse::WithVideo,
    );

    assert!(
        subscription.pulse_receiver().try_recv().is_err(),
        "no present pass was submitted"
    );
    assert_eq!(renderer.frame_counters().present_pass_signals, 0);
}
