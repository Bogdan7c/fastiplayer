//! Таблица перевода исходов кадра.

use gles2_egui::{
    ContextLoss, ContextLossDetection, ContextLossReason, Gles2Error, RetainedUiUnavailable,
};
use render_shell_api::RenderFrameFailureKind;

use super::*;

/// Повтор UI и рассинхрон текстур — разные ответы приложения, они не сливаются.
#[test]
fn drop_reasons_keep_their_meaning() {
    for reason in [
        RetainedUiUnavailable::NothingRetained,
        RetainedUiUnavailable::ScreenMismatch,
    ] {
        assert_eq!(
            drop_reason_of(UiFrameDropReason::RetainedUiUnavailable(reason)),
            RenderFrameDropReason::RetainedUiUnavailable
        );
    }
    assert_eq!(
        drop_reason_of(UiFrameDropReason::UiTexturesOutOfSync {
            texture_id: egui::TextureId::Managed(1)
        }),
        RenderFrameDropReason::UiTexturesOutOfSync
    );
    assert_eq!(
        drop_reason_of(UiFrameDropReason::UiTexturesNotApplied),
        RenderFrameDropReason::UiTexturesOutOfSync
    );
}

/// Окно отпущено — `PresentationDetached`; окно свёрнуто — `SurfaceOccluded`.
#[test]
fn skipped_frames_map_to_detached_and_occluded() {
    assert_eq!(
        skip_reason_of(FrameSkipReason::PresentationDetached),
        RenderFrameDropReason::PresentationDetached
    );
    assert_eq!(
        skip_reason_of(FrameSkipReason::ZeroSize),
        RenderFrameDropReason::SurfaceOccluded
    );
}

/// Ошибка кадра — `Failed(VideoRender)`, причина потери контекста видна в тексте.
#[test]
fn frame_errors_become_video_render_failures_with_the_cause() {
    let loss = ContextLoss {
        reason: ContextLossReason::GuiltyReset,
        detection: ContextLossDetection::ResetStatusQuery,
    };
    let failure = failure_of(&UiFrameError::Context(Gles2Error::ContextLost(loss)));
    assert_eq!(failure.kind, RenderFrameFailureKind::VideoRender);
    assert!(
        failure.message.contains("GuiltyReset"),
        "{}",
        failure.message
    );
}
