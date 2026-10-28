//! Проверка сигнала «проход показа отправлен на GPU» (gles2-renderer 12a.2).
//!
//! По этому сигналу аппаратный декодер встаёт в очередь GPU после прохода показа. Поэтому
//! рендер, который сигнал отдаёт, обязан звонить после каждого показанного кадра и молчать,
//! когда на GPU ничего не ушло (кадр отклонён). Рендер без сигнала (wgpu) проверку проходит
//! сразу: декодер у него работает без такта.

use video_core::PresentPassSubscription;

use crate::{EguiFrameContent, RenderFrameDropReason, RenderFrameOutcome};

use super::{
    ConformanceCheck, ConformanceViolation, RenderShellConformanceHarness, create_renderer,
    expect_presented, render_with_acquired_surface,
};

/// Проверка 12: отклонённый кадр не звонит, каждый показанный кадр звонит.
pub fn check_present_pass_signal_follows_presented_frames(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::PresentPassSignalFollowsPresentedFrames;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;
    let Some(signal) = renderer.present_pass_signal() else {
        // Рендер не сообщает о проходах показа: такта декодера у него нет, проверять нечего.
        return Ok(());
    };
    let subscription = signal.subscribe().map_err(|error| {
        ConformanceViolation::new(
            check,
            format!("fresh renderer signal refused a subscriber: {error}"),
        )
    })?;

    // `ReusePrevious` на свежем рендере отклоняется: на GPU ничего не отправлено. Если рендер
    // его всё же показал, это нарушение проверки 2, а не этой: тогда жетон честный и
    // просто забирается.
    let refused = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::ReusePrevious,
    )?;
    if refused == RenderFrameOutcome::Dropped(RenderFrameDropReason::RetainedUiUnavailable) {
        expect_no_pulse(check, &subscription, "after a rejected frame")?;
    } else {
        let _ = subscription.pulse_receiver().try_recv();
    }

    for what in ["first presented frame", "second presented frame"] {
        let presented = render_with_acquired_surface(
            renderer.as_mut(),
            check,
            screen,
            None,
            EguiFrameContent::empty_fresh(),
        )?;
        expect_presented(check, what, &presented)?;
        expect_pulse(check, &subscription, what)?;
    }
    Ok(())
}

/// Жетон «показ был» лежит у подписчика (и забирается, чтобы следующий шаг начинал с нуля).
fn expect_pulse(
    check: ConformanceCheck,
    subscription: &PresentPassSubscription,
    what: &str,
) -> Result<(), ConformanceViolation> {
    match subscription.pulse_receiver().try_recv() {
        Ok(()) => Ok(()),
        Err(error) => Err(ConformanceViolation::new(
            check,
            format!("{what} must signal a submitted present pass, got {error:?}"),
        )),
    }
}

/// Жетона нет: рендер не звонил.
fn expect_no_pulse(
    check: ConformanceCheck,
    subscription: &PresentPassSubscription,
    what: &str,
) -> Result<(), ConformanceViolation> {
    match subscription.pulse_receiver().try_recv() {
        Err(error) if error.is_empty() => Ok(()),
        other => Err(ConformanceViolation::new(
            check,
            format!("no present pass may be signalled {what}, got {other:?}"),
        )),
    }
}
