//! Фоновый writer «только последнее»: порядок ревизий и сброс при остановке.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use super::super::*;
use super::{capacity, hint, key_of};
use crate::AtomicWriteOutcome;

fn revision(value: u64) -> TrackMemorySaveRevision {
    TrackMemorySaveRevision::new(NonZeroU64::new(value).expect("non-zero"))
}

fn memory_with_offset(millis: i64) -> TrackMemory {
    let mut memory = TrackMemory::new(capacity(10));
    memory.remember_audio(key_of("video"), hint("eng", "A_AC3", 0));
    memory.remember_sync_offset(
        key_of("video"),
        SubtitleSyncOffset::from_millis(millis).expect("offset"),
    );
    memory
}

fn loaded_offset(store: &TrackMemoryStore) -> i64 {
    match store.inspect(capacity(10)) {
        TrackMemoryInspection::Loaded(memory) => memory
            .entry_for(&key_of("video"))
            .expect("entry")
            .subtitle_sync_offset()
            .millis(),
        other => panic!("loaded expected, got {other:?}"),
    }
}

#[test]
fn shutdown_flushes_the_newest_snapshot_and_reports_a_durable_write() {
    let directory = tempfile::tempdir().expect("dir");
    let store = Arc::new(TrackMemoryStore::new(
        directory.path().join(TRACK_MEMORY_FILENAME),
    ));
    let mut writer = TrackMemoryWriter::start(store.clone()).expect("start");
    for step in 1..=32_u64 {
        let snapshot =
            TrackMemoryWriteSnapshot::new(revision(step), memory_with_offset(step as i64));
        assert_eq!(writer.submit(snapshot), TrackMemorySubmitOutcome::Accepted);
    }

    let outcome = writer.shutdown(
        Some(TrackMemoryWriteSnapshot::new(
            revision(33),
            memory_with_offset(33),
        )),
        Duration::from_secs(2),
    );

    assert!(matches!(
        outcome,
        TrackMemoryWriterShutdownOutcome::Completed {
            final_report: Some(TrackMemoryWriteReport {
                outcome: AtomicWriteOutcome::Durable,
                ..
            })
        }
    ));
    assert_eq!(loaded_offset(&store), 33);
}

#[test]
fn same_or_older_revision_is_rejected_and_never_overwrites_newer_data() {
    let directory = tempfile::tempdir().expect("dir");
    let store = Arc::new(TrackMemoryStore::new(
        directory.path().join(TRACK_MEMORY_FILENAME),
    ));
    let mut writer = TrackMemoryWriter::start(store.clone()).expect("start");
    assert_eq!(
        writer.submit(TrackMemoryWriteSnapshot::new(
            revision(5),
            memory_with_offset(5)
        )),
        TrackMemorySubmitOutcome::Accepted
    );

    assert_eq!(
        writer.submit(TrackMemoryWriteSnapshot::new(
            revision(5),
            memory_with_offset(99)
        )),
        TrackMemorySubmitOutcome::SameOrOlderRevision
    );
    assert_eq!(
        writer.submit(TrackMemoryWriteSnapshot::new(
            revision(4),
            memory_with_offset(98)
        )),
        TrackMemorySubmitOutcome::SameOrOlderRevision
    );
    let outcome = writer.shutdown(None, Duration::from_secs(2));

    assert!(matches!(
        outcome,
        TrackMemoryWriterShutdownOutcome::Completed { .. }
    ));
    assert_eq!(loaded_offset(&store), 5);
}

#[test]
fn submit_after_shutdown_is_disconnected_and_second_shutdown_is_already_completed() {
    let directory = tempfile::tempdir().expect("dir");
    let store = Arc::new(TrackMemoryStore::new(
        directory.path().join(TRACK_MEMORY_FILENAME),
    ));
    let mut writer = TrackMemoryWriter::start(store).expect("start");

    let first = writer.shutdown(None, Duration::from_secs(2));

    assert!(matches!(
        first,
        TrackMemoryWriterShutdownOutcome::Completed { final_report: None }
    ));
    assert_eq!(
        writer.submit(TrackMemoryWriteSnapshot::new(
            revision(1),
            memory_with_offset(1)
        )),
        TrackMemorySubmitOutcome::Disconnected
    );
    assert_eq!(
        writer.shutdown(None, Duration::from_secs(1)),
        TrackMemoryWriterShutdownOutcome::AlreadyCompleted
    );
}

#[test]
fn write_failure_is_reported_as_not_replaced_and_keeps_the_writer_alive() {
    let directory = tempfile::tempdir().expect("dir");
    let missing_parent = directory
        .path()
        .join("no-such-dir")
        .join(TRACK_MEMORY_FILENAME);
    let store = Arc::new(TrackMemoryStore::new(missing_parent));
    let mut writer = TrackMemoryWriter::start(store).expect("start");
    writer.submit(TrackMemoryWriteSnapshot::new(
        revision(1),
        memory_with_offset(1),
    ));

    let outcome = writer.shutdown(None, Duration::from_secs(2));

    assert!(matches!(
        outcome,
        TrackMemoryWriterShutdownOutcome::Completed {
            final_report: Some(TrackMemoryWriteReport {
                outcome: AtomicWriteOutcome::NotReplaced(_),
                ..
            })
        }
    ));
}
