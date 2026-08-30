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

#[test]
fn public_shutdown_reports_command_admission_timeout_while_queue_is_full() {
    const COMMAND_ADMISSION_TIMEOUT: Duration = Duration::from_secs(1);

    let writer = Arc::new(BlockingWriter::new());
    let wake_port = Arc::new(CountingWakePort::default());
    let worker = started_test_worker(writer.clone(), wake_port);
    worker
        .submit_snapshot(captured_snapshot(revision(1), 1))
        .expect("revision 1 принимается");
    worker.retry_now().expect("write запускается немедленно");
    writer.wait_until_entered(1);

    // Заблокированный writer не позволяет receiver-у освободить ни один command slot.
    let capacity_admissions = (2..=9)
        .map(|value| worker.submit_snapshot(captured_snapshot(revision(value), value as usize)))
        .collect::<Vec<_>>();
    let backpressured_admission = worker.submit_snapshot(captured_snapshot(revision(10), 10));

    let shutdown = worker.shutdown(None, COMMAND_ADMISSION_TIMEOUT);
    let revisions_before_release = writer.revisions();
    let maximum_active_writes = writer.maximum_active_writes();
    // Fake writer остаётся test-owned даже после consuming shutdown и освобождается до assert.
    writer.release();

    assert!(
        capacity_admissions
            .iter()
            .all(|outcome| matches!(outcome, Ok(SubmitSnapshotOutcome::Accepted)))
    );
    let backpressured_snapshot = match backpressured_admission {
        Err(SubmitSnapshotError::Backpressure(snapshot)) => snapshot,
        other => panic!("ожидался typed backpressure, получено {other:?}"),
    };
    assert_eq!(backpressured_snapshot.revision(), revision(10));
    assert_eq!(
        shutdown,
        SaveWorkerShutdownOutcome::TimedOut {
            phase: ShutdownTimeoutPhase::CommandAdmission,
            completion: None,
        }
    );
    assert_eq!(revisions_before_release, vec![revision(1)]);
    assert_eq!(maximum_active_writes, 1);
}
