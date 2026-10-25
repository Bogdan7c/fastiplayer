//! Отвязка рендера (`renderer_detaching`): ни один fence не переживает рендер, кадры
//! возвращаются ровно один раз, поток-ожидатель fence не держит.

use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use super::super::detach::RendererDetachGrace;
use super::super::shared::WaiterLifecycle;
use super::fakes::{EVENT_TIMEOUT, FakeFence, Harness, QUIET_WINDOW};

/// Короткая отсрочка для тестов «fence не сработает».
const SHORT_GRACE: RendererDetachGrace = RendererDetachGrace::new(Duration::from_millis(30));

/// Fence и «наблюдатели» за ним: слабая ссылка (жив ли) и журнал потока разрушения.
struct FenceObservers {
    weak: std::sync::Weak<FakeFence>,
    destroyed_in: Arc<Mutex<Vec<ThreadId>>>,
}

impl FenceObservers {
    fn watch(fence: &Arc<FakeFence>) -> Self {
        Self {
            weak: Arc::downgrade(fence),
            destroyed_in: fence.destruction_log(),
        }
    }

    fn is_alive(&self) -> bool {
        self.weak.upgrade().is_some()
    }

    fn destroyed_threads(&self) -> Vec<ThreadId> {
        self.destroyed_in.lock().expect("журнал").clone()
    }
}

/// Главный сценарий причины падения: после отвязки привязка и поток-ожидатель не держат fence,
/// а он разрушен в потоке, вызвавшем отвязку (в приложении это поток рендера с живым контекстом).
#[test]
fn detach_leaves_no_fence_alive_and_destroys_it_in_calling_thread() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    let observers = FenceObservers::watch(&fence);
    harness.publish(&fence);
    harness.request_release(1);
    harness.request_release(2);
    assert!(
        fence.wait_until_waited(EVENT_TIMEOUT),
        "поток-ожидатель обязан держать fence"
    );
    drop(fence);
    assert!(observers.is_alive(), "до отвязки fence держит привязка");

    let report = harness.binding.renderer_detaching(SHORT_GRACE);

    assert!(!report.already_detached);
    assert!(!report.waiter_still_holds_fence);
    assert_eq!(report.released_unfinished, 2);
    assert!(
        !observers.is_alive(),
        "после отвязки fence не держит ни привязка, ни поток-ожидатель"
    );
    assert_eq!(observers.destroyed_threads(), vec![thread::current().id()]);
    assert_eq!(harness.inner_log.released_handles(), vec![1, 2]);
}

/// Кадры ждут fence: до срабатывания не возвращаются, после — ровно по разу.
#[test]
fn detach_waits_for_fence_within_grace_and_releases_each_frame_once() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(5);
    harness.request_release(6);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    let binding = harness.binding.clone();
    let detaching =
        thread::spawn(move || binding.renderer_detaching(RendererDetachGrace::new(EVENT_TIMEOUT)));

    thread::sleep(QUIET_WINDOW);
    assert_eq!(
        harness.inner_log.released_handles(),
        Vec::<u64>::new(),
        "до срабатывания fence кадры не возвращаются"
    );
    fence.signal();
    let report = detaching.join().expect("поток отвязки");

    let mut released = harness.inner_log.released_handles();
    released.sort_unstable();
    assert_eq!(released, vec![5, 6], "каждый кадр вернулся ровно один раз");
    assert_eq!(report.released_unfinished, 0);
    let stats = harness.binding.stats();
    assert_eq!(stats.released_at_renderer_detach_unfinished, 0);
    // Кадр вернул либо ожидатель, либо отвязка (гонка решается CAS): сумма всегда 2.
    assert_eq!(
        stats.released_by_waiter + stats.released_at_renderer_detach_after_fence,
        2
    );
    assert_eq!(stats.pending_now, 0);
}

#[test]
fn detach_twice_is_a_noop_that_changes_nothing() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(1);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    let first = harness.binding.renderer_detaching(SHORT_GRACE);
    assert!(!first.already_detached);
    let stats_after_first = harness.binding.stats();

    let second = harness.binding.renderer_detaching(SHORT_GRACE);

    assert!(second.already_detached);
    assert_eq!(second.released_after_fence + second.released_unfinished, 0);
    assert_eq!(harness.binding.stats(), stats_after_first);
    assert_eq!(harness.inner_log.released_handles(), vec![1]);
}

/// После отвязки запросы возвращаются сразу, поток не стартует, чужие fence не принимаются, а
/// счётчик причины свой (не путается с потерей контекста).
#[test]
fn requests_and_fences_after_detach_are_handled_immediately_without_holding_fence() {
    let harness = Harness::new();
    let report = harness.binding.renderer_detaching(SHORT_GRACE);
    assert!(!report.already_detached);
    let late_fence = FakeFence::pending();
    let observers = FenceObservers::watch(&late_fence);

    harness.publish(&late_fence);
    drop(late_fence);
    harness.request_release(9);

    assert!(
        !observers.is_alive(),
        "fence, опубликованный после отвязки, не принимается"
    );
    assert_eq!(harness.inner_log.released_handles(), vec![9]);
    let stats = harness.binding.stats();
    assert_eq!(stats.released_after_renderer_detach, 1);
    assert_eq!(stats.released_after_context_loss, 0);
    assert_eq!(
        harness.binding.core_for_tests().waiter_lifecycle(),
        WaiterLifecycle::NotStarted
    );
}

/// Нет ожидающих кадров: отвязка ничего не возвращает, но fence последнего кадра всё равно
/// отпускает (именно он держал display).
#[test]
fn detach_without_waiting_frames_still_drops_last_fence() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    let observers = FenceObservers::watch(&fence);
    harness.publish(&fence);
    drop(fence);

    let report = harness.binding.renderer_detaching(SHORT_GRACE);

    assert_eq!(report.released_after_fence, 0);
    assert_eq!(report.released_unfinished, 0);
    assert_eq!(report.released_after_fence_failure, 0);
    assert!(!observers.is_alive());
    assert_eq!(observers.destroyed_threads(), vec![thread::current().id()]);
    assert_eq!(harness.inner_log.released_handles(), Vec::<u64>::new());
}

/// Отказ ожидания fence при отвязке не теряет кадр и не возвращает его дважды.
#[test]
fn detach_with_failing_fence_releases_frame_once_as_failure() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(3);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    fence.fail_with_context_loss();

    harness.binding.renderer_detaching(SHORT_GRACE);

    assert!(harness.inner_log.wait_for_release_count(1, EVENT_TIMEOUT));
    thread::sleep(QUIET_WINDOW);
    assert_eq!(harness.inner_log.released_handles(), vec![3]);
    assert_eq!(harness.binding.stats().fence_wait_failures, 1);
}

/// Новый рендер на том же binding: после `resume_after_context_recreated` кадры снова ждут fence.
#[test]
fn binding_works_again_after_resume_following_detach() {
    let harness = Harness::new();
    harness.binding.renderer_detaching(SHORT_GRACE);
    harness.binding.resume_after_context_recreated();
    let fence = FakeFence::pending();
    harness.publish(&fence);

    harness.request_release(7);

    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), Vec::<u64>::new());
    fence.signal();
    assert!(harness.inner_log.wait_for_release_count(1, EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), vec![7]);
    assert_eq!(harness.binding.stats().released_by_waiter, 1);
}

/// Страховка: рендер забыл отвязаться. Пока поток-ожидатель ещё жив и держит общее ядро,
/// fence последнего кадра всё равно умирает в потоке, уничтожившем последний клон привязки, а
/// не в отсоединённом потоке-ожидателе (там он пережил бы `main`).
#[test]
fn dropping_last_binding_clone_destroys_last_fence_in_dropping_thread() {
    let harness = Harness::new();
    let earlier_fence = FakeFence::pending();
    harness.publish(&earlier_fence);
    harness.request_release(1);
    assert!(earlier_fence.wait_until_waited(EVENT_TIMEOUT));
    let last_fence = FakeFence::pending();
    let observers = FenceObservers::watch(&last_fence);
    harness.publish(&last_fence);
    drop(last_fence);
    let core = harness.binding.core_for_tests();
    assert!(observers.is_alive());

    // Ожидатель всё ещё ждёт `earlier_fence` и держит ядро, но привязки больше нет.
    drop(harness);

    assert!(
        !observers.is_alive(),
        "fence последнего кадра не должен дожидаться конца потока-ожидателя"
    );
    assert_eq!(observers.destroyed_threads(), vec![thread::current().id()]);
    earlier_fence.signal();
    assert!(core.wait_until_waiter_exited(EVENT_TIMEOUT));
}

/// После отвязки поток-ожидатель завершается, когда уничтожены все клоны привязки.
#[test]
fn waiter_exits_after_detach_and_dropping_all_clones() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(1);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    let core = harness.binding.core_for_tests();
    harness.binding.renderer_detaching(SHORT_GRACE);

    drop(harness);

    assert!(core.wait_until_waiter_exited(EVENT_TIMEOUT));
}
