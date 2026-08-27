//! Event-driven readiness concrete progressive queue до type erasure.

use std::future::Future;
use std::sync::{Arc, Weak};
use std::task::{Context, Wake, Waker};
use std::time::Instant;

use source_core::CancellationToken;

use super::worker::ProgressiveSharedState;

/// Concrete wake boundary очереди до type erasure в `dyn Demuxer`.
#[derive(Clone)]
pub struct ProgressiveDemuxReadinessPort {
    /// Shared queue остаётся единственной authority readiness predicate-а.
    shared: Arc<ProgressiveSharedState>,
    /// Тот же lifecycle token отличает cancellation от неожиданной смерти worker-а.
    cancellation: CancellationToken,
}

/// Результат одного bounded event-driven ожидания progressive queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressiveDemuxReadiness {
    /// В очереди есть хотя бы одно сообщение current generation.
    EventAvailable,
    /// Source lifecycle отменён либо владелец demuxer-а запросил stop.
    Cancelled,
    /// Worker завершился, не оставив current-generation сообщения.
    WorkerStopped,
    /// Caller-owned monotonic deadline истёк без изменения predicate-а.
    DeadlineReached,
}

/// Waker связывает existing `CancellationFuture` с queue Condvar без executor/thread-а.
struct ProgressiveReadinessCancellationWake {
    /// Weak не продлевает lifetime queue после drop demuxer-а и worker-а.
    shared: Weak<ProgressiveSharedState>,
}

impl ProgressiveDemuxReadinessPort {
    /// Создаёт observation-only port до type erasure concrete progressive demuxer-а.
    pub(super) fn new(
        shared: Arc<ProgressiveSharedState>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            shared,
            cancellation,
        }
    }

    /// Ждёт current-generation message либо terminal state до monotonic deadline.
    ///
    /// Port ничего не извлекает из очереди: lifecycle snapshots и byte accounting
    /// по-прежнему меняет только `ProgressiveDemuxer::next_event()`.
    #[must_use]
    pub fn wait_until(&self, deadline: Instant) -> ProgressiveDemuxReadiness {
        let cancellation_wake = Arc::new(ProgressiveReadinessCancellationWake {
            shared: Arc::downgrade(&self.shared),
        });
        let cancellation_waker = Waker::from(cancellation_wake);
        let mut cancellation_context = Context::from_waker(&cancellation_waker);
        let mut cancellation_future = std::pin::pin!(self.cancellation.cancelled());
        let mut queue = self.shared.lock_queue();

        loop {
            let current_generation = queue.current_generation;
            if queue
                .messages
                .iter()
                .any(|message| message.generation == current_generation)
            {
                return ProgressiveDemuxReadiness::EventAvailable;
            }
            if queue.stop_requested || self.cancellation.is_cancelled() {
                return ProgressiveDemuxReadiness::Cancelled;
            }
            if queue.worker_stopped {
                return ProgressiveDemuxReadiness::WorkerStopped;
            }

            // Poll под тем же queue mutex-ом регистрирует wake до atomic unlock+wait.
            // Cancellation wake берёт этот mutex перед notify, поэтому сигнал не теряется
            // в окне между повторной проверкой predicate-а и Condvar::wait_timeout.
            self.shared.begin_readiness_cancellation_poll();
            let cancellation_ready = cancellation_future
                .as_mut()
                .poll(&mut cancellation_context)
                .is_ready();
            self.shared.finish_readiness_cancellation_poll();
            if cancellation_ready {
                return ProgressiveDemuxReadiness::Cancelled;
            }

            let now = Instant::now();
            if now >= deadline {
                return ProgressiveDemuxReadiness::DeadlineReached;
            }
            let wait_result = self
                .shared
                .message_available
                .wait_timeout(queue, deadline.saturating_duration_since(now));
            queue = match wait_result {
                Ok((queue, _)) => queue,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }
}

impl std::fmt::Debug for ProgressiveDemuxReadinessPort {
    /// Queue internals и cancellation state не становятся public diagnostics surface.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgressiveDemuxReadinessPort")
            .finish_non_exhaustive()
    }
}

impl Wake for ProgressiveReadinessCancellationWake {
    /// Cancellation caller синхронизируется с тем же queue mutex перед notification.
    fn wake(self: Arc<Self>) {
        self.notify_waiter();
    }

    /// Не создаёт новый Arc при обычном wake-by-reference path-е.
    fn wake_by_ref(self: &Arc<Self>) {
        self.notify_waiter();
    }
}

impl ProgressiveReadinessCancellationWake {
    /// Weak upgrade fail означает, что demuxer и worker уже освободили queue.
    fn notify_waiter(&self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        if shared.readiness_poll_is_owned_by_current_thread() {
            // Bounded waiter saturation может синхронно разбудить несколько readiness
            // waker-ов той же queue, пока poll уже держит её mutex. Poll вернёт Ready,
            // поэтому synchronous path не начнёт Condvar wait и recursive lock не нужен.
            shared.message_available.notify_all();
            return;
        }
        let _queue = shared.lock_queue();
        shared.message_available.notify_all();
    }
}
