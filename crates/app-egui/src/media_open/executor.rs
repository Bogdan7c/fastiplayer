//! Bounded owner blocking source-preparation worker-ов.

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

/// Один stale blocking preparation рядом с актуальным request — жёсткий D38 budget.
pub(crate) const MAX_NON_CANCELLABLE_STALE_PREPARATIONS: usize = 1;

/// Один слот принадлежит актуальному request-у, остальные — bounded stale work.
const PREPARATION_WORKER_COUNT: usize = MAX_NON_CANCELLABLE_STALE_PREPARATIONS + 1;

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

/// Bounded worker pool и capacity-one latest pending slot.
pub(super) struct PreparationExecutor {
    /// Workers захватывают только shared state, поэтому owner не образует Arc-cycle.
    shared: Arc<PreparationExecutorShared>,

    /// Физический предел одновременно работающих preparation-задач этого owner-а.
    worker_count: usize,

    /// Exact join authority всех bounded worker-ов остаётся у process owner-а.
    worker_handles: Mutex<Vec<thread::JoinHandle<()>>>,

    /// Повторный terminal call возвращает отдельный `AlreadyCompleted`.
    terminal_shutdown_completed: AtomicBool,
}

impl PreparationExecutor {
    pub(super) fn new(wake_port: AppWakePort) -> Arc<Self> {
        Self::new_with_worker_count(wake_port, PREPARATION_WORKER_COUNT)
    }

    /// Создаёт executor для speculative preload: физически не больше одного open одновременно.
    pub(super) fn new_single_worker(wake_port: AppWakePort) -> Arc<Self> {
        Self::new_with_worker_count(wake_port, 1)
    }

    fn new_with_worker_count(wake_port: AppWakePort, worker_count: usize) -> Arc<Self> {
        debug_assert!(worker_count > 0, "preparation executor обязан иметь worker");
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
            worker_count,
            worker_handles: Mutex::new(Vec::new()),
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
            let mut spawned_workers = Vec::with_capacity(self.worker_count);
            for worker_index in 0..self.worker_count {
                let worker_shared = Arc::clone(&self.shared);
                let worker_handle = match thread::Builder::new()
                    .name(format!("media-open-worker-{worker_index}"))
                    .spawn(move || Self::run(worker_shared))
                {
                    Ok(worker_handle) => worker_handle,
                    Err(_) => {
                        state.shutting_down = true;
                        self.shared.ready.notify_all();
                        self.retain_spawned_worker_handles(spawned_workers);
                        return Err(MediaOpenStartError::WorkerStartup);
                    }
                };
                spawned_workers.push(worker_handle);
            }
            match self.worker_handles.lock() {
                Ok(mut worker_handles) => worker_handles.extend(spawned_workers),
                Err(poisoned_worker_handles) => {
                    self.shared.state_lost.store(true, Ordering::Release);
                    poisoned_worker_handles.into_inner().extend(spawned_workers);
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

        let mut worker_handles = match self.worker_handles.lock() {
            Ok(worker_handles) => worker_handles,
            Err(poisoned_worker_handles) => {
                self.shared.state_lost.store(true, Ordering::Release);
                poisoned_worker_handles.into_inner()
            }
        };
        let mut pending_workers = Vec::new();
        let mut panicked_threads = 0_usize;
        for worker_handle in std::mem::take(&mut *worker_handles) {
            let mut worker_slot = Some(worker_handle);
            match join_thread_until(&mut worker_slot, deadline) {
                FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::Joined => {}
                FinishedThreadJoin::StillRunning => pending_workers
                    .push(worker_slot.expect("still-running worker сохраняет exact join handle")),
                FinishedThreadJoin::Panicked => panicked_threads += 1,
            }
        }
        *worker_handles = pending_workers;
        let pending_threads = worker_handles.len();
        if pending_threads > 0 {
            if panicked_threads > 0 {
                ProcessOwnerShutdownOutcome::ThreadPanicked {
                    panicked_threads,
                    pending_threads,
                }
            } else {
                ProcessOwnerShutdownOutcome::TimedOut { pending_threads }
            }
        } else {
            self.terminal_shutdown_completed
                .store(true, Ordering::Release);
            if panicked_threads > 0 {
                ProcessOwnerShutdownOutcome::ThreadPanicked {
                    panicked_threads,
                    pending_threads: 0,
                }
            } else {
                ProcessOwnerShutdownOutcome::Completed
            }
        }
    }

    /// Spawn failure не теряет join authority уже созданных worker-ов.
    fn retain_spawned_worker_handles(&self, spawned_workers: Vec<thread::JoinHandle<()>>) {
        match self.worker_handles.lock() {
            Ok(mut worker_handles) => worker_handles.extend(spawned_workers),
            Err(poisoned_worker_handles) => {
                self.shared.state_lost.store(true, Ordering::Release);
                poisoned_worker_handles.into_inner().extend(spawned_workers);
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
        let worker_handles = match self.worker_handles.get_mut() {
            Ok(worker_handles) => worker_handles,
            Err(poisoned_worker_handles) => poisoned_worker_handles.into_inner(),
        };

        // Fail-safe Drop не является bounded process path. В production terminal
        // coordinator обязан заранее вызвать `shutdown_until` и убрать handles.
        for worker_handle in worker_handles.drain(..) {
            let _worker_result = worker_handle.join();
        }
    }
}

#[cfg(test)]
mod shutdown_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

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
    fn single_worker_profile_never_runs_replacement_beside_stale_work() {
        let executor = PreparationExecutor::new_single_worker(AppWakePort::disconnected(
            AppWakeOwner::PlaylistRuntime,
        ));
        let first_started = Arc::new(AtomicBool::new(false));
        let release_first = Arc::new(AtomicBool::new(false));
        let first_started_by_worker = Arc::clone(&first_started);
        let release_first_for_worker = Arc::clone(&release_first);
        executor
            .submit_latest(PreparationWork::new(
                Arc::new(PreparationCancellation::new()),
                Arc::new(PreparationResultSlot::new()),
                move |_| {
                    first_started_by_worker.store(true, Ordering::Release);
                    while !release_first_for_worker.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    Err(MediaPreparationFailureKind::Cancelled)
                },
            ))
            .expect("first speculative work starts");
        let first_deadline = Instant::now() + Duration::from_secs(1);
        while !first_started.load(Ordering::Acquire) {
            assert!(Instant::now() < first_deadline, "first work did not start");
            std::thread::yield_now();
        }

        let replacement_started = Arc::new(AtomicBool::new(false));
        let replacement_started_by_worker = Arc::clone(&replacement_started);
        executor
            .submit_latest(PreparationWork::new(
                Arc::new(PreparationCancellation::new()),
                Arc::new(PreparationResultSlot::new()),
                move |_| {
                    replacement_started_by_worker.store(true, Ordering::Release);
                    Err(MediaPreparationFailureKind::Cancelled)
                },
            ))
            .expect("replacement is retained in the bounded latest slot");

        assert_eq!(
            executor
                .worker_handles
                .lock()
                .expect("worker handles mutex")
                .len(),
            1
        );
        assert!(
            !replacement_started.load(Ordering::Acquire),
            "replacement must not overlap a non-cooperative stale preparation"
        );

        release_first.store(true, Ordering::Release);
        let replacement_deadline = Instant::now() + Duration::from_secs(1);
        while !replacement_started.load(Ordering::Acquire) {
            assert!(
                Instant::now() < replacement_deadline,
                "replacement did not start after stale work released the only worker"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            executor.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::Completed
        );
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
                .worker_handles
                .lock()
                .expect("worker handles mutex")
                .iter()
                .any(|worker_handle| !worker_handle.is_finished())
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
        executor
            .worker_handles
            .lock()
            .expect("worker handles mutex")
            .push(std::thread::spawn(|| panic!("expected executor panic")));

        assert_eq!(
            executor.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1))),
            ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 0,
            }
        );
    }
}
