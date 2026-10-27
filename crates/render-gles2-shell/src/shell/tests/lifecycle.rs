//! Жизненный цикл показа: отпуск и возврат окна, перенастройка на месте. Рендер без окна
//! моделирует те же состояния, что окно (`presentation`); живое окно проверяется на железе.

use render_shell_api::{
    PresentationDetachOutcome, PresentationReattachOutcome, PresentationReconfigureError,
    PresentationReconfigured, RenderFrameDropReason, RenderFrameOutcome, RenderShell,
    RenderSurfaceSize, ShellPresentMode, SurfaceFrameAcquire, SurfacePresentSettings,
};

use super::{
    FrameExtras, assert_gray, connect_software_decoder, materialize_key, offscreen_or_skip, pixel,
    present, screen_for,
};
use crate::test_support::fake_decoder::SplitPicture;

const TARGET: [u32; 2] = [64, 32];

fn picture(handle: u64) -> SplitPicture {
    SplitPicture {
        handle,
        size: TARGET,
        left_luma: 180,
        right_luma: 60,
    }
}

/// Пока окно отпущено, буферов нет и размер не возвращает их; после возврата тот же ключ видео
/// снова доходит до экрана с теми же пикселями (импорт кадров окно не трогает).
#[test]
fn a_detached_window_gives_no_buffers_and_the_picture_returns_with_the_window() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_detached_window_gives_no_buffers_and_the_picture_returns_with_the_window",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(1));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));

    assert_eq!(
        renderer.detach_presentation(),
        PresentationDetachOutcome::Detached
    );
    assert_eq!(
        renderer.detach_presentation(),
        PresentationDetachOutcome::AlreadyDetached
    );
    renderer
        .wait_for_gpu_idle(std::time::Duration::from_secs(5))
        .expect("a renderer without the window still waits for its presented work");
    renderer.resize(RenderSurfaceSize {
        width: 96,
        height: 48,
    });
    assert!(
        matches!(
            renderer.acquire_surface_frame(screen_for(TARGET)),
            SurfaceFrameAcquire::Dropped(RenderFrameDropReason::PresentationDetached)
        ),
        "no buffer without the window"
    );

    assert_eq!(
        renderer.reattach_presentation(),
        Ok(PresentationReattachOutcome::Reattached)
    );
    assert_eq!(
        renderer.reattach_presentation(),
        Ok(PresentationReattachOutcome::AlreadyAttached)
    );
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(
        pixel(&renderer, 16, 16),
        180,
        "picture after the window came back",
    );
    assert_gray(
        pixel(&renderer, 48, 16),
        60,
        "picture after the window came back",
    );
}

/// Перенастройка: `Mailbox` честно заменяется, повтор — `AlreadyActive`, смена только
/// задержки — применена (запомнена), смена прозрачности и перенастройка без окна — отказ без
/// изменений.
#[test]
fn reconfiguration_reports_substitution_repeat_and_refusals_without_changing_state() {
    let Some(mut renderer) = offscreen_or_skip(
        "reconfiguration_reports_substitution_repeat_and_refusals_without_changing_state",
        TARGET,
    ) else {
        return;
    };
    let base = SurfacePresentSettings::default();
    let mailbox = SurfacePresentSettings {
        present_mode: ShellPresentMode::Mailbox,
        ..base
    };
    assert_eq!(
        renderer.reconfigure_presentation(mailbox),
        Ok(PresentationReconfigured::PresentModeFallback {
            requested: ShellPresentMode::Mailbox
        }),
        "EGL has no mailbox: the substitution is reported"
    );
    assert_eq!(
        renderer.reconfigure_presentation(mailbox),
        Ok(PresentationReconfigured::AlreadyActive)
    );

    let immediate = SurfacePresentSettings {
        present_mode: ShellPresentMode::Immediate,
        ..base
    };
    assert_eq!(
        renderer.reconfigure_presentation(immediate),
        Ok(PresentationReconfigured::AsRequested)
    );
    let deeper = SurfacePresentSettings {
        max_frame_latency: immediate.max_frame_latency + 3,
        ..immediate
    };
    assert_eq!(
        renderer.reconfigure_presentation(deeper),
        Ok(PresentationReconfigured::AsRequested),
        "the latency is not controlled by EGL, but the request is remembered as in effect"
    );
    assert_eq!(
        renderer.reconfigure_presentation(deeper),
        Ok(PresentationReconfigured::AlreadyActive)
    );

    assert_eq!(
        renderer.detach_presentation(),
        PresentationDetachOutcome::Detached
    );
    assert_eq!(
        renderer.reconfigure_presentation(base),
        Err(PresentationReconfigureError::PresentationDetached)
    );
    assert_eq!(
        renderer.reattach_presentation(),
        Ok(PresentationReattachOutcome::Reattached)
    );
    assert_eq!(
        renderer.reconfigure_presentation(deeper),
        Ok(PresentationReconfigured::AlreadyActive),
        "the detached refusal changed nothing"
    );
}
