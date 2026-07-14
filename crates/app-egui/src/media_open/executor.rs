//! Bounded owner единственного blocking source-preparation worker-а.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use player_core::MediaInstallCancellationCause;

use crate::app_wake::AppWakePort;

use super::{MediaOpenStartError, MediaPreparationFailureKind, PreparedMediaOpen};

/// Один running blocking preparation + один latest pending request — жёсткий D38 budget.
pub(crate) const MAX_NON_CANCELLABLE_STALE_PREPARATIONS: usize = 1;

pub(super) type PreparationResult = Result<PreparedMediaOpen, MediaPreparationFailureKind>;
type PreparationTask = Box<dyn FnOnce(&PreparationCancellation) -> PreparationResult + Send>;

/// Cooperative token хранит exact caller cause, а не безликий boolean.
pub(super) struct PreparationCancellation {
    cancelled: AtomicBool,
    state_lost: AtomicBool,
    cause: Mutex<Option<MediaInstallCancellationCause>>,
}

impl PreparationCancellation {
    pub(super) fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
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
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
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

/// Один process worker и capacity-one latest pending slot.
pub(super) struct PreparationExecutor {
    state: Mutex<PreparationExecutorState>,
    ready: Condvar,
    wake_port: AppWakePort,
    state_lost: AtomicBool,
}

impl PreparationExecutor {
    pub(super) fn new(wake_port: AppWakePort) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PreparationExecutorState {
                pending_latest: None,
                shutting_down: false,
                worker_started: false,
            }),
            ready: Condvar::new(),
            wake_port,
            state_lost: AtomicBool::new(false),
        })
    }

    pub(super) fn submit_latest(
        self: &Arc<Self>,
        work: PreparationWork,
    ) -> Result<(), MediaOpenStartError> {
        let mut state = self.state.lock().map_err(|_| {
            self.state_lost.store(true, Ordering::Release);
            MediaOpenStartError::ExecutorInvariant
        })?;
        if state.shutting_down {
            return Err(MediaOpenStartError::ShuttingDown);
        }
        if !state.worker_started {
            let worker_executor = Arc::clone(self);
            thread::Builder::new()
                .name("media-open-worker".to_owned())
                .spawn(move || worker_executor.run())
                .map_err(|_| MediaOpenStartError::WorkerStartup)?;
            state.worker_started = true;
        }
        if let Some(replaced) = state.pending_latest.replace(work) {
            replaced
                .cancellation
                .cancel(MediaInstallCancellationCause::Superseded);
        }
        self.ready.notify_one();
        Ok(())
    }

    pub(super) fn shutdown(&self) {
        let Ok(mut state) = self.state.lock() else {
            self.state_lost.store(true, Ordering::Release);
            let _wake_delivery = self.wake_port.request_wake();
            return;
        };
        state.shutting_down = true;
        if let Some(pending) = state.pending_latest.take() {
            pending
                .cancellation
                .cancel(MediaInstallCancellationCause::LifecycleShutdown);
        }
        self.ready.notify_all();
    }

    fn run(&self) {
        loop {
            let work = {
                let Ok(mut state) = self.state.lock() else {
                    self.state_lost.store(true, Ordering::Release);
                    let _wake_delivery = self.wake_port.request_wake();
                    return;
                };
                while state.pending_latest.is_none() && !state.shutting_down {
                    let Ok(waited_state) = self.ready.wait(state) else {
                        self.state_lost.store(true, Ordering::Release);
                        let _wake_delivery = self.wake_port.request_wake();
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
            let _wake_delivery = self.wake_port.request_wake();
        }
    }

    pub(super) fn state_was_lost(&self) -> bool {
        self.state_lost.load(Ordering::Acquire)
    }
}
