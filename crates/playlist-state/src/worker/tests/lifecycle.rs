//! Functional worker lifecycle proofs через public save-control boundary.

use super::*;

#[test]
fn public_debounce_reschedule_starts_exact_dirty_snapshot_before_old_deadline() {
    let writer = Arc::new(BlockingWriter::new());
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = SaveWorker::start_with_dependencies(
        SaveDebounce::new(SaveDebounce::MAXIMUM).expect("maximum debounce валиден"),
        wake_port.clone(),
        writer.clone(),
    )
    .expect("real save worker должен запуститься");
    let submitted_revision = revision(7);
    let submitted_snapshot = captured_snapshot(submitted_revision, 2);
    let expected_json: serde_json::Value = serde_json::from_slice(
        &submitted_snapshot
            .serialize_json()
            .expect("expected snapshot сериализуется"),
    )
    .expect("expected snapshot является JSON");

    assert_eq!(
        worker
            .submit_snapshot(submitted_snapshot)
            .expect("dirty snapshot принимается"),
        SubmitSnapshotOutcome::Accepted
    );
    worker
        .reschedule_debounce(
            SaveDebounce::new(SaveDebounce::MINIMUM).expect("minimum debounce валиден"),
        )
        .expect("public debounce reschedule принимается");

    // Condvar ограничивает ожидание двумя секундами: старый 30 s deadline сюда не подходит.
    writer.wait_until_entered(1);
    assert_eq!(writer.revisions(), vec![submitted_revision]);
    assert_eq!(writer.json_snapshots(), vec![expected_json]);

    writer.release();
    wake_port.wait_for_count(1);
    assert_eq!(
        worker.drain_events(),
        vec![SaveWorkerEvent::AttemptCompleted(SaveAttemptReport {
            revision: submitted_revision,
            outcome: SaveAttemptOutcome::FullWrite(AtomicWriteOutcome::Durable),
        })]
    );
    assert!(worker.drain_events().is_empty());
    assert_eq!(writer.revisions(), vec![submitted_revision]);
    assert_eq!(writer.maximum_active_writes(), 1);

    assert_eq!(
        worker.shutdown(None, Duration::from_secs(2)),
        SaveWorkerShutdownOutcome::Complete(ShutdownCompletion {
            persistence: ShutdownPersistenceOutcome::AlreadyDurable {
                revision: submitted_revision,
            },
        })
    );
}
