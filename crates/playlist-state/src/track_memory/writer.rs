use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::AtomicWriteOutcome;

use super::memory::TrackMemory;
use super::store::TrackMemoryStore;

/// Монотонная ревизия снимка (выдаёт владелец приложения); старее или равная — отбрасывается.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrackMemorySaveRevision(NonZeroU64);

impl TrackMemorySaveRevision {
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Неизменяемая копия памяти, которую фоновый поток запишет целиком.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackMemoryWriteSnapshot {
    revision: TrackMemorySaveRevision,
    memory: TrackMemory,
}

impl TrackMemoryWriteSnapshot {
    #[must_use]
    pub fn new(revision: TrackMemorySaveRevision, memory: TrackMemory) -> Self {
        Self { revision, memory }
    }

    #[must_use]
    pub const fn revision(&self) -> TrackMemorySaveRevision {
        self.revision
    }

    fn memory(&self) -> &TrackMemory {
        &self.memory
    }
}

/// Submission не смешивает accepted/no-op/disconnected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackMemorySubmitOutcome {
    Accepted,
    SameOrOlderRevision,
    Disconnected,
}

/// Последний physical attempt доступен runtime-у без worker-thread logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackMemoryWriteReport {
    pub revision: TrackMemorySaveRevision,
    pub outcome: AtomicWriteOutcome,
}

/// Thread spawn failure не теряет privacy-safe io category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackMemoryWriterStartError(pub std::io::ErrorKind);

impl fmt::Display for TrackMemoryWriterStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to launch track memory writer: {:?}",
            self.0
        )
    }
}

impl std::error::Error for TrackMemoryWriterStartError {}

/// Terminal completion различает bounded timeout и полный join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackMemoryWriterShutdownOutcome {
    Completed {
        final_report: Option<TrackMemoryWriteReport>,
    },
    TimedOut,
    WorkerUnavailable,
    AlreadyCompleted,
}

struct SharedState {
    pending: Option<TrackMemoryWriteSnapshot>,
    latest_accepted_revision: Option<TrackMemorySaveRevision>,
    latest_report: Option<TrackMemoryWriteReport>,
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

/// Фоновый writer «только последнее»: общий слот хранит newest value, capacity-one wake только будит thread.
pub struct TrackMemoryWriter {
    shared: Arc<Mutex<SharedState>>,
    wake_tx: SyncSender<()>,
    shutdown_requested: Arc<AtomicBool>,
    completion_rx: Receiver<Option<TrackMemoryWriteReport>>,
    join_handle: Option<JoinHandle<()>>,
    completed: bool,
}

impl TrackMemoryWriter {
    /// Запускает поток. Вызывать только если инспекция разрешила запись
    /// ([`super::TrackMemoryInspection::allows_saving_immediately`] или успешный карантин).
    pub fn start(store: Arc<TrackMemoryStore>) -> Result<Self, TrackMemoryWriterStartError> {
        let shared = Arc::new(Mutex::new(SharedState::new()));
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let thread_shared = shared.clone();
        let thread_shutdown = shutdown_requested.clone();
        let join_handle = thread::Builder::new()
            .name("track-memory-writer".to_owned())
            .spawn(move || {
                run_worker(
                    store,
                    thread_shared,
                    thread_shutdown,
                    wake_rx,
                    completion_tx,
                );
            })
            .map_err(|error| TrackMemoryWriterStartError(error.kind()))?;
        Ok(Self {
            shared,
            wake_tx,
            shutdown_requested,
            completion_rx,
            join_handle: Some(join_handle),
            completed: false,
        })
    }

    /// Заменяет ожидающий слот только strictly newer revision-ом.
    pub fn submit(&self, snapshot: TrackMemoryWriteSnapshot) -> TrackMemorySubmitOutcome {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return TrackMemorySubmitOutcome::Disconnected;
        }
        let mut shared = match self.shared.lock() {
            Ok(shared) => shared,
            Err(_) => return TrackMemorySubmitOutcome::Disconnected,
        };
        if shared
            .latest_accepted_revision
            .is_some_and(|latest| latest >= snapshot.revision())
        {
            return TrackMemorySubmitOutcome::SameOrOlderRevision;
        }
        shared.latest_accepted_revision = Some(snapshot.revision());
        shared.pending = Some(snapshot);
        drop(shared);
        match self.wake_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => TrackMemorySubmitOutcome::Accepted,
            Err(TrySendError::Disconnected(())) => TrackMemorySubmitOutcome::Disconnected,
        }
    }

    /// Возвращает newest report, сохраняя его для terminal durability proof.
    pub fn latest_report(&self) -> Option<TrackMemoryWriteReport> {
        self.shared.lock().ok()?.latest_report
    }

    /// Перед shutdown newest snapshot попадает в тот же latest slot и не теряется при Full wake.
    pub fn shutdown(
        &mut self,
        newest: Option<TrackMemoryWriteSnapshot>,
        timeout: Duration,
    ) -> TrackMemoryWriterShutdownOutcome {
        if self.completed {
            return TrackMemoryWriterShutdownOutcome::AlreadyCompleted;
        }
        if let Some(snapshot) = newest {
            let mut shared = match self.shared.lock() {
                Ok(shared) => shared,
                Err(_) => return TrackMemoryWriterShutdownOutcome::TimedOut,
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
            Err(_) => return TrackMemoryWriterShutdownOutcome::TimedOut,
        };
        let Some(join_handle) = self.join_handle.take() else {
            return TrackMemoryWriterShutdownOutcome::TimedOut;
        };
        if join_handle.join().is_err() {
            return TrackMemoryWriterShutdownOutcome::TimedOut;
        }
        self.completed = true;
        TrackMemoryWriterShutdownOutcome::Completed { final_report }
    }
}

fn run_worker(
    store: Arc<TrackMemoryStore>,
    shared: Arc<Mutex<SharedState>>,
    shutdown_requested: Arc<AtomicBool>,
    wake_rx: Receiver<()>,
    completion_tx: SyncSender<Option<TrackMemoryWriteReport>>,
) {
    loop {
        let pending = shared
            .lock()
            .ok()
            .and_then(|mut shared| shared.pending.take());
        if let Some(snapshot) = pending {
            let report = TrackMemoryWriteReport {
                revision: snapshot.revision(),
                outcome: store.write(snapshot.memory()),
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
