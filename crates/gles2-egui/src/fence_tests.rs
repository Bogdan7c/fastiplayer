//! Fence и очередь освобождений на fake-драйвере.

use std::cell::Cell;
use std::sync::Arc;

use super::*;
use crate::context::display_registry::{DisplayLease, DisplayRegistry};
use crate::context::driver::{EglErrorCode, NativeDisplay};
use crate::test_support::fake_egl::{FakeBehavior, FakeCall, FakeEgl};

/// Fake отправки команд: считает `glFlush`/`glFinish`.
#[derive(Default)]
struct CountingSubmission {
    flushes: Cell<u32>,
    finishes: Cell<u32>,
}

impl GpuCommandSubmission for CountingSubmission {
    fn flush(&self) {
        self.flushes.set(self.flushes.get() + 1);
    }

    fn finish(&self) {
        self.finishes.set(self.finishes.get() + 1);
    }
}

fn surfaceless_lease(
    behavior: FakeBehavior,
) -> (Arc<FakeEgl>, Arc<DisplayRegistry>, Arc<DisplayLease>) {
    let (fake, registry) = FakeEgl::with_registry(behavior);
    let lease = match registry.acquire(NativeDisplay::Surfaceless) {
        Ok(lease) => Arc::new(lease),
        Err(error) => panic!("fake display must open: {error}"),
    };
    (fake, registry, lease)
}

fn issue_or_panic(
    issuer: &mut FenceIssuer,
    lease: &Arc<DisplayLease>,
    submission: &CountingSubmission,
) -> GpuFence {
    match issuer.issue(lease, submission) {
        Ok(fence) => fence,
        Err(error) => panic!("fence must be issued: {error}"),
    }
}

#[test]
fn egl_fence_flushes_signals_and_is_destroyed_exactly_once() {
    let (fake, _registry, lease) = surfaceless_lease(FakeBehavior::default());
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::EglFenceSync);

    let fence = issue_or_panic(&mut issuer, &lease, &submission);
    assert_eq!(submission.flushes.get(), 1, "fence must reach the GPU");
    assert_eq!(submission.finishes.get(), 0);
    assert_eq!(
        fence.wait(Duration::from_millis(5)),
        Ok(FenceStatus::Signaled)
    );
    assert_eq!(issuer.stats().egl_fences_created, 1);
    assert_eq!(issuer.stats().fence_fallback_finishes, 0);

    drop(fence);
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::DestroySync(_))),
        1
    );
}

#[test]
fn finish_fallback_counts_every_gl_finish_and_creates_no_sync() {
    let (fake, _registry, lease) = surfaceless_lease(FakeBehavior::default());
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::FinishFallback);

    let first = issue_or_panic(&mut issuer, &lease, &submission);
    let second = issue_or_panic(&mut issuer, &lease, &submission);
    assert_eq!(submission.finishes.get(), 2);
    assert_eq!(issuer.stats().fence_fallback_finishes, 2);
    assert_eq!(issuer.stats().egl_fences_created, 0);
    assert_eq!(first.poll(), Ok(FenceStatus::Signaled));
    assert_eq!(second.poll(), Ok(FenceStatus::Signaled));
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::CreateSync(_))),
        0
    );
}

#[test]
fn completion_queue_releases_in_order_once_each_fence_signals() {
    let (fake, _registry, lease) = surfaceless_lease(FakeBehavior {
        new_syncs_pending: true,
        ..FakeBehavior::default()
    });
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::EglFenceSync);
    let mut queue = GpuCompletionQueue::new();
    queue.push(issue_or_panic(&mut issuer, &lease, &submission), "frame-1");
    queue.push(issue_or_panic(&mut issuer, &lease, &submission), "frame-2");

    // GPU ещё читает: ничего не освобождается.
    let nothing = queue.drain_completed();
    assert!(nothing.released.is_empty());
    assert_eq!(nothing.stopped_by, None);
    assert_eq!(queue.len(), 2);

    fake.signal_all_syncs();
    let released = queue.drain_completed();
    assert_eq!(released.released, vec!["frame-1", "frame-2"]);
    assert!(queue.is_empty());
    // Повторный опрос ничего не отдаёт: освобождение ровно один раз.
    assert!(queue.drain_completed().released.is_empty());
    // Sync каждого fence разрушен ровно один раз (fake упал бы на втором).
    assert_eq!(
        fake.count(|call| matches!(call, FakeCall::DestroySync(_))),
        2
    );
}

#[test]
fn completion_queue_stops_at_the_first_pending_fence() {
    let (fake, _registry, lease) = surfaceless_lease(FakeBehavior::default());
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::EglFenceSync);
    let mut queue = GpuCompletionQueue::new();
    queue.push(issue_or_panic(&mut issuer, &lease, &submission), 1_u32);
    fake.update_behavior(|behavior| behavior.new_syncs_pending = true);
    queue.push(issue_or_panic(&mut issuer, &lease, &submission), 2_u32);

    let partial = queue.drain_completed();
    assert_eq!(partial.released, vec![1]);
    assert_eq!(queue.len(), 1);
}

#[test]
fn context_loss_while_waiting_is_reported_and_drain_after_loss_releases_once() {
    let (fake, _registry, lease) = surfaceless_lease(FakeBehavior {
        new_syncs_pending: true,
        ..FakeBehavior::default()
    });
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::EglFenceSync);
    let mut queue = GpuCompletionQueue::new();
    queue.push(issue_or_panic(&mut issuer, &lease, &submission), "a");
    queue.push(issue_or_panic(&mut issuer, &lease, &submission), "b");
    fake.update_behavior(|behavior| behavior.wait_error = Some(EglErrorCode::ContextLost));

    let stopped = queue.drain_completed();
    assert!(stopped.released.is_empty());
    assert!(matches!(
        stopped.stopped_by,
        Some(FenceWaitError::ContextLost(_))
    ));
    assert_eq!(queue.len(), 2, "nothing is lost when polling fails");

    assert_eq!(queue.drain_after_context_lost(), vec!["a", "b"]);
    assert!(queue.drain_after_context_lost().is_empty());
}

#[test]
fn driver_error_while_waiting_is_not_reported_as_context_loss() {
    let (fake, _registry, lease) = surfaceless_lease(FakeBehavior::default());
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::EglFenceSync);
    let fence = issue_or_panic(&mut issuer, &lease, &submission);
    fake.update_behavior(|behavior| behavior.wait_error = Some(EglErrorCode::BadAlloc));
    assert!(
        matches!(fence.poll(), Err(FenceWaitError::Driver(error)) if error.code == EglErrorCode::BadAlloc)
    );
}

#[test]
fn live_fence_keeps_its_display_from_being_terminated() {
    let (fake, registry, lease) = surfaceless_lease(FakeBehavior::default());
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::EglFenceSync);
    let fence = issue_or_panic(&mut issuer, &lease, &submission);

    // Контекст ушёл, а fence ещё ждёт в потоке плеера.
    drop(lease);
    assert_eq!(fake.count(|call| matches!(call, FakeCall::Terminate(_))), 0);
    assert_eq!(registry.lease_count(NativeDisplay::Surfaceless), 1);
    assert_eq!(fence.poll(), Ok(FenceStatus::Signaled));

    drop(fence);
    assert_eq!(fake.count(|call| matches!(call, FakeCall::Terminate(_))), 1);
    fake.assert_nothing_alive();
}

#[test]
fn fence_can_be_waited_from_another_thread() {
    let (_fake, _registry, lease) = surfaceless_lease(FakeBehavior::default());
    let submission = CountingSubmission::default();
    let mut issuer = FenceIssuer::new(FenceStrategy::EglFenceSync);
    let fence = issue_or_panic(&mut issuer, &lease, &submission);
    let waited = std::thread::spawn(move || fence.wait(Duration::from_millis(5))).join();
    assert!(matches!(waited, Ok(Ok(FenceStatus::Signaled))));
}
