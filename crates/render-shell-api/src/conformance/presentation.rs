//! Проверки жизненного цикла показа на окне (сессия 02a, «одно окно — одна surface»).
//!
//! Рендер без окна (offscreen-цель стенда) моделирует те же состояния: «отпустить окно»
//! прекращает выдачу буферов, «вернуть» — возобновляет; перенастройка показа на месте
//! запоминает действующую конфигурацию и различает «применено» и «уже так».

use crate::{
    EguiFrameContent, PresentationDetachOutcome, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured, RenderFrameDropReason,
    RenderScreenDescriptor, RenderShell, RenderSurfaceSize, ShellPresentMode, SurfaceFrameAcquire,
    SurfacePresentSettings,
};

use super::{
    ConformanceCheck, ConformanceViolation, RenderShellConformanceHarness, create_renderer,
    expect_presented, render_with_acquired_surface,
};

/// Проверка 8: пока окно отпущено, буферов нет (`Dropped(PresentationDetached)`), resize и
/// перенастройка этого не меняют; повторы detach/reattach — явные no-op; после возврата окна
/// кадр снова показывается.
pub fn check_detached_presentation_gives_no_frames(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::DetachedPresentationGivesNoFrames;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;

    expect_equal(
        check,
        "first detach",
        renderer.detach_presentation(),
        PresentationDetachOutcome::Detached,
    )?;
    expect_equal(
        check,
        "repeated detach",
        renderer.detach_presentation(),
        PresentationDetachOutcome::AlreadyDetached,
    )?;
    expect_no_buffer_while_detached(renderer.as_mut(), check, screen, "after detach")?;

    // Resize без окна только запоминает размер и не возвращает буферы.
    let [width, height] = screen.size_in_pixels;
    renderer.resize(RenderSurfaceSize { width, height });
    expect_no_buffer_while_detached(
        renderer.as_mut(),
        check,
        screen,
        "after resize while detached",
    )?;

    match renderer.reconfigure_presentation(harness.initial_presentation()) {
        Err(PresentationReconfigureError::PresentationDetached) => {}
        other => {
            return Err(ConformanceViolation::new(
                check,
                format!(
                    "reconfigure without the window must be PresentationDetached, got {other:?}"
                ),
            ));
        }
    }

    expect_equal(
        check,
        "first reattach",
        renderer.reattach_presentation(),
        Ok(PresentationReattachOutcome::Reattached),
    )?;
    expect_equal(
        check,
        "repeated reattach",
        renderer.reattach_presentation(),
        Ok(PresentationReattachOutcome::AlreadyAttached),
    )?;
    let presented = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::empty_fresh(),
    )?;
    expect_presented(check, "frame after reattach", &presented)
}

/// Проверка 9: перенастройка показа на месте применяет изменение, повтор тех же настроек —
/// `AlreadyActive`, смена прозрачности — отказ без изменений; показ после всего продолжается.
///
/// Запрос — FIFO (его обязаны поддерживать и Vulkan, и EGL со swap interval 1) с задержкой на
/// единицу больше стартовой, поэтому первый вызов не может оказаться no-op.
pub fn check_presentation_reconfigure_distinguishes_no_op(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::PresentationReconfigureDistinguishesNoOp;
    let screen = harness.screen();
    let initial = harness.initial_presentation();
    let mut renderer = create_renderer(harness)?;
    let requested = SurfacePresentSettings {
        present_mode: ShellPresentMode::Fifo,
        max_frame_latency: initial.max_frame_latency.max(1) + 1,
    };

    expect_equal(
        check,
        "first reconfigure to FIFO with a deeper latency",
        renderer.reconfigure_presentation(requested),
        Ok(PresentationReconfigured::AsRequested),
    )?;
    expect_equal(
        check,
        "repeating the same presentation settings",
        renderer.reconfigure_presentation(requested),
        Ok(PresentationReconfigured::AlreadyActive),
    )?;

    let presented = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::empty_fresh(),
    )?;
    expect_presented(check, "frame after reconfigure", &presented)
}

/// Без окна `acquire_surface_frame` обязан дать именно `Dropped(PresentationDetached)`.
fn expect_no_buffer_while_detached(
    renderer: &mut dyn RenderShell,
    check: ConformanceCheck,
    screen: RenderScreenDescriptor,
    moment: &str,
) -> Result<(), ConformanceViolation> {
    match renderer.acquire_surface_frame(screen) {
        SurfaceFrameAcquire::Dropped(RenderFrameDropReason::PresentationDetached) => Ok(()),
        SurfaceFrameAcquire::Dropped(other) => Err(ConformanceViolation::new(
            check,
            format!("{moment}: acquire must drop as PresentationDetached, got {other:?}"),
        )),
        SurfaceFrameAcquire::Acquired(_) => Err(ConformanceViolation::new(
            check,
            format!("{moment}: renderer gave a buffer without holding the window"),
        )),
    }
}

/// Сравнивает исход с ожидаемым и объясняет расхождение.
fn expect_equal<Outcome: PartialEq + std::fmt::Debug>(
    check: ConformanceCheck,
    what: &str,
    actual: Outcome,
    expected: Outcome,
) -> Result<(), ConformanceViolation> {
    if actual == expected {
        Ok(())
    } else {
        Err(ConformanceViolation::new(
            check,
            format!("{what}: expected {expected:?}, got {actual:?}"),
        ))
    }
}
