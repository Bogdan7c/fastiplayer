//! Ожидание возврата кадров (`wait_for_pending_releases`): наблюдает очередь, но не возвращает
//! кадры сам.

use std::time::Duration;

use super::super::drain::{PendingReleaseDrain, PresentedWorkWait};
use super::fakes::{EVENT_TIMEOUT, FakeFence, Harness};

/// Кадров в очереди нет — ожидание не ждёт.
#[test]
fn nothing_pending_means_drained_at_once() {
    let harness = Harness::new();

    assert_eq!(
        harness.binding.wait_for_pending_releases(Duration::ZERO),
        PendingReleaseDrain::Drained
    );
}

/// Кадр ждёт fence: ожидание заканчивается таймаутом и не освобождает кадр; после сигнала fence
/// оно возвращается только когда кадр уже у декодера.
#[test]
fn waits_until_the_waiter_has_returned_the_frame_to_the_decoder() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(31);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT), "waiter waits");

    assert_eq!(
        harness
            .binding
            .wait_for_pending_releases(Duration::from_millis(30)),
        PendingReleaseDrain::TimedOut { still_pending: 1 },
        "an unsignaled fence keeps the frame pending"
    );
    assert_eq!(
        harness.inner_log.released_handles(),
        Vec::<u64>::new(),
        "observing does not release anything"
    );

    fence.signal();

    assert_eq!(
        harness.binding.wait_for_pending_releases(EVENT_TIMEOUT),
        PendingReleaseDrain::Drained
    );
    assert_eq!(
        harness.inner_log.released_handles(),
        vec![31],
        "the frame is back at the decoder by the time the wait returns"
    );
}

/// Ожидание последнего fence различает «кадров не было», «ещё идёт», «дошёл» и «отказ».
#[test]
fn waiting_for_the_last_presented_fence_tells_the_four_situations_apart() {
    let harness = Harness::new();
    assert_eq!(
        harness
            .binding
            .wait_for_last_presented_fence(Duration::ZERO),
        PresentedWorkWait::NothingPublished
    );

    let fence = FakeFence::pending();
    harness.publish(&fence);
    assert_eq!(
        harness
            .binding
            .wait_for_last_presented_fence(Duration::from_millis(20)),
        PresentedWorkWait::Pending
    );

    fence.signal();
    assert_eq!(
        harness.binding.wait_for_last_presented_fence(EVENT_TIMEOUT),
        PresentedWorkWait::Done
    );

    fence.fail_with_context_loss();
    assert!(matches!(
        harness.binding.wait_for_last_presented_fence(EVENT_TIMEOUT),
        PresentedWorkWait::Failed(_)
    ));
}
