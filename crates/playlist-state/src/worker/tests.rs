use std::collections::VecDeque;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use playlist_core::{
    CachedPlaylistMetadata, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue, RepeatMode,
    SecretUrlLocator,
};

use super::*;
use crate::atomic_write::{
    DurabilityUnconfirmedCause, NotReplacedCause, NotReplacedFailure, NotReplacedStage,
};
use crate::{PlaylistStateSnapshot, SaveRevision};

fn revision(value: u64) -> SaveRevision {
    let mut revision = SaveRevision::FIRST;
    for _ in 1..value {
        revision = revision.checked_next().expect("малый test revision");
    }
    revision
}

fn captured_snapshot(revision: SaveRevision, item_count: usize) -> ImmutableSaveSnapshot {
    let mut queue = PlaylistQueue::new();
    for item_index in 0..item_count {
        let locator = SecretUrlLocator::from_reopenable_url(format!(
            "https://media.invalid/{item_index}.mp4?private=value"
        ))
        .expect("test URL непустой");
        let metadata =
            CachedPlaylistMetadata::new(format!("item-{item_index}"), PlaylistMediaKind::Video);
        queue
            .append_one(PlaylistItemDraft::url(locator, metadata))
            .expect("test queue остаётся bounded");
    }
    ImmutableSaveSnapshot::capture(
        revision,
        PlaylistStateSnapshot::new(&queue, RepeatMode::StopAtEnd),
    )
    .expect("малый snapshot валиден")
}

#[derive(Default)]
struct CountingWakePort {
    wake_count: AtomicUsize,
    fail: AtomicBool,
    wait_lock: Mutex<()>,
    changed: Condvar,
}

impl CountingWakePort {
    fn count(&self) -> usize {
        self.wake_count.load(Ordering::Acquire)
    }

    fn wait_for_count(&self, expected_count: usize) {
        let wait_guard = self.wait_lock.lock().expect("test wake lock");
        let (_wait_guard, timeout) = self
            .changed
            .wait_timeout_while(wait_guard, Duration::from_secs(2), |_| {
                self.count() < expected_count
            })
            .expect("test wake condvar");
        assert!(!timeout.timed_out(), "wake signal не опубликован");
    }
}

impl SaveWakePort for CountingWakePort {
    fn wake_save_worker(&self) -> Result<(), WakePortDisconnected> {
        self.wake_count.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_all();
        if self.fail.load(Ordering::Acquire) {
            Err(WakePortDisconnected)
        } else {
            Ok(())
        }
    }
}

struct ScriptedWriter {
    full_write_outcomes: Mutex<VecDeque<AtomicWriteOutcome>>,
    durability_outcomes: Mutex<VecDeque<DurabilityRetryOutcome>>,
    written_revisions: Mutex<Vec<SaveRevision>>,
    written_json: Mutex<Vec<serde_json::Value>>,
    durability_retry_count: AtomicUsize,
}

struct FailingThreadSpawner;

impl WorkerThreadSpawner for FailingThreadSpawner {
    fn spawn(
        &self,
        _thread_name: &str,
        _worker_job: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<std::thread::JoinHandle<()>> {
        Err(io::Error::other("injected thread spawn failure"))
    }
}

impl ScriptedWriter {
    fn new(
        full_write_outcomes: impl IntoIterator<Item = AtomicWriteOutcome>,
        durability_outcomes: impl IntoIterator<Item = DurabilityRetryOutcome>,
    ) -> Self {
        Self {
            full_write_outcomes: Mutex::new(full_write_outcomes.into_iter().collect()),
            durability_outcomes: Mutex::new(durability_outcomes.into_iter().collect()),
            written_revisions: Mutex::new(Vec::new()),
            written_json: Mutex::new(Vec::new()),
            durability_retry_count: AtomicUsize::new(0),
        }
    }

    fn revisions(&self) -> Vec<SaveRevision> {
        self.written_revisions
            .lock()
            .expect("test writer lock")
            .clone()
    }

    fn json_snapshots(&self) -> Vec<serde_json::Value> {
        self.written_json.lock().expect("test writer lock").clone()
    }
}

impl SnapshotWriter for ScriptedWriter {
    fn write_snapshot(&self, snapshot: &ImmutableSaveSnapshot) -> AtomicWriteOutcome {
        self.written_revisions
            .lock()
            .expect("test writer lock")
            .push(snapshot.revision());
        let json = serde_json::from_slice(
            &snapshot
                .serialize_json()
                .expect("test snapshot сериализуется"),
        )
        .expect("serialized snapshot является JSON");
        self.written_json
            .lock()
            .expect("test writer lock")
            .push(json);
        self.full_write_outcomes
            .lock()
            .expect("test outcome lock")
            .pop_front()
            .unwrap_or(AtomicWriteOutcome::Durable)
    }

    fn retry_directory_durability(&self) -> DurabilityRetryOutcome {
        self.durability_retry_count.fetch_add(1, Ordering::AcqRel);
        self.durability_outcomes
            .lock()
            .expect("test outcome lock")
            .pop_front()
            .unwrap_or(DurabilityRetryOutcome::Durable)
    }
}

struct BlockingWriter {
    state: Mutex<BlockingWriterState>,
    changed: Condvar,
}

struct BlockingWriterState {
    release: bool,
    entered_writes: usize,
    active_writes: usize,
    maximum_active_writes: usize,
    revisions: Vec<SaveRevision>,
}

impl BlockingWriter {
    fn new() -> Self {
        Self {
            state: Mutex::new(BlockingWriterState {
                release: false,
                entered_writes: 0,
                active_writes: 0,
                maximum_active_writes: 0,
                revisions: Vec::new(),
            }),
            changed: Condvar::new(),
        }
    }

    fn wait_until_entered(&self, expected_writes: usize) {
        let state = self.state.lock().expect("test blocking lock");
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.entered_writes < expected_writes
            })
            .expect("test condvar wait");
        assert!(!timeout.timed_out(), "writer не вошёл в ожидаемую write");
        assert!(state.entered_writes >= expected_writes);
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("test blocking lock");
        state.release = true;
        self.changed.notify_all();
    }

    fn revisions(&self) -> Vec<SaveRevision> {
        self.state
            .lock()
            .expect("test blocking lock")
            .revisions
            .clone()
    }

    fn maximum_active_writes(&self) -> usize {
        self.state
            .lock()
            .expect("test blocking lock")
            .maximum_active_writes
    }
}

impl SnapshotWriter for BlockingWriter {
    fn write_snapshot(&self, snapshot: &ImmutableSaveSnapshot) -> AtomicWriteOutcome {
        let mut state = self.state.lock().expect("test blocking lock");
        state.entered_writes += 1;
        state.active_writes += 1;
        state.maximum_active_writes = state.maximum_active_writes.max(state.active_writes);
        state.revisions.push(snapshot.revision());
        self.changed.notify_all();
        while !state.release {
            state = self.changed.wait(state).expect("test condvar wait");
        }
        state.active_writes -= 1;
        AtomicWriteOutcome::Durable
    }

    fn retry_directory_durability(&self) -> DurabilityRetryOutcome {
        DurabilityRetryOutcome::Durable
    }
}

fn started_test_worker(
    writer: Arc<dyn SnapshotWriter>,
    wake_port: Arc<CountingWakePort>,
) -> SaveWorker {
    SaveWorker::start_with_dependencies(
        SaveDebounce::new(Duration::from_secs(30)).expect("test debounce валиден"),
        wake_port,
        writer,
    )
    .expect("test thread запускается")
}

#[test]
fn debounce_bounds_are_inclusive_and_zero_is_rejected() {
    assert!(SaveDebounce::new(SaveDebounce::MINIMUM).is_ok());
    assert!(SaveDebounce::new(SaveDebounce::MAXIMUM).is_ok());
    assert!(SaveDebounce::new(Duration::ZERO).is_err());
    assert!(SaveDebounce::new(Duration::from_millis(249)).is_err());
    assert!(SaveDebounce::new(Duration::from_millis(30_001)).is_err());
}

#[test]
fn thread_spawn_failure_is_typed_and_does_not_panic() {
    let writer = Arc::new(ScriptedWriter::new([], []));
    let wake_port = Arc::new(CountingWakePort::default());
    let outcome = SaveWorker::start_with_spawner(
        SaveDebounce::new(Duration::from_secs(2)).expect("debounce валиден"),
        wake_port,
        writer,
        &FailingThreadSpawner,
    );

    assert!(matches!(
        outcome,
        Err(SaveWorkerStartError::ThreadSpawn(io::ErrorKind::Other))
    ));
}

#[test]
fn latest_only_shutdown_coalesces_snapshot_and_matching_allocator_watermark() {
    let writer = Arc::new(ScriptedWriter::new([], []));
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer.clone(), wake_port);
    assert_eq!(
        worker
            .submit_snapshot(captured_snapshot(revision(1), 1))
            .expect("revision 1 принимается"),
        SubmitSnapshotOutcome::Accepted
    );
    assert_eq!(
        worker
            .submit_snapshot(captured_snapshot(revision(2), 2))
            .expect("revision 2 принимается"),
        SubmitSnapshotOutcome::Accepted
    );

    let shutdown = worker.shutdown(None, Duration::from_secs(2));

    assert!(matches!(shutdown, SaveWorkerShutdownOutcome::Complete(_)));
    assert_eq!(writer.revisions(), vec![revision(2)]);
    let json = writer.json_snapshots();
    assert_eq!(json[0]["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(json[0]["next_item_id"].as_u64(), Some(3));
}

#[test]
fn same_revision_is_a_no_op_before_queue_admission() {
    let writer = Arc::new(ScriptedWriter::new([], []));
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer, wake_port);
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("first snapshot принимается");

    assert_eq!(
        worker
            .submit_snapshot(captured_snapshot(revision(1), 2))
            .expect("same revision является typed no-op"),
        SubmitSnapshotOutcome::NoOpSameOrOlderRevision
    );
    assert!(matches!(
        worker.shutdown(None, Duration::from_secs(2)),
        SaveWorkerShutdownOutcome::Complete(_)
    ));
}

#[test]
fn mutation_during_write_runs_next_revision_without_parallel_write() {
    let writer = Arc::new(BlockingWriter::new());
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer.clone(), wake_port);
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("revision 1 принимается");
    worker.retry_now().expect("manual retry принимается");
    writer.wait_until_entered(1);
    worker
        .submit_snapshot(captured_snapshot(revision(2), 2))
        .expect("mutation во время write принимается");

    writer.release();
    let shutdown = worker.shutdown(None, Duration::from_secs(2));

    assert!(matches!(shutdown, SaveWorkerShutdownOutcome::Complete(_)));
    assert_eq!(writer.revisions(), vec![revision(1), revision(2)]);
    assert_eq!(writer.maximum_active_writes(), 1);
}

#[test]
fn full_command_queue_returns_latest_snapshot_to_caller() {
    let writer = Arc::new(BlockingWriter::new());
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer.clone(), wake_port);
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("revision 1 принимается");
    worker.retry_now().expect("write запускается немедленно");
    writer.wait_until_entered(1);

    for value in 2..=9 {
        worker
            .submit_snapshot(captured_snapshot(revision(value), value as usize))
            .expect("ровно capacity commands принимаются");
    }
    let newest_committed = match worker.submit_snapshot(captured_snapshot(revision(10), 10)) {
        Err(SubmitSnapshotError::Backpressure(snapshot)) => *snapshot,
        other => panic!("ожидался typed backpressure, получено {other:?}"),
    };

    writer.release();
    let shutdown = worker.shutdown(Some(newest_committed), Duration::from_secs(2));
    assert!(matches!(shutdown, SaveWorkerShutdownOutcome::Complete(_)));
    assert_eq!(writer.maximum_active_writes(), 1);
    assert_eq!(writer.revisions().last().copied(), Some(revision(10)));
}

#[test]
fn wake_is_coalesced_until_drain_and_terminal_report_is_exactly_once() {
    let failure = NotReplacedFailure {
        stage: NotReplacedStage::WriteTempFile,
        cause: NotReplacedCause::Io(io::ErrorKind::WriteZero),
    };
    let writer = Arc::new(ScriptedWriter::new(
        [AtomicWriteOutcome::NotReplaced(failure)],
        [],
    ));
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer, wake_port.clone());
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("snapshot принимается");
    worker.retry_now().expect("retry принимается");

    wake_port.wait_for_count(1);
    assert_eq!(wake_port.count(), 1);
    let first_drain = worker.drain_events();
    assert_eq!(
        first_drain
            .iter()
            .filter(|event| matches!(event, SaveWorkerEvent::AttemptCompleted(_)))
            .count(),
        1
    );
    assert!(matches!(
        first_drain.as_slice(),
        [
            SaveWorkerEvent::AttemptCompleted(_),
            SaveWorkerEvent::WarningChanged(Some(_))
        ]
    ));
    assert!(worker.drain_events().is_empty());

    assert!(matches!(
        worker.shutdown(None, Duration::from_secs(2)),
        SaveWorkerShutdownOutcome::Complete(_)
    ));
}

#[test]
fn publish_vs_clear_race_never_loses_disconnect_payload() {
    let wake_port = Arc::new(CountingWakePort::default());
    let mailbox = Arc::new(WorkerMailbox::new(wake_port));
    for _ in 0..100 {
        let producer_mailbox = mailbox.clone();
        let producer = std::thread::spawn(move || {
            producer_mailbox.publish_disconnect(WorkerDisconnectReason::CommandChannelClosed);
        });
        let mut events = mailbox.drain();
        producer.join().expect("test producer завершается");
        events.extend(mailbox.drain());
        assert!(events.iter().any(|event| {
            matches!(
                event,
                SaveWorkerEvent::WorkerDisconnected(WorkerDisconnectReason::CommandChannelClosed)
            )
        }));
    }
}

#[test]
fn targeted_retry_never_performs_second_full_write() {
    let unconfirmed = DurabilityUnconfirmedCause::SyncDirectory(io::ErrorKind::Other);
    let writer = Arc::new(ScriptedWriter::new(
        [AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(
            unconfirmed,
        )],
        [DurabilityRetryOutcome::Durable],
    ));
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer.clone(), wake_port.clone());
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("snapshot принимается");
    worker.retry_now().expect("full write запускается");

    wake_port.wait_for_count(1);
    let _first_attempt_events = worker.drain_events();
    worker.retry_now().expect("targeted retry запускается");
    let shutdown = worker.shutdown(None, Duration::from_secs(2));

    assert!(matches!(shutdown, SaveWorkerShutdownOutcome::Complete(_)));
    assert_eq!(writer.revisions(), vec![revision(1)]);
    assert_eq!(writer.durability_retry_count.load(Ordering::Acquire), 1);
}

#[test]
fn retry_backoff_caps_resets_and_manual_retry_is_immediate_with_fake_time() {
    let debounce = SaveDebounce::new(Duration::from_secs(2)).expect("debounce валиден");
    let mut state = WorkerState::new(debounce);
    let failure = NotReplacedFailure {
        stage: NotReplacedStage::SyncTempFile,
        cause: NotReplacedCause::Io(io::ErrorKind::Other),
    };
    let mut now = Duration::ZERO;
    state.accept_snapshot(captured_snapshot(revision(1), 1), now);
    state.retry_now(now);

    for _ in 0..10 {
        state.apply_full_write_outcome(revision(1), AtomicWriteOutcome::NotReplaced(failure), now);
        let scheduled = state.pending_attempt.expect("retry запланирован");
        assert!(scheduled.deadline.saturating_sub(now) <= MAXIMUM_RETRY_DELAY);
        now = scheduled.deadline;
    }
    assert_eq!(state.next_retry_delay, MAXIMUM_RETRY_DELAY);
    assert_eq!(
        state.warning.map(|warning| warning.occurrence_count),
        Some(10)
    );

    state.retry_now(now);
    assert_eq!(state.due_attempt(now), Some(PendingAttemptKind::FullWrite));
    state.accept_snapshot(captured_snapshot(revision(2), 2), now);
    assert_eq!(state.next_retry_delay, INITIAL_RETRY_DELAY);
    assert_eq!(
        state.pending_attempt.map(|attempt| attempt.deadline),
        Some(now + debounce.duration())
    );
    state.apply_full_write_outcome(revision(2), AtomicWriteOutcome::Durable, now);
    assert!(state.warning.is_none());
}

#[test]
fn live_debounce_reschedule_preserves_dirty_revision() {
    let mut state =
        WorkerState::new(SaveDebounce::new(Duration::from_secs(2)).expect("debounce валиден"));
    state.accept_snapshot(captured_snapshot(revision(3), 3), Duration::from_secs(5));

    state.reschedule_debounce(
        SaveDebounce::new(Duration::from_secs(10)).expect("debounce валиден"),
        Duration::from_secs(6),
    );

    assert_eq!(state.current_revision(), Some(revision(3)));
    assert_eq!(
        state.pending_attempt.map(|attempt| attempt.deadline),
        Some(Duration::from_secs(16))
    );
}

#[test]
fn shutdown_flushes_only_newest_committed_revision_before_debounce() {
    let writer = Arc::new(ScriptedWriter::new([], []));
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer.clone(), wake_port);
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("older committed принимается");

    let shutdown = worker.shutdown(
        Some(captured_snapshot(revision(2), 2)),
        Duration::from_secs(2),
    );

    assert!(matches!(
        shutdown,
        SaveWorkerShutdownOutcome::Complete(ShutdownCompletion {
            persistence: ShutdownPersistenceOutcome::Attempted(SaveAttemptReport {
                revision: saved_revision,
                ..
            })
        }) if saved_revision == revision(2)
    ));
    assert_eq!(writer.revisions(), vec![revision(2)]);
}

#[test]
fn shutdown_timeout_does_not_claim_join_or_durability_success() {
    let writer = Arc::new(BlockingWriter::new());
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer.clone(), wake_port);
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("snapshot принимается");
    worker.retry_now().expect("write запускается");
    writer.wait_until_entered(1);

    let shutdown = worker.shutdown(None, Duration::from_millis(10));

    assert!(matches!(
        shutdown,
        SaveWorkerShutdownOutcome::TimedOut {
            phase: ShutdownTimeoutPhase::CompletionAcknowledgement,
            completion: None
        }
    ));
    writer.release();
}

#[test]
fn every_save_block_skips_thread_and_target_access() {
    let reasons = [
        SaveBlockReason::NewerSchema,
        SaveBlockReason::UnrecognizedVersion,
        SaveBlockReason::DuplicateVersion,
        SaveBlockReason::QuarantineFailed,
        SaveBlockReason::QuarantineSourceChanged,
    ];
    for reason in reasons {
        let directory = tempfile::tempdir().expect("tempdir доступен");
        let target_path = directory.path().join("playlist-state.json");
        fs::write(&target_path, b"protected").expect("protected target создаётся");
        let wake_port = Arc::new(CountingWakePort::default());
        let outcome = SaveWorker::start(
            SaveWorkerAccess::SaveBlocked(reason),
            Arc::new(PlaylistStateStore::new(&target_path)),
            SaveDebounce::new(Duration::from_secs(2)).expect("debounce валиден"),
            wake_port,
        )
        .expect("blocked start не вызывает OS spawn");

        assert!(matches!(
            outcome,
            SaveWorkerStartOutcome::SaveBlocked(returned_reason) if returned_reason == reason
        ));
        assert_eq!(
            fs::read(&target_path).expect("target остаётся"),
            b"protected"
        );
    }
}

#[test]
fn failed_wake_is_reported_once_without_retry_spin() {
    let wake_port = Arc::new(CountingWakePort::default());
    wake_port.fail.store(true, Ordering::Release);
    let mailbox = WorkerMailbox::new(wake_port.clone());

    mailbox.publish_disconnect(WorkerDisconnectReason::CommandChannelClosed);
    mailbox.publish_warning(None);
    mailbox.publish_disconnect(WorkerDisconnectReason::UnexpectedThreadExit);

    assert_eq!(wake_port.count(), 1);
    let events = mailbox.drain();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SaveWorkerEvent::WakePortDisconnected))
            .count(),
        1
    );
}
