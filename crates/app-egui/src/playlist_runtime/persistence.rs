//! Process-lifetime wiring между playlist controller и `playlist-state` worker.
//!
//! Модуль не принимает UI-решения и не владеет queue: он только снимает immutable
//! snapshot после committed mutation, передаёт его worker-у и хранит privacy-safe
//! read model для будущего UI.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::SystemTime;

use playlist_state::{
    AtomicWriteOutcome, DurabilityRetryOutcome, ImmutableSaveSnapshot, PlaylistStateSnapshot,
    PlaylistStateStore, QuarantineFileName, SaveAttemptOutcome, SaveBlockReason, SaveControlError,
    SaveDebounce, SaveRevision, SaveWakePort, SaveWarning, SaveWarningFailure, SaveWorker,
    SaveWorkerAccess, SaveWorkerEvent, SaveWorkerShutdownOutcome, SaveWorkerStartError,
    SaveWorkerStartOutcome, ShutdownCompletion, ShutdownPersistenceOutcome, ShutdownTimeoutPhase,
    SubmitSnapshotError, SubmitSnapshotOutcome, WakePortDisconnected,
};

use crate::process_shutdown::ShutdownDeadline;

use super::settings::PlaylistSaveDebouncePort;
use super::{PlaylistController, PlaylistLineagePersistence, PlaylistOwnerPorts};

/// Injectable policy отделяет wall clock от quarantine workflow и focused tests.
pub(super) trait QuarantineNamePolicy {
    fn next_name(&mut self) -> QuarantineFileName;
}

/// Production policy создаёт collision-resistant имя из текущего wall clock.
pub(super) struct SystemQuarantineNamePolicy;

impl QuarantineNamePolicy for SystemQuarantineNamePolicy {
    fn next_name(&mut self) -> QuarantineFileName {
        QuarantineFileName::from_timestamp(SystemTime::now())
    }
}

/// Последний подтверждённый filesystem outcome не смешивает replace и durability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistSaveDurability {
    /// Persistent mutation ещё не публиковалась.
    NoCommittedSnapshot,
    /// Snapshot принят либо удерживается latest-only owner-ом до worker admission.
    Pending { revision: SaveRevision },
    /// Target не заменён; прежний файл остаётся authoritative.
    NotReplaced { revision: SaveRevision },
    /// Rename состоялся, но directory durability ещё не подтверждена.
    ReplacedDurabilityUnconfirmed { revision: SaveRevision },
    /// Snapshot и directory entry подтверждены доступными OS primitives.
    Durable { revision: SaveRevision },
}

/// App-side failure category не раскрывает путь или media locator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistPersistenceFault {
    SnapshotCapture,
    WorkerBackpressure,
    WorkerDisconnected,
    WorkerSubmissionStatePoisoned,
    WorkerCommandInvariant,
    WorkerStart,
    WorkerWakeDisconnected,
}

/// Read-only persistence model доступен renderer/UI без ownership worker-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistPersistenceView {
    pub(crate) save_block: Option<SaveBlockReason>,
    pub(crate) latest_committed_revision: Option<SaveRevision>,
    pub(crate) durability: PlaylistSaveDurability,
    pub(crate) warning: Option<SaveWarning>,
    pub(crate) fault: Option<PlaylistPersistenceFault>,
}

/// Terminal outcome persistence owner-а сохраняет точный worker protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistPersistenceShutdownOutcome {
    /// Writer не запускался: обычно это protected save-block либо clean fake owner.
    CompletedWithoutWorker { save_block: Option<SaveBlockReason> },
    /// Worker подтвердил final filesystem state и завершил поток.
    Completed(ShutdownCompletion),
    /// Owner уже полностью завершён предыдущим вызовом.
    AlreadyCompleted,
    /// Worker был нужен для committed revision, но отсутствовал.
    WriterUnavailable { revision: SaveRevision },
    /// Снять committed-only snapshot той же revision не удалось.
    SnapshotCaptureFailed {
        revision: SaveRevision,
        completion: ShutdownCompletion,
    },
    /// Deadline истёк внутри consuming worker API; process обязан завершиться с lease.
    TimedOut {
        phase: ShutdownTimeoutPhase,
        completion: Option<ShutdownCompletion>,
    },
    /// Worker завершил filesystem protocol, затем сообщил panic при join.
    ThreadPanicked(ShutdownCompletion),
}

impl Default for PlaylistPersistenceView {
    fn default() -> Self {
        Self {
            save_block: None,
            latest_committed_revision: None,
            durability: PlaylistSaveDurability::NoCommittedSnapshot,
            warning: None,
            fault: None,
        }
    }
}

/// Shared UI-thread cell позволяет Session 13 adapter-у reschedule-ить тот же worker.
struct SaveDebounceRuntime {
    configured: SaveDebounce,
    worker: Option<SaveWorker>,
}

/// Settings owner держит только weak intent port и не продлевает writer lifecycle.
pub(super) struct WorkerSaveDebouncePort {
    runtime: Weak<RefCell<SaveDebounceRuntime>>,
}

impl PlaylistSaveDebouncePort for WorkerSaveDebouncePort {
    fn reschedule_debounce(&mut self, debounce_ms: u64) -> Result<(), String> {
        let requested = SaveDebounce::new(std::time::Duration::from_millis(debounce_ms))
            .map_err(|error| error.to_string())?;
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| "playlist persistence owner is unavailable".to_owned())?;
        let mut runtime = runtime.borrow_mut();
        if let Some(worker) = runtime.worker.as_ref() {
            worker
                .reschedule_debounce(requested)
                .map_err(save_control_error_message)?;
        }
        runtime.configured = requested;
        Ok(())
    }
}

/// Worker wake публикует marker через единый PlaylistRuntime owner mailbox.
struct PlaylistSaveWakePort {
    owner_ports: PlaylistOwnerPorts,
}

impl SaveWakePort for PlaylistSaveWakePort {
    fn wake_save_worker(&self) -> Result<(), WakePortDisconnected> {
        self.owner_ports
            .publish_progress()
            .then_some(())
            .ok_or(WakePortDisconnected)
    }
}

/// Единственный app-side owner concrete store, worker и latest-only snapshot.
pub(super) struct PlaylistPersistenceOwner {
    store: Option<Arc<PlaylistStateStore>>,
    save_runtime: Rc<RefCell<SaveDebounceRuntime>>,
    latest_revision: Option<SaveRevision>,
    pending_snapshot: Option<ImmutableSaveSnapshot>,
    quarantine_name_policy: Box<dyn QuarantineNamePolicy>,
    view: PlaylistPersistenceView,
    shutdown_complete: bool,
    exit_required_outcome: Option<PlaylistPersistenceShutdownOutcome>,
}

impl PlaylistPersistenceOwner {
    pub(super) fn new(debounce_ms: u64) -> Self {
        let configured = SaveDebounce::new(std::time::Duration::from_millis(debounce_ms))
            .expect("validated PlaylistConfig must contain a valid state save debounce");
        Self {
            store: None,
            save_runtime: Rc::new(RefCell::new(SaveDebounceRuntime {
                configured,
                worker: None,
            })),
            latest_revision: None,
            pending_snapshot: None,
            quarantine_name_policy: Box::new(SystemQuarantineNamePolicy),
            view: PlaylistPersistenceView::default(),
            shutdown_complete: false,
            exit_required_outcome: None,
        }
    }

    pub(super) fn debounce_port(&self) -> Box<dyn PlaylistSaveDebouncePort> {
        Box::new(WorkerSaveDebouncePort {
            runtime: Rc::downgrade(&self.save_runtime),
        })
    }

    pub(super) fn install_store(&mut self, store: Arc<PlaylistStateStore>) {
        self.store = Some(store);
    }

    pub(super) fn next_quarantine_file_name(&mut self) -> QuarantineFileName {
        self.quarantine_name_policy.next_name()
    }

    #[cfg(test)]
    pub(super) fn replace_quarantine_name_policy(&mut self, policy: Box<dyn QuarantineNamePolicy>) {
        self.quarantine_name_policy = policy;
    }

    pub(super) fn start_for_lineage(
        &mut self,
        lineage: PlaylistLineagePersistence,
        owner_ports: PlaylistOwnerPorts,
    ) -> Result<(), SaveWorkerStartError> {
        if self.save_runtime.borrow().worker.is_some() || self.view.save_block.is_some() {
            return Ok(());
        }
        let access = match lineage {
            PlaylistLineagePersistence::Persistent => SaveWorkerAccess::Writable,
            PlaylistLineagePersistence::NonPersistent { save_block, .. } => {
                self.view.save_block = Some(save_block);
                SaveWorkerAccess::SaveBlocked(save_block)
            }
        };
        let Some(store) = self.store.clone() else {
            // Fake startup stores intentionally have no production writer.
            return Ok(());
        };
        let debounce = self.save_runtime.borrow().configured;
        let wake_port: Arc<dyn SaveWakePort> = Arc::new(PlaylistSaveWakePort { owner_ports });
        match SaveWorker::start(access, store, debounce, wake_port)? {
            SaveWorkerStartOutcome::Started(worker) => {
                self.save_runtime.borrow_mut().worker = Some(worker);
            }
            SaveWorkerStartOutcome::SaveBlocked(reason) => {
                self.view.save_block = Some(reason);
            }
        }
        Ok(())
    }

    pub(super) fn record_worker_start_error(&mut self, error: &SaveWorkerStartError) {
        tracing::error!(error = %error, "Не удалось запустить playlist state save worker");
        self.view.fault = Some(PlaylistPersistenceFault::WorkerStart);
    }

    /// Снимает queue, traversal, repeat и allocator watermark до следующей mutation.
    pub(super) fn publish_committed_controller(&mut self, controller: &PlaylistController) {
        if self.view.save_block.is_some() {
            return;
        }
        let revision = match self.latest_revision {
            Some(previous) => match previous.checked_next() {
                Ok(next) => next,
                Err(_) => {
                    self.view.fault = Some(PlaylistPersistenceFault::SnapshotCapture);
                    return;
                }
            },
            None => SaveRevision::FIRST,
        };
        let snapshot = PlaylistStateSnapshot::new(controller.queue(), controller.repeat_mode);
        let immutable = match ImmutableSaveSnapshot::capture(revision, snapshot) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(error = %error, "Не удалось снять immutable playlist snapshot");
                self.view.fault = Some(PlaylistPersistenceFault::SnapshotCapture);
                return;
            }
        };
        self.latest_revision = Some(revision);
        self.view.latest_committed_revision = Some(revision);
        self.view.durability = PlaylistSaveDurability::Pending { revision };
        self.view.fault = None;
        self.submit_or_retain(immutable);
    }

    fn submit_or_retain(&mut self, snapshot: ImmutableSaveSnapshot) {
        let runtime = self.save_runtime.borrow();
        let Some(worker) = runtime.worker.as_ref() else {
            drop(runtime);
            self.pending_snapshot = Some(snapshot);
            return;
        };
        match worker.submit_snapshot(snapshot) {
            Ok(
                SubmitSnapshotOutcome::Accepted | SubmitSnapshotOutcome::NoOpSameOrOlderRevision,
            ) => {
                self.pending_snapshot = None;
            }
            Err(SubmitSnapshotError::Backpressure(snapshot)) => {
                self.pending_snapshot = Some(*snapshot);
                self.view.fault = Some(PlaylistPersistenceFault::WorkerBackpressure);
            }
            Err(SubmitSnapshotError::Disconnected(snapshot)) => {
                self.pending_snapshot = Some(*snapshot);
                self.view.fault = Some(PlaylistPersistenceFault::WorkerDisconnected);
            }
            Err(SubmitSnapshotError::SubmissionStatePoisoned(snapshot)) => {
                self.pending_snapshot = Some(*snapshot);
                self.view.fault = Some(PlaylistPersistenceFault::WorkerSubmissionStatePoisoned);
            }
            Err(SubmitSnapshotError::CommandTypeInvariantLost) => {
                self.view.fault = Some(PlaylistPersistenceFault::WorkerCommandInvariant);
            }
        }
    }

    pub(super) fn flush_pending_submission(&mut self) {
        if let Some(snapshot) = self.pending_snapshot.take() {
            self.submit_or_retain(snapshot);
        }
    }

    /// Возвращает true только если read-only UI model действительно изменился.
    pub(super) fn drain_worker_events(&mut self) -> bool {
        let before = self.view;
        let events = self
            .save_runtime
            .borrow()
            .worker
            .as_ref()
            .map(SaveWorker::drain_events)
            .unwrap_or_default();
        for event in events {
            self.apply_worker_event(event);
        }
        self.flush_pending_submission();
        self.view != before
    }

    fn apply_worker_event(&mut self, event: SaveWorkerEvent) {
        match event {
            SaveWorkerEvent::AttemptCompleted(report) => {
                if self
                    .view
                    .latest_committed_revision
                    .is_some_and(|latest| report.revision < latest)
                {
                    return;
                }
                self.view.durability = match report.outcome {
                    SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::NotReplaced(_)) => {
                        PlaylistSaveDurability::NotReplaced {
                            revision: report.revision,
                        }
                    }
                    SaveAttemptOutcome::FullWrite(
                        AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(_),
                    )
                    | SaveAttemptOutcome::DirectoryDurabilityRetry(
                        DurabilityRetryOutcome::StillUnconfirmed(_),
                    ) => PlaylistSaveDurability::ReplacedDurabilityUnconfirmed {
                        revision: report.revision,
                    },
                    SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::Durable)
                    | SaveAttemptOutcome::DirectoryDurabilityRetry(
                        DurabilityRetryOutcome::Durable,
                    ) => PlaylistSaveDurability::Durable {
                        revision: report.revision,
                    },
                };
            }
            SaveWorkerEvent::WarningChanged(warning) => self.view.warning = warning,
            SaveWorkerEvent::WorkerDisconnected(_) => {
                self.view.fault = Some(PlaylistPersistenceFault::WorkerDisconnected);
            }
            SaveWorkerEvent::WakePortDisconnected => {
                self.view.fault = Some(PlaylistPersistenceFault::WorkerWakeDisconnected);
            }
        }
    }

    #[allow(
        dead_code,
        reason = "manual Retry UI intent is exposed by PlaylistRuntime"
    )]
    pub(super) fn retry_now(&self) -> Result<(), SaveControlError> {
        self.save_runtime
            .borrow()
            .worker
            .as_ref()
            .ok_or(SaveControlError::Disconnected)?
            .retry_now()
    }

    #[allow(dead_code, reason = "read-only model is exposed by PlaylistRuntime")]
    pub(super) const fn view(&self) -> PlaylistPersistenceView {
        self.view
    }

    pub(super) fn has_background_work(&self) -> bool {
        let worker_can_progress = self.save_runtime.borrow().worker.is_some()
            && !matches!(
                self.view.fault,
                Some(
                    PlaylistPersistenceFault::WorkerStart
                        | PlaylistPersistenceFault::WorkerDisconnected
                )
            );
        worker_can_progress
            && matches!(
                self.view.durability,
                PlaylistSaveDurability::Pending { .. }
                    | PlaylistSaveDurability::NotReplaced { .. }
                    | PlaylistSaveDurability::ReplacedDurabilityUnconfirmed { .. }
            )
    }

    /// Немедленно flush-ит только newest committed state в рамках общего deadline.
    ///
    /// Revision повторно не увеличивается: shutdown capture описывает тот же domain
    /// commit, а не создаёт синтетическую playlist mutation.
    pub(super) fn shutdown_until(
        &mut self,
        controller: Option<&PlaylistController>,
        deadline: ShutdownDeadline,
    ) -> PlaylistPersistenceShutdownOutcome {
        if let Some(outcome) = self.exit_required_outcome {
            return outcome;
        }
        if self.shutdown_complete {
            return PlaylistPersistenceShutdownOutcome::AlreadyCompleted;
        }

        // Сначала забираем уже опубликованные lossless reports, чтобы terminal
        // warning occurrence продолжал существующую последовательность ошибок.
        self.drain_worker_events();

        let snapshot_capture = self.latest_revision.map(|revision| {
            let controller = controller.ok_or(revision)?;
            ImmutableSaveSnapshot::capture(
                revision,
                PlaylistStateSnapshot::new(controller.queue(), controller.repeat_mode),
            )
            .map_err(|_| revision)
        });
        let (newest_committed, capture_failure) = match snapshot_capture {
            Some(Ok(snapshot)) => (Some(snapshot), None),
            Some(Err(revision)) => {
                self.view.fault = Some(PlaylistPersistenceFault::SnapshotCapture);
                tracing::error!(
                    revision = revision.value(),
                    "Не удалось снять committed-only playlist snapshot при shutdown"
                );
                (None, Some(revision))
            }
            None => (None, None),
        };
        self.pending_snapshot = None;

        let worker = self.save_runtime.borrow_mut().worker.take();
        let Some(worker) = worker else {
            self.shutdown_complete = true;
            if let Some(revision) = self.latest_revision {
                return PlaylistPersistenceShutdownOutcome::WriterUnavailable { revision };
            }
            return PlaylistPersistenceShutdownOutcome::CompletedWithoutWorker {
                save_block: self.view.save_block,
            };
        };

        let worker_outcome = worker.shutdown(newest_committed, deadline.remaining());
        let outcome = match worker_outcome {
            SaveWorkerShutdownOutcome::Complete(completion) => {
                self.apply_shutdown_completion(completion);
                self.shutdown_complete = true;
                match capture_failure {
                    Some(revision) => PlaylistPersistenceShutdownOutcome::SnapshotCaptureFailed {
                        revision,
                        completion,
                    },
                    None => PlaylistPersistenceShutdownOutcome::Completed(completion),
                }
            }
            SaveWorkerShutdownOutcome::TimedOut { phase, completion } => {
                if let Some(completion) = completion {
                    self.apply_shutdown_completion(completion);
                }
                PlaylistPersistenceShutdownOutcome::TimedOut { phase, completion }
            }
            SaveWorkerShutdownOutcome::ThreadPanicked(completion) => {
                self.apply_shutdown_completion(completion);
                self.shutdown_complete = true;
                PlaylistPersistenceShutdownOutcome::ThreadPanicked(completion)
            }
        };
        if matches!(outcome, PlaylistPersistenceShutdownOutcome::TimedOut { .. }) {
            // `SaveWorker::shutdown(self, ..)` не возвращает JoinHandle при timeout.
            // Повторный UI lifecycle не может восстановить ownership: остаётся только
            // terminal process exit до освобождения app-instance lease.
            self.exit_required_outcome = Some(outcome);
        }
        outcome
    }

    fn apply_shutdown_completion(&mut self, completion: ShutdownCompletion) {
        match completion.persistence {
            ShutdownPersistenceOutcome::NoCommittedSnapshot => {}
            ShutdownPersistenceOutcome::AlreadyDurable { revision } => {
                self.view.durability = PlaylistSaveDurability::Durable { revision };
                self.view.warning = None;
            }
            ShutdownPersistenceOutcome::Attempted(report) => {
                self.apply_worker_event(SaveWorkerEvent::AttemptCompleted(report));
                let failure = match report.outcome {
                    SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::NotReplaced(cause)) => {
                        Some(SaveWarningFailure::NotReplaced(cause))
                    }
                    SaveAttemptOutcome::FullWrite(
                        AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(cause),
                    )
                    | SaveAttemptOutcome::DirectoryDurabilityRetry(
                        DurabilityRetryOutcome::StillUnconfirmed(cause),
                    ) => Some(SaveWarningFailure::DurabilityUnconfirmed(cause)),
                    SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::Durable)
                    | SaveAttemptOutcome::DirectoryDurabilityRetry(
                        DurabilityRetryOutcome::Durable,
                    ) => None,
                };
                match failure {
                    Some(failure) => self.record_shutdown_warning(report.revision, failure),
                    None => self.view.warning = None,
                }
            }
        }
    }

    fn record_shutdown_warning(&mut self, revision: SaveRevision, failure: SaveWarningFailure) {
        let occurrence_count = self
            .view
            .warning
            .filter(|warning| warning.revision == revision && warning.failure == failure)
            .map_or(1, |warning| warning.occurrence_count.saturating_add(1));
        self.view.warning = Some(SaveWarning {
            revision,
            failure,
            occurrence_count,
        });
    }
}

fn save_control_error_message(error: SaveControlError) -> String {
    match error {
        SaveControlError::Backpressure => "playlist save worker control queue is busy".to_owned(),
        SaveControlError::Disconnected => "playlist save worker is disconnected".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind,
    };
    use playlist_state::{
        DurabilityUnconfirmedCause, InspectionOutcome, NotReplacedCause, NotReplacedFailure,
        NotReplacedStage, SaveAttemptReport, SaveWorkerShutdownOutcome,
    };

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort, OwnerMailboxReceiver, owner_mailbox};
    use crate::playlist_runtime::{
        PlaylistOwnerCompletion, PlaylistOwnerProgress, PlaylistQueueGeneration,
    };

    struct FixedQuarantineNamePolicy {
        timestamp: SystemTime,
    }

    impl QuarantineNamePolicy for FixedQuarantineNamePolicy {
        fn next_name(&mut self) -> QuarantineFileName {
            QuarantineFileName::from_timestamp(self.timestamp)
        }
    }

    fn draft(label: &str) -> PlaylistItemDraft {
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(label)),
            None,
            CachedPlaylistMetadata::new(label.to_owned(), PlaylistMediaKind::Video),
        )
    }

    fn owner_ports() -> (
        PlaylistOwnerPorts,
        OwnerMailboxReceiver<PlaylistOwnerProgress, PlaylistOwnerCompletion>,
    ) {
        let wake_port = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
        let (publisher, receiver) = owner_mailbox(wake_port);
        (
            PlaylistOwnerPorts {
                publisher,
                admission_open: Arc::new(AtomicBool::new(true)),
            },
            receiver,
        )
    }

    fn wait_for_terminal_durability(owner: &mut PlaylistPersistenceOwner) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            owner.drain_worker_events();
            if matches!(
                owner.view.durability,
                PlaylistSaveDurability::Durable { .. }
                    | PlaylistSaveDurability::ReplacedDurabilityUnconfirmed { .. }
                    | PlaylistSaveDurability::NotReplaced { .. }
            ) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("playlist save worker did not produce a terminal attempt");
    }

    fn shutdown_worker(owner: &mut PlaylistPersistenceOwner) {
        let worker = owner
            .save_runtime
            .borrow_mut()
            .worker
            .take()
            .expect("started worker");
        assert!(matches!(
            worker.shutdown(None, Duration::from_secs(1)),
            SaveWorkerShutdownOutcome::Complete(_)
        ));
    }

    #[test]
    fn persistent_snapshot_roundtrips_current_none_and_allocator_watermark() {
        let directory = tempfile::tempdir().expect("state temp directory");
        let state_path = directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME);
        let store = Arc::new(PlaylistStateStore::new(&state_path));
        let mut controller = PlaylistController::new();
        let added = controller
            .append(vec![draft("first.mkv"), draft("second.mkv")])
            .expect("append");
        assert!(matches!(
            added,
            crate::playlist_runtime::controller::ControllerAppendOutcome::Added { .. }
        ));
        let watermark = controller.queue().next_item_id_snapshot();
        let first_item = controller.queue().items()[0].item_id();
        assert!(controller.select_row(Some(first_item)));

        let (ports, _receiver) = owner_ports();
        let mut owner = PlaylistPersistenceOwner::new(250);
        owner.install_store(store.clone());
        owner
            .start_for_lineage(PlaylistLineagePersistence::Persistent, ports)
            .expect("start writer");
        owner.publish_committed_controller(&controller);
        wait_for_terminal_durability(&mut owner);

        let InspectionOutcome::Loaded(loaded) = store.inspect_state() else {
            panic!("saved state must load");
        };
        let (loaded_queue, _) = loaded.into_parts();
        assert_eq!(loaded_queue.traversal_current(), None);
        assert_eq!(loaded_queue.next_item_id_snapshot(), watermark);
        assert_eq!(loaded_queue.len(), 2);
        shutdown_worker(&mut owner);
    }

    #[test]
    fn protected_lineage_starts_no_worker_and_never_creates_target() {
        let directory = tempfile::tempdir().expect("state temp directory");
        let state_path = directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME);
        let mut owner = PlaylistPersistenceOwner::new(250);
        owner.install_store(Arc::new(PlaylistStateStore::new(&state_path)));
        let (ports, _receiver) = owner_ports();
        owner
            .start_for_lineage(
                PlaylistLineagePersistence::NonPersistent {
                    queue_generation: PlaylistQueueGeneration::INITIAL,
                    save_block: SaveBlockReason::NewerSchema,
                },
                ports,
            )
            .expect("blocked start is typed success");

        let mut controller = PlaylistController::new();
        controller
            .append(vec![draft("protected.mkv")])
            .expect("append");
        owner.publish_committed_controller(&controller);

        assert!(owner.save_runtime.borrow().worker.is_none());
        assert_eq!(owner.view.save_block, Some(SaveBlockReason::NewerSchema));
        assert!(!state_path.exists());
    }

    #[test]
    fn detached_then_live_debounce_adapter_preserves_pending_snapshot_policy() {
        let owner = PlaylistPersistenceOwner::new(2_000);
        let mut port = owner.debounce_port();

        port.reschedule_debounce(3_000)
            .expect("apply staged debounce before worker start");
        assert_eq!(
            owner.save_runtime.borrow().configured.duration(),
            Duration::from_millis(3_000)
        );

        port.reschedule_debounce(2_000)
            .expect("rollback restores committed debounce");
        assert_eq!(
            owner.save_runtime.borrow().configured.duration(),
            Duration::from_millis(2_000)
        );
    }

    #[test]
    fn quarantine_name_policy_is_injectable_without_filesystem_access() {
        let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(17);
        let mut owner = PlaylistPersistenceOwner::new(2_000);
        owner.replace_quarantine_name_policy(Box::new(FixedQuarantineNamePolicy { timestamp }));

        assert_eq!(
            owner.next_quarantine_file_name(),
            QuarantineFileName::from_timestamp(timestamp)
        );
    }

    #[test]
    fn read_model_never_collapses_replace_and_directory_durability_outcomes() {
        let mut owner = PlaylistPersistenceOwner::new(2_000);
        let revision = SaveRevision::FIRST;
        owner.apply_worker_event(SaveWorkerEvent::AttemptCompleted(SaveAttemptReport {
            revision,
            outcome: SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::NotReplaced(
                NotReplacedFailure {
                    stage: NotReplacedStage::RenameTempFile,
                    cause: NotReplacedCause::Io(std::io::ErrorKind::PermissionDenied),
                },
            )),
        }));
        assert_eq!(
            owner.view.durability,
            PlaylistSaveDurability::NotReplaced { revision }
        );

        owner.apply_worker_event(SaveWorkerEvent::AttemptCompleted(SaveAttemptReport {
            revision,
            outcome: SaveAttemptOutcome::FullWrite(
                AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(
                    DurabilityUnconfirmedCause::SyncDirectory(std::io::ErrorKind::Unsupported),
                ),
            ),
        }));
        assert_eq!(
            owner.view.durability,
            PlaylistSaveDurability::ReplacedDurabilityUnconfirmed { revision }
        );

        owner.apply_worker_event(SaveWorkerEvent::AttemptCompleted(SaveAttemptReport {
            revision,
            outcome: SaveAttemptOutcome::DirectoryDurabilityRetry(DurabilityRetryOutcome::Durable),
        }));
        assert_eq!(
            owner.view.durability,
            PlaylistSaveDurability::Durable { revision }
        );

        let newer_revision = revision.checked_next().expect("second save revision");
        owner.view.latest_committed_revision = Some(newer_revision);
        owner.view.durability = PlaylistSaveDurability::Pending {
            revision: newer_revision,
        };
        owner.apply_worker_event(SaveWorkerEvent::AttemptCompleted(SaveAttemptReport {
            revision,
            outcome: SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::NotReplaced(
                NotReplacedFailure {
                    stage: NotReplacedStage::RenameTempFile,
                    cause: NotReplacedCause::Io(std::io::ErrorKind::PermissionDenied),
                },
            )),
        }));
        assert_eq!(
            owner.view.durability,
            PlaylistSaveDurability::Pending {
                revision: newer_revision
            }
        );
    }

    #[test]
    fn shutdown_flushes_newest_committed_snapshot_without_incrementing_revision() {
        let directory = tempfile::tempdir().expect("state temp directory");
        let state_path = directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME);
        let store = Arc::new(PlaylistStateStore::new(&state_path));
        let mut controller = PlaylistController::new();
        controller
            .append(vec![draft("shutdown-latest.mkv")])
            .expect("append committed item");

        let (ports, _receiver) = owner_ports();
        let mut owner = PlaylistPersistenceOwner::new(30_000);
        owner.install_store(store.clone());
        owner
            .start_for_lineage(PlaylistLineagePersistence::Persistent, ports)
            .expect("start writer");
        owner.publish_committed_controller(&controller);
        let committed_revision = owner.latest_revision.expect("committed revision");

        let outcome = owner.shutdown_until(
            Some(&controller),
            ShutdownDeadline::after(Duration::from_secs(2)),
        );
        let PlaylistPersistenceShutdownOutcome::Completed(completion) = outcome else {
            panic!("unexpected persistence shutdown outcome: {outcome:?}");
        };
        assert_eq!(owner.latest_revision, Some(committed_revision));
        assert_eq!(
            completion.persistence,
            ShutdownPersistenceOutcome::Attempted(SaveAttemptReport {
                revision: committed_revision,
                outcome: SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::Durable),
            })
        );
        let InspectionOutcome::Loaded(loaded) = store.inspect_state() else {
            panic!("shutdown snapshot must be readable");
        };
        assert_eq!(loaded.queue().len(), 1);
    }

    #[test]
    fn clean_shutdown_does_not_create_synthetic_snapshot_or_target() {
        let directory = tempfile::tempdir().expect("state temp directory");
        let state_path = directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME);
        let mut owner = PlaylistPersistenceOwner::new(30_000);
        owner.install_store(Arc::new(PlaylistStateStore::new(&state_path)));
        let (ports, _receiver) = owner_ports();
        owner
            .start_for_lineage(PlaylistLineagePersistence::Persistent, ports)
            .expect("start writer");
        let controller = PlaylistController::new();

        assert_eq!(
            owner.shutdown_until(
                Some(&controller),
                ShutdownDeadline::after(Duration::from_secs(2)),
            ),
            PlaylistPersistenceShutdownOutcome::Completed(ShutdownCompletion {
                persistence: ShutdownPersistenceOutcome::NoCommittedSnapshot,
            })
        );
        assert!(!state_path.exists());
        assert_eq!(
            owner.shutdown_until(
                Some(&controller),
                ShutdownDeadline::after(Duration::from_secs(2)),
            ),
            PlaylistPersistenceShutdownOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn protected_lineage_shutdown_keeps_writer_absent() {
        let mut owner = PlaylistPersistenceOwner::new(30_000);
        let (ports, _receiver) = owner_ports();
        owner
            .start_for_lineage(
                PlaylistLineagePersistence::NonPersistent {
                    queue_generation: PlaylistQueueGeneration::INITIAL,
                    save_block: SaveBlockReason::NewerSchema,
                },
                ports,
            )
            .expect("protected lineage is a typed non-writer");

        assert_eq!(
            owner.shutdown_until(None, ShutdownDeadline::after(Duration::from_secs(1))),
            PlaylistPersistenceShutdownOutcome::CompletedWithoutWorker {
                save_block: Some(SaveBlockReason::NewerSchema),
            }
        );
    }
}
