//! Перевод потери контекста и ошибок ожидания в словарь контракта.

use gles2_egui::{ContextLossDetection, EglCallError, EglErrorCode, EglOperation};

use super::*;

fn loss(reason: ContextLossReason) -> ContextLoss {
    ContextLoss {
        reason,
        detection: ContextLossDetection::ResetStatusQuery,
    }
}

/// Любая потеря контекста — потеря устройства с текстом причины для лога и отчёта об ошибке.
#[test]
fn every_context_loss_is_a_lost_device_with_its_reason_in_the_message() {
    for reason in [
        ContextLossReason::GuiltyReset,
        ContextLossReason::InnocentReset,
        ContextLossReason::UnknownReset,
        ContextLossReason::EglContextLost,
    ] {
        let lost = device_lost_from(loss(reason));
        assert_eq!(lost.reason, RendererDeviceLossReason::Unknown);
        assert!(
            lost.message.contains(&format!("{reason:?}")),
            "`{}` must name {reason:?}",
            lost.message
        );
    }
}

/// Потеря контекста при постановке fence — `DeviceLost`; прочие отказы — `WaitRejected`, а не
/// потеря устройства (приложение не должно считать здоровый контекст мёртвым).
#[test]
fn fence_failures_separate_device_loss_from_a_rejected_wait() {
    assert!(matches!(
        drain_error_for_context(Gles2Error::ContextLost(loss(
            ContextLossReason::GuiltyReset
        ))),
        RendererGpuDrainError::DeviceLost(_)
    ));
    assert_eq!(
        drain_error_for_context(Gles2Error::Driver(EglCallError {
            operation: EglOperation::MakeCurrent,
            code: EglErrorCode::BadAlloc,
        })),
        RendererGpuDrainError::WaitRejected
    );
    assert_eq!(
        drain_error_for_context(Gles2Error::Gl {
            operation: "fence",
            reason: "out of memory".into(),
        }),
        RendererGpuDrainError::WaitRejected
    );
}

/// Ожидание GPU не считает работу законченной, пока отпущенный кадр не вернулся декодеру: кадр
/// ждёт fence — таймаут и кадр на месте; fence сработал — кадр вернулся к моменту ответа.
#[test]
fn idle_waits_until_released_frames_are_back_at_the_decoder() {
    use crate::frame_release::tests::fakes::{EVENT_TIMEOUT, FakeFence, Harness};

    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(5);
    assert!(
        fence.wait_until_waited(EVENT_TIMEOUT),
        "the waiter waits for the fence"
    );

    assert_eq!(
        drain_returned_frames(&harness.binding, Duration::from_millis(30)),
        Err(RendererGpuDrainError::Timeout)
    );
    assert_eq!(harness.inner_log.released_handles(), Vec::<u64>::new());

    fence.signal();

    assert_eq!(
        drain_returned_frames(&harness.binding, EVENT_TIMEOUT),
        Ok(())
    );
    assert_eq!(
        harness.inner_log.released_handles(),
        vec![5],
        "the frame is back at the decoder by the time the wait answers"
    );
}

/// Без текущего контекста ждётся fence последнего показанного кадра: сигнал — готово, ещё нет —
/// таймаут, потеря контекста — потеря устройства, кадров не было — ждать нечего.
#[test]
fn without_a_current_context_the_last_presented_fence_decides() {
    use crate::frame_release::tests::fakes::{EVENT_TIMEOUT, FakeFence, Harness};

    let harness = Harness::new();
    assert_eq!(
        presented_work_result(
            harness
                .binding
                .wait_for_last_presented_fence(Duration::ZERO)
        ),
        Ok(()),
        "nothing was presented, nothing to wait for"
    );

    let fence = FakeFence::pending();
    harness.publish(&fence);
    assert_eq!(
        presented_work_result(
            harness
                .binding
                .wait_for_last_presented_fence(Duration::from_millis(20))
        ),
        Err(RendererGpuDrainError::Timeout)
    );

    fence.signal();
    assert_eq!(
        presented_work_result(harness.binding.wait_for_last_presented_fence(EVENT_TIMEOUT)),
        Ok(())
    );

    fence.fail_with_context_loss();
    assert!(matches!(
        presented_work_result(harness.binding.wait_for_last_presented_fence(EVENT_TIMEOUT)),
        Err(RendererGpuDrainError::DeviceLost(_))
    ));
}
