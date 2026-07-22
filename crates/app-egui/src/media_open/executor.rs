//! Bounded owner единственного blocking source-preparation worker-а.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use player_core::MediaInstallCancellationCause;

use crate::app_wake::AppWakePort;
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_thread_until,
};

use super::{MediaOpenStartError, MediaPreparationFailureKind, PreparedMediaOpen};

/// Один running blocking preparation + один latest pending request — жёсткий D38 budget.
pub(crate) const MAX_NON_CANCELLABLE_STALE_PREPARATIONS: usize = 1;

pub(super) type PreparationResult = Result<PreparedMediaOpen, MediaPreparationFailureKind>;
type PreparationTask = Box<dyn FnOnce(&PreparationCancellation) -> PreparationResult + Send>;

/// Cooperative token хранит exact caller cause, а не безликий boolean.
pub(super) struct PreparationCancellation {
    cancelled: AtomicBool,
    source_cancellation: source_core::CancellationToken,
    state_lost: AtomicBool,
    cause: Mutex<Option<MediaInstallCancellationCause>>,
}

impl PreparationCancellation {
    pub(super) fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            source_cancellation: source_core::CancellationToken::new(),
            state_lost: AtomicBool::new(false),
            cause: Mutex::new(None),
        }
    }

    pub(super) fn cancel(&self, cause: MediaInstallCancellationCause) {
        match self.cause.lock() {
            Ok(mut stored_cause) => {
                if stored_cause.is_none() {
                    *stored_cause = Some(cause);
                }
            }
            Err(_) => {
                self.state_lost.store(true, Ordering::Release);
            }
        }
        self.source_cancellation.cancel();
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Передаёт transport/demux слоям clone того же cooperative cancellation state.
    pub(super) fn source_token(&self) -> source_core::CancellationToken {
        self.source_cancellation.clone()
    }

    pub(super) fn cause(&self) -> Result<Option<MediaInstallCancellationCause>, ()> {
        if self.state_lost.load(Ordering::Acquire) {
            return Err(());
        }
        self.cause.lock().map(|cause| *cause).map_err(|_| ())
    }
}

/// Preallocated request-owned result slot: worker никогда не блокируется на send.
pub(super) struct PreparationResultSlot {
    state_lost: AtomicBool,
    result: Mutex<Option<PreparationResult>>,
    ready: Condvar,
}

impl PreparationResultSlot {
    pub(super) fn new() -> Self {
        Self {
            state_lost: AtomicBool::new(false),
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn publish(&self, result: PreparationResult) {
        match self.result.lock() {
            Ok(mut slot) if slot.is_none() => {
                *slot = Some(result);
                self.ready.notify_all();
            }
            Ok(_) | Err(_) => {
                self.state_lost.store(true, Ordering::Release);
                self.ready.notify_all();
            }
        }
    }

    pub(super) fn take(&self) -> Result<Option<PreparationResult>, ()> {
        if self.state_lost.load(Ordering::Acquire) {
            return Err(());
        }
        self.result
            .lock()
            .map(|mut result| result.take())
            .map_err(|_| ())
    }

    /// Ждёт availability без polling spin и оставляет payload coordinator drain-у.
    pub(super) fn wait_until_result_available(&self) -> Result<(), ()> {
        let mut result = self.result.lock().map_err(|_| ())?;
        loop {
            if self.state_lost.load(Ordering::Acquire) {
                return Err(());
            }
            if result.is_some() {
                return Ok(());
            }
            result = self.ready.wait(result).map_err(|_| ())?;
        }
    }
}

pub(super) struct PreparationWork {
    pub(super) cancellation: Arc<PreparationCancellation>,
    pub(super) result_slot: Arc<PreparationResultSlot>,
    pub(super) task: PreparationTask,
}

impl PreparationWork {
    pub(super) fn new(
        cancellation: Arc<PreparationCancellation>,
        result_slot: Arc<PreparationResultSlot>,
        task: impl FnOnce(&PreparationCancellation) -> PreparationResult + Send + 'static,
    ) -> Self {
        Self {
            cancellation,
            result_slot,
            task: Box::new(task),
        }
    }
}

struct PreparationExecutorState {
    pending_latest: Option<PreparationWork>,
    shutting_down: bool,
    worker_started: bool,
}

/// Разделяемое worker-состояние не владеет JoinHandle своего потока.
struct PreparationExecutorShared {
    state: Mutex<PreparationExecutorState>,
    ready: Condvar,
    wake_port: AppWakePort,
    state_lost: AtomicBool,
}

/// Один process worker и capacity-one latest pending slot.
pub(super) struct PreparationExecutor {
    /// Worker захватывает только shared state, поэтому owner не образует Arc-cycle.
    shared: Arc<PreparationExecutorShared>,

    /// Exact join authority единственного worker-а остаётся у process owner-а.
    worker_handle: Mutex<Option<thread::JoinHandle<()>>>,

    /// Повторный terminal call возвращает отдельный `AlreadyCompleted`.
    terminal_shutdown_completed: AtomicBool,
}

impl PreparationExecutor {
    pub(super) fn new(wake_port: AppWakePort) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(PreparationExecutorShared {
                state: Mutex::new(PreparationExecutorState {
                    pending_latest: None,
                    shutting_down: false,
                    worker_started: false,
                }),
                ready: Condvar::new(),
                wake_port,
                state_lost: AtomicBool::new(false),
            }),
            worker_handle: Mutex::new(None),
            terminal_shutdown_completed: AtomicBool::new(false),
        })
    }

    pub(super) fn submit_latest(
        self: &Arc<Self>,
        work: PreparationWork,
    ) -> Result<(), MediaOpenStartError> {
        let mut state = self.shared.state.lock().map_err(|_| {
            self.shared.state_lost.store(true, Ordering::Release);
            MediaOpenStartError::ExecutorInvariant
        })?;
        if state.shutting_down {
            return Err(MediaOpenStartError::ShuttingDown);
        }
        if !state.worker_started {
            let worker_shared = Arc::clone(&self.shared);
            let worker_handle = thread::Builder::new()
                .name("media-open-worker".to_owned())
                .spawn(move || Self::run(worker_shared))
                .map_err(|_| MediaOpenStartError::WorkerStartup)?;
            match self.worker_handle.lock() {
                Ok(mut handle_slot) => *handle_slot = Some(worker_handle),
                Err(poisoned_handle_slot) => {
                    self.shared.state_lost.store(true, Ordering::Release);
                    *poisoned_handle_slot.into_inner() = Some(worker_handle);
                    state.shutting_down = true;
                    self.shared.ready.notify_all();
                    return Err(MediaOpenStartError::ExecutorInvariant);
                }
            }
            state.worker_started = true;
        }
        if let Some(replaced) = state.pending_latest.replace(work) {
            replaced
                .cancellation
                .cancel(MediaInstallCancellationCause::Superseded);
        }
        self.shared.ready.notify_one();
        Ok(())
    }

    pub(super) fn shutdown(&self) {
        let Ok(mut state) = self.shared.state.lock() else {
            self.shared.state_lost.store(true, Ordering::Release);
            let _wake_delivery = self.shared.wake_port.request_wake();
            return;
        };
        state.shutting_down = true;
        if let Some(pending) = state.pending_latest.take() {
            pending
                .cancellation
                .cancel(MediaInstallCancellationCause::LifecycleShutdown);
        }
        self.shared.ready.notify_all();
    }

    /// Выполняет terminal bounded join; timeout сохраняет handle у executor-а.
    pub(super) fn shutdown_until(&self, deadline: ShutdownDeadline) -> ProcessOwnerShutdownOutcome {
        if self.terminal_shutdown_completed.load(Ordering::Acquire) {
            return ProcessOwnerShutdownOutcome::AlreadyCompleted;
        }
        self.shutdown();

        let mut handle_slot = match self.worker_handle.lock() {
            Ok(handle_slot) => handle_slot,
            Err(poisoned_handle_slot) => {
                self.shared.state_lost.store(true, Ordering::Release);
                poisoned_handle_slot.into_inner()
            }
        };
        match join_thread_until(&mut handle_slot, deadline) {
            FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::Joined => {
                self.terminal_shutdown_completed
                    .store(true, Ordering::Release);
                ProcessOwnerShutdownOutcome::Completed
            }
            FinishedThreadJoin::StillRunning => {
                ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
            }
            FinishedThreadJoin::Panicked => {
                self.terminal_shutdown_completed
                    .store(true, Ordering::Release);
                ProcessOwnerShutdownOutcome::ThreadPanicked {
                    panicked_threads: 1,
                    pending_threads: 0,
                }
            }
        }
    }

    fn run(shared: Arc<PreparationExecutorShared>) {
        loop {
            let work = {
                let Ok(mut state) = shared.state.lock() else {
                    shared.state_lost.store(true, Ordering::Release);
                    let _wake_delivery = shared.wake_port.request_wake();
                    return;
                };
                while state.pending_latest.is_none() && !state.shutting_down {
                    let Ok(waited_state) = shared.ready.wait(state) else {
                        shared.state_lost.store(true, Ordering::Release);
                        let _wake_delivery = shared.wake_port.request_wake();
                        return;
                    };
                    state = waited_state;
                }
                if state.shutting_down && state.pending_latest.is_none() {
                    return;
                }
                state.pending_latest.take()
            };
            let Some(work) = work else {
                continue;
            };
            let result = if work.cancellation.is_cancelled() {
                Err(MediaPreparationFailureKind::Cancelled)
            } else {
                catch_unwind(AssertUnwindSafe(|| (work.task)(&work.cancellation)))
                    .unwrap_or(Err(MediaPreparationFailureKind::WorkerPanicked))
            };
            work.result_slot.publish(result);
            let _wake_delivery = shared.wake_port.request_wake();
        }
    }

    pub(super) fn state_was_lost(&self) -> bool {
        self.shared.state_lost.load(Ordering::Acquire)
    }
}

impl Drop for PreparationExecutor {
    fn drop(&mut self) {
        self.shutdown();
        let handle_slot = match self.worker_handle.get_mut() {
            Ok(handle_slot) => handle_slot,
            Err(poisoned_handle_slot) => poisoned_handle_slot.into_inner(),
        };
        let Some(worker_handle) = handle_slot.take() else {
            return;
        };

        // Fail-safe Drop не является bounded process path. В production terminal
        // coordinator обязан заранее вызвать `shutdown_until` и убрать handle.
        let _worker_result = worker_handle.join();
    }
}

#[cfg(test)]
mod shutdown_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

    fn disconnected_executor() -> Arc<PreparationExecutor> {
        PreparationExecutor::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime))
    }

    #[test]
    fn idle_executor_shutdown_is_idempotent() {
        let executor = disconnected_executor();

        assert_eq!(
            executor.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
        assert_eq!(
            executor.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn cancellation_reaches_source_transport_token() {
        let cancellation = PreparationCancellation::new();
        let source_token = cancellation.source_token();

        cancellation.cancel(MediaInstallCancellationCause::LifecycleShutdown);

        assert!(cancellation.is_cancelled());
        assert!(source_token.is_cancelled());
    }

    #[test]
    fn timeout_retains_worker_handle_and_later_reaps_it() {
        let executor = disconnected_executor();
        let release = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let worker_started = Arc::clone(&started);
        executor
            .submit_latest(PreparationWork::new(
                Arc::new(PreparationCancellation::new()),
                Arc::new(PreparationResultSlot::new()),
                move |_| {
                    worker_started.store(true, Ordering::Release);
                    while !worker_release.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    Err(MediaPreparationFailureKind::Cancelled)
                },
            ))
            .expect("test work должен стартовать");
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }

        assert_eq!(
            executor.shutdown_until(ShutdownDeadline::after(Duration::from_millis(1))),
            ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
        );
        assert!(
            executor
                .worker_handle
                .lock()
                .expect("worker handle mutex")
                .is_some()
        );

        release.store(true, Ordering::Release);
        assert_eq!(
            executor.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
    }

    #[test]
    fn worker_thread_panic_is_typed() {
        let executor = disconnected_executor();
        *executor.worker_handle.lock().expect("worker handle mutex") =
            Some(std::thread::spawn(|| panic!("expected executor panic")));

        assert_eq!(
            executor.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 0,
            }
        );
    }
}
