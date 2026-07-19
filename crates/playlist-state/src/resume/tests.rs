use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use playlist_core::{LocalLocator, PlaylistItemId, SecretUrlLocator};

use super::*;

fn item_id(value: u64) -> PlaylistItemId {
    PlaylistItemId::from_persistence_value(value).expect("non-zero item id")
}

fn local_locator(name: &str) -> PlaylistLocator {
    PlaylistLocator::Local(LocalLocator::Native(name.into()))
}

fn url_locator(url: &str) -> PlaylistLocator {
    PlaylistLocator::Url(
        SecretUrlLocator::from_reopenable_url(url.to_owned()).expect("valid absolute URL"),
    )
}

#[test]
fn strict_roundtrip_distinguishes_required_null_and_missing_field() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join(PLAYLIST_RESUME_FILENAME);
    let store = PlaylistResumeStore::new(&path);
    let checkpoint = ResumeCheckpoint::for_locator(
        item_id(7),
        &local_locator("episode.mkv"),
        Duration::new(91, 42),
    )
    .expect("fingerprint");
    std::fs::write(
        &path,
        serialize_resume(Some(&checkpoint)).expect("serialize resume"),
    )
    .expect("write fixture");

    assert!(matches!(
        store.inspect(),
        ResumeInspectionOutcome::Loaded(Some(loaded))
            if loaded == checkpoint
    ));

    std::fs::write(&path, br#"{"schema_version":1}"#).expect("write missing field");
    let missing_field_outcome = store.inspect();
    assert!(
        matches!(
            missing_field_outcome,
            ResumeInspectionOutcome::CorruptNeedsQuarantine {
                cause: ResumeCorruptCause::InvalidV1Payload,
                ..
            }
        ),
        "unexpected missing-field outcome: {missing_field_outcome:?}"
    );

    std::fs::write(&path, br#"{"schema_version":1,"checkpoint":null}"#)
        .expect("write null checkpoint");
    assert!(matches!(
        store.inspect(),
        ResumeInspectionOutcome::Loaded(None)
    ));
}

#[test]
fn invalid_duration_and_newer_schema_keep_typed_distinctions() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join(PLAYLIST_RESUME_FILENAME);
    let store = PlaylistResumeStore::new(&path);
    std::fs::write(
        &path,
        br#"{"schema_version":1,"checkpoint":{"item_id":1,"locator_fingerprint_sha256":"0000000000000000000000000000000000000000000000000000000000000000","position":{"seconds":1,"nanoseconds":1000000000}}}"#,
    )
    .expect("write invalid duration");
    assert!(matches!(
        store.inspect(),
        ResumeInspectionOutcome::CorruptNeedsQuarantine {
            cause: ResumeCorruptCause::InvalidDomainValue,
            ..
        }
    ));

    std::fs::write(
        &path,
        br#"{"schema_version":2,"checkpoint":null,"future_secret":"untouched"}"#,
    )
    .expect("write newer schema");
    assert!(matches!(
        store.inspect(),
        ResumeInspectionOutcome::NewerSchemaSaveBlocked { schema_version: 2 }
    ));
}

#[test]
fn corrupt_sidecar_quarantines_without_touching_queue_file() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join(PLAYLIST_RESUME_FILENAME);
    let queue_path = directory.path().join(crate::PLAYLIST_STATE_FILENAME);
    std::fs::write(&queue_path, b"queue sentinel").expect("write queue sentinel");
    std::fs::write(&path, br#"{"schema_version":1,"checkpoint":"bad"}"#)
        .expect("write corrupt resume");
    let store = PlaylistResumeStore::new(&path);
    let ResumeInspectionOutcome::CorruptNeedsQuarantine {
        inspected_identity, ..
    } = store.inspect()
    else {
        panic!("corrupt outcome expected");
    };
    let outcome = store.apply_quarantine(
        &inspected_identity,
        &QuarantineFileName::resume_from_timestamp(SystemTime::UNIX_EPOCH),
    );
    assert!(matches!(outcome, QuarantineOutcome::Applied { .. }));
    assert_eq!(
        std::fs::read(queue_path).expect("queue remains readable"),
        b"queue sentinel"
    );
}

#[test]
fn locator_fingerprint_is_exact_and_never_contains_secret_url() {
    let secret = "https://user:password@example.test/private/video.mkv?token=secret";
    let locator = url_locator(secret);
    let checkpoint =
        ResumeCheckpoint::for_locator(item_id(1), &locator, Duration::ZERO).expect("fingerprint");
    let json = String::from_utf8(
        serialize_resume(Some(&checkpoint)).expect("serialize secret-safe checkpoint"),
    )
    .expect("utf8 json");
    assert!(!json.contains("password"));
    assert!(!json.contains("token"));
    assert!(!format!("{checkpoint:?}").contains("example.test"));
    assert_eq!(checkpoint.fingerprint_hex().len(), 64);

    let changed = url_locator("https://user:password@example.test/private/video.mkv?token=changed");
    assert!(
        !checkpoint
            .matches(item_id(1), &changed)
            .expect("compare fingerprint")
    );
}

#[test]
fn latest_only_worker_writes_newest_revision_and_private_permissions() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join(PLAYLIST_RESUME_FILENAME);
    let store = Arc::new(PlaylistResumeStore::new(&path));
    let locator = local_locator("latest.mkv");
    let mut worker = ResumeWorker::start(store.clone()).expect("start worker");
    for revision in 1..=32 {
        let snapshot = ResumeWriteSnapshot::new(
            ResumeSaveRevision::new(NonZeroU64::new(revision).expect("revision")),
            Some(
                ResumeCheckpoint::for_locator(item_id(9), &locator, Duration::from_secs(revision))
                    .expect("checkpoint"),
            ),
        );
        assert_eq!(worker.submit(snapshot), ResumeSubmitOutcome::Accepted);
    }
    let newest = ResumeWriteSnapshot::new(
        ResumeSaveRevision::new(NonZeroU64::new(33).expect("revision")),
        Some(
            ResumeCheckpoint::for_locator(item_id(9), &locator, Duration::from_secs(33))
                .expect("checkpoint"),
        ),
    );
    assert!(matches!(
        worker.shutdown(Some(newest), Duration::from_secs(2)),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));
    assert!(matches!(
        store.inspect(),
        ResumeInspectionOutcome::Loaded(Some(checkpoint))
            if checkpoint.position() == Duration::from_secs(33)
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(path)
            .expect("resume metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn resume_update_never_rewrites_large_playlist_state_file() {
    let directory = tempfile::tempdir().expect("temp directory");
    let queue_path = directory.path().join(crate::PLAYLIST_STATE_FILENAME);
    let queue_bytes = vec![b'q'; 256 * 1024];
    std::fs::write(&queue_path, &queue_bytes).expect("write large queue sentinel");
    let queue_metadata_before = std::fs::metadata(&queue_path).expect("queue metadata");

    let resume_path = directory.path().join(PLAYLIST_RESUME_FILENAME);
    let store = Arc::new(PlaylistResumeStore::new(&resume_path));
    let locator = local_locator("separate-sidecar.mkv");
    let mut worker = ResumeWorker::start(store).expect("start resume worker");
    let snapshot = ResumeWriteSnapshot::new(
        ResumeSaveRevision::new(NonZeroU64::new(1).expect("revision")),
        Some(
            ResumeCheckpoint::for_locator(item_id(3), &locator, Duration::from_secs(17))
                .expect("checkpoint"),
        ),
    );
    assert_eq!(worker.submit(snapshot), ResumeSubmitOutcome::Accepted);
    assert!(matches!(
        worker.shutdown(None, Duration::from_secs(2)),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));

    let queue_metadata_after = std::fs::metadata(&queue_path).expect("queue metadata after");
    assert_eq!(
        std::fs::read(&queue_path).expect("read queue sentinel"),
        queue_bytes
    );
    assert_eq!(queue_metadata_after.len(), queue_metadata_before.len());
    assert_eq!(
        queue_metadata_after.modified().expect("modified after"),
        queue_metadata_before.modified().expect("modified before")
    );
}

#[test]
fn observed_write_report_remains_available_for_bounded_shutdown_proof() {
    let directory = tempfile::tempdir().expect("temp directory");
    let store = Arc::new(PlaylistResumeStore::new(
        directory.path().join(PLAYLIST_RESUME_FILENAME),
    ));
    let locator = local_locator("report.mkv");
    let mut worker = ResumeWorker::start(store).expect("start resume worker");
    let snapshot = ResumeWriteSnapshot::new(
        ResumeSaveRevision::new(NonZeroU64::new(1).expect("revision")),
        Some(
            ResumeCheckpoint::for_locator(item_id(4), &locator, Duration::from_secs(19))
                .expect("checkpoint"),
        ),
    );
    assert_eq!(worker.submit(snapshot), ResumeSubmitOutcome::Accepted);

    let deadline = Instant::now() + Duration::from_secs(2);
    let observed_report = loop {
        if let Some(report) = worker.latest_report() {
            break report;
        }
        assert!(Instant::now() < deadline, "writer report timeout");
        std::thread::yield_now();
    };
    assert!(matches!(
        observed_report.outcome,
        AtomicWriteOutcome::Durable
    ));
    assert!(matches!(
        worker.shutdown(None, Duration::from_secs(2)),
        ResumeWorkerShutdownOutcome::Completed {
            final_report: Some(final_report)
        } if final_report == observed_report
    ));
}
