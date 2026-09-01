use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::AtomicWriteOutcome;

use super::{PlaylistResumeStore, ResumeCheckpoint};

/// Monotonic app-owned revision latest-only checkpoint-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResumeSaveRevision(NonZeroU64);

impl ResumeSaveRevision {
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Immutable owned snapshot: `None` намеренно сериализуется как tombstone `null`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeWriteSnapshot {
    revision: ResumeSaveRevision,
    checkpoint: Option<ResumeCheckpoint>,
}

impl ResumeWriteSnapshot {
    #[must_use]
    pub fn new(revision: ResumeSaveRevision, checkpoint: Option<ResumeCheckpoint>) -> Self {
        Self {
            revision,
            checkpoint,
        }
    }

    #[must_use]
    pub const fn revision(&self) -> ResumeSaveRevision {
        self.revision
    }

    pub(super) fn checkpoint(&self) -> Option<&ResumeCheckpoint> {
        self.checkpoint.as_ref()
    }
}

/// Submission не смешивает accepted/no-op/disconnected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeSubmitOutcome {
    Accepted,
    SameOrOlderRevision,
    Disconnected,
}

/// Последний physical attempt доступен runtime-у без worker-thread logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResumeWriteReport {
    pub revision: ResumeSaveRevision,
    pub outcome: AtomicWriteOutcome,
}

/// Thread spawn failure не теряет privacy-safe io category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResumeWorkerStartError(pub std::io::ErrorKind);

impl fmt::Display for ResumeWorkerStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "не удалось запустить playlist resume writer: {:?}",
            self.0
        )
    }
}

impl std::error::Error for ResumeWorkerStartError {}

/// Terminal completion различает bounded timeout и полный join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeWorkerShutdownOutcome {
    Completed {
        final_report: Option<ResumeWriteReport>,
    },
    TimedOut,
    WorkerUnavailable,
    AlreadyCompleted,
}

struct SharedState {
    pending: Option<ResumeWriteSnapshot>,
    latest_accepted_revision: Option<ResumeSaveRevision>,
    latest_report: Option<ResumeWriteReport>,
}

impl SharedState {
    fn new() -> Self {
        Self {
            pending: None,
            latest_accepted_revision: None,
            latest_report: None,
        }
    }
}

/// Latest-only writer: shared slot хранит newest value, capacity-one wake только будит thread.
pub struct ResumeWorker {
    shared: Arc<Mutex<SharedState>>,
    wake_tx: SyncSender<()>,
    shutdown_requested: Arc<AtomicBool>,
    completion_rx: Receiver<Option<ResumeWriteReport>>,
    join_handle: Option<JoinHandle<()>>,
    completed: bool,
}

impl ResumeWorker {
    pub fn start(store: Arc<PlaylistResumeStore>) -> Result<Self, ResumeWorkerStartError> {
        let shared = Arc::new(Mutex::new(SharedState::new()));
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let thread_shared = shared.clone();
        let thread_shutdown = shutdown_requested.clone();
        let join_handle = thread::Builder::new()
            .name("playlist-resume-writer".to_owned())
            .spawn(move || {
                run_worker(
                    store,
                    thread_shared,
                    thread_shutdown,
                    wake_rx,
                    completion_tx,
                );
            })
            .map_err(|error| ResumeWorkerStartError(error.kind()))?;
        Ok(Self {
            shared,
            wake_tx,
            shutdown_requested,
            completion_rx,
            join_handle: Some(join_handle),
            completed: false,
        })
    }

    /// Заменяет pending slot только strictly newer revision-ом.
    pub fn submit(&self, snapshot: ResumeWriteSnapshot) -> ResumeSubmitOutcome {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return ResumeSubmitOutcome::Disconnected;
        }
        let mut shared = match self.shared.lock() {
            Ok(shared) => shared,
            Err(_) => return ResumeSubmitOutcome::Disconnected,
        };
        if shared
            .latest_accepted_revision
            .is_some_and(|latest| latest >= snapshot.revision())
        {
            return ResumeSubmitOutcome::SameOrOlderRevision;
        }
        shared.latest_accepted_revision = Some(snapshot.revision());
        shared.pending = Some(snapshot);
        drop(shared);
        match self.wake_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => ResumeSubmitOutcome::Accepted,
            Err(TrySendError::Disconnected(())) => ResumeSubmitOutcome::Disconnected,
        }
    }

    /// Возвращает newest report, сохраняя его для terminal durability proof.
    pub fn latest_report(&self) -> Option<ResumeWriteReport> {
        self.shared.lock().ok()?.latest_report
    }

    /// Перед shutdown newest snapshot попадает в тот же latest slot и не теряется при Full wake.
    pub fn shutdown(
        &mut self,
        newest: Option<ResumeWriteSnapshot>,
        timeout: Duration,
    ) -> ResumeWorkerShutdownOutcome {
        if self.completed {
            return ResumeWorkerShutdownOutcome::AlreadyCompleted;
        }
        if let Some(snapshot) = newest {
            let mut shared = match self.shared.lock() {
                Ok(shared) => shared,
                Err(_) => return ResumeWorkerShutdownOutcome::TimedOut,
            };
            if shared
                .latest_accepted_revision
                .is_none_or(|latest| latest < snapshot.revision())
            {
                shared.latest_accepted_revision = Some(snapshot.revision());
                shared.pending = Some(snapshot);
            }
        }
        self.shutdown_requested.store(true, Ordering::Release);
        let _wake_is_coalesced_or_delivered = self.wake_tx.try_send(());
        let final_report = match self.completion_rx.recv_timeout(timeout) {
            Ok(report) => report,
            Err(_) => return ResumeWorkerShutdownOutcome::TimedOut,
        };
        let Some(join_handle) = self.join_handle.take() else {
            return ResumeWorkerShutdownOutcome::TimedOut;
        };
        if join_handle.join().is_err() {
            return ResumeWorkerShutdownOutcome::TimedOut;
        }
        self.completed = true;
        ResumeWorkerShutdownOutcome::Completed { final_report }
    }
}

fn run_worker(
    store: Arc<PlaylistResumeStore>,
    shared: Arc<Mutex<SharedState>>,
    shutdown_requested: Arc<AtomicBool>,
    wake_rx: Receiver<()>,
    completion_tx: SyncSender<Option<ResumeWriteReport>>,
) {
    loop {
        let pending = shared
            .lock()
            .ok()
            .and_then(|mut shared| shared.pending.take());
        if let Some(snapshot) = pending {
            let report = ResumeWriteReport {
                revision: snapshot.revision(),
                outcome: store.write_snapshot(&snapshot),
            };
            if let Ok(mut shared) = shared.lock() {
                shared.latest_report = Some(report);
            } else {
                break;
            }
            continue;
        }
        if shutdown_requested.load(Ordering::Acquire) {
            let final_report = shared.lock().ok().and_then(|shared| shared.latest_report);
            let _completion_visible = completion_tx.send(final_report);
            break;
        }
        if wake_rx.recv().is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests;
