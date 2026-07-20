//! Focused S11 tests process-lifetime export owner-а и atomic writer adapter-а.

use std::fs;
use std::io;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use playlist_core::{
    AddPlaylistEntriesOutcome, CachedPlaylistMetadata, LocalLocator, PlaylistEntryDraft,
    PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
};
use tempfile::tempdir;

use crate::app_wake::AppWakeOwner;

use super::*;

/// Минимальная metadata не добавляет export-specific service policy.
fn metadata(label: &str) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video)
}

/// Возвращает queue с одним URL либо local item и опубликованным stable ID.
fn queue_with_draft(draft: PlaylistItemDraft) -> PlaylistQueue {
    let mut queue = PlaylistQueue::new();
    let outcome = queue
        .append_entries(vec![PlaylistEntryDraft::Single(draft)])
        .expect("export fixture append");
    assert!(matches!(outcome, AddPlaylistEntriesOutcome::Added(_)));
    queue
}

/// Production intent нельзя подменить неявным bool в writer callsite.
fn continuation(generation: u64, target_path: PathBuf) -> PlaylistExportConfirmationContinuation {
    PlaylistExportConfirmationContinuation {
        generation,
        target_path,
        document_bytes: b"#EXTM3U\n".to_vec(),
        overwrite_intent: PlaylistExportOverwriteIntent::ReplaceTargetSelectedBySaveDialog,
        flattened_compound_groups: false,
    }
}

fn wake_port() -> AppWakePort {
    AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime)
}

#[test]
fn sensitive_preflight_does_not_touch_target_before_confirmation() {
    let directory = tempdir().expect("tempdir");
    let target_path = directory.path().join("private.m3u8");
    fs::write(&target_path, b"old-bytes").expect("seed target");
    let queue = queue_with_draft(PlaylistItemDraft::url(
        SecretUrlLocator::from_reopenable_url(
            "https://media.example.test/watch?id=stable&token=secret",
        )
        .expect("secret URL"),
        metadata("secret"),
    ));
    let revisions_before = queue.revision_snapshot();
    let snapshot = PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full)
        .expect("immutable snapshot");
    let cancellation = AtomicBool::new(false);

    let completion = prepare_and_write_export(
        7,
        PlaylistExportFormat::M3u8,
        snapshot,
        target_path.clone(),
        PlaylistExportOverwriteIntent::ReplaceTargetSelectedBySaveDialog,
        &cancellation,
    );

    let PlaylistExportJobCompletion::AwaitingSensitiveConfirmation {
        locator_count,
        continuation,
        ..
    } = completion
    else {
        panic!("sensitive URL обязан ждать aggregated confirmation");
    };
    assert_eq!(locator_count, 1);
    assert_eq!(
        fs::read(&target_path).expect("read old target"),
        b"old-bytes"
    );
    assert_eq!(queue.revision_snapshot(), revisions_before);

    let written = write_prepared_export(continuation);
    assert!(matches!(
        written,
        PlaylistExportJobCompletion::Written {
            durability: PlaylistExportDurability::Durable,
            ..
        }
    ));
    assert!(
        fs::read_to_string(&target_path)
            .expect("read export")
            .starts_with("#EXTM3U")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&target_path)
                .expect("target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn public_export_is_durable_without_queue_or_revision_mutation() {
    let directory = tempdir().expect("tempdir");
    let source_path = directory.path().join("movie.webm");
    let target_path = directory.path().join("playlist.xspf");
    let queue = queue_with_draft(PlaylistItemDraft::local(
        LocalLocator::Native(source_path),
        None,
        metadata("movie"),
    ));
    let revisions_before = queue.revision_snapshot();
    let item_ids_before = queue.iter_playable_ids().collect::<Vec<_>>();
    let snapshot = PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full)
        .expect("immutable snapshot");
    let cancellation = AtomicBool::new(false);

    let completion = prepare_and_write_export(
        3,
        PlaylistExportFormat::Xspf,
        snapshot,
        target_path.clone(),
        PlaylistExportOverwriteIntent::ReplaceTargetSelectedBySaveDialog,
        &cancellation,
    );

    assert!(matches!(
        completion,
        PlaylistExportJobCompletion::Written {
            durability: PlaylistExportDurability::Durable,
            ..
        }
    ));
    assert_eq!(queue.revision_snapshot(), revisions_before);
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        item_ids_before
    );
    assert!(
        fs::read_to_string(target_path)
            .expect("read XSPF")
            .contains("<playlist")
    );
}

#[test]
fn cancellation_after_preflight_wins_before_first_target_mutation() {
    let directory = tempdir().expect("tempdir");
    let target_path = directory.path().join("cancelled.m3u8");
    let queue = queue_with_draft(PlaylistItemDraft::local(
        LocalLocator::Native(directory.path().join("movie.webm")),
        None,
        metadata("movie"),
    ));
    let snapshot = PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full)
        .expect("immutable snapshot");
    let cancellation = AtomicBool::new(true);

    let completion = prepare_and_write_export(
        9,
        PlaylistExportFormat::M3u8,
        snapshot,
        target_path.clone(),
        PlaylistExportOverwriteIntent::ReplaceTargetSelectedBySaveDialog,
        &cancellation,
    );

    assert!(matches!(
        completion,
        PlaylistExportJobCompletion::Cancelled { generation: 9 }
    ));
    assert!(!target_path.exists());
}

#[test]
fn writer_preserves_atomic_failure_and_post_rename_durability_distinction() {
    let target_path = PathBuf::from("/tmp/redacted-export-test.m3u8");
    let not_replaced =
        write_prepared_export_with(continuation(1, target_path.clone()), |_path, _bytes| {
            atomic_file_store::AtomicFileWriteOutcome::NotReplaced(
                atomic_file_store::AtomicFileWriteFailure {
                    stage: atomic_file_store::AtomicFileWriteStage::WriteTempFile,
                    cause: atomic_file_store::AtomicFileWriteCause::Io(io::ErrorKind::Other),
                },
            )
        });
    assert!(matches!(
        not_replaced,
        PlaylistExportJobCompletion::Failed {
            error: PlaylistExportJobError::AtomicWriteFailed,
            ..
        }
    ));

    let durability_unconfirmed =
        write_prepared_export_with(continuation(2, target_path), |_path, _bytes| {
            atomic_file_store::AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(
                atomic_file_store::DirectorySyncError::SyncDirectory(io::ErrorKind::Other),
            )
        });
    assert!(matches!(
        durability_unconfirmed,
        PlaylistExportJobCompletion::Written {
            durability: PlaylistExportDurability::ReplacedDurabilityUnconfirmed,
            ..
        }
    ));
}

#[test]
fn cancellation_suppresses_completion_published_before_owner_drain() {
    let mut job = PlaylistExportJob::spawn_runner(wake_port(), 1, "export-cancel-test", |_| {
        PlaylistExportJobCompletion::Written {
            generation: 1,
            durability: PlaylistExportDurability::Durable,
            flattened_compound_groups: false,
        }
    })
    .expect("spawn test job");
    job.join_handle
        .take()
        .expect("join handle")
        .join()
        .expect("join");
    job.cancellation_requested.store(true, Ordering::Release);
    let mut owner = PlaylistExportIoOwner {
        wake_port: wake_port(),
        generation: 1,
        job: Some(job),
    };

    assert!(matches!(
        owner.drain(),
        Some(PlaylistExportJobCompletion::Cancelled { generation: 1 })
    ));
}

#[test]
fn stale_generation_and_conflicting_writer_cannot_mutate_current_job() {
    let mut stale_job =
        PlaylistExportJob::spawn_runner(wake_port(), 1, "export-stale-test", |_| {
            PlaylistExportJobCompletion::Written {
                generation: 1,
                durability: PlaylistExportDurability::Durable,
                flattened_compound_groups: false,
            }
        })
        .expect("spawn stale job");
    stale_job
        .join_handle
        .take()
        .expect("join handle")
        .join()
        .expect("join");
    let mut owner = PlaylistExportIoOwner {
        wake_port: wake_port(),
        generation: 2,
        job: Some(stale_job),
    };
    assert!(owner.drain().is_none(), "stale terminal обязан подавляться");

    owner.job = Some(
        PlaylistExportJob::spawn_runner(wake_port(), 2, "export-conflict-test", |_| {
            PlaylistExportJobCompletion::Cancelled { generation: 2 }
        })
        .expect("spawn active job"),
    );
    let started = owner
        .start_confirmed(continuation(2, PathBuf::from("/tmp/never-written.m3u8")))
        .expect("typed conflict outcome");
    assert!(!started);
    owner.cancel_active();
    let _ = owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1)));
}

#[test]
fn shutdown_cancels_and_joins_active_export_worker() {
    let job = PlaylistExportJob::spawn_runner(
        wake_port(),
        1,
        "export-shutdown-test",
        |cancelled: Arc<AtomicBool>| {
            while !cancelled.load(Ordering::Acquire) {
                thread::yield_now();
            }
            PlaylistExportJobCompletion::Cancelled { generation: 1 }
        },
    )
    .expect("spawn shutdown job");
    let mut owner = PlaylistExportIoOwner {
        wake_port: wake_port(),
        generation: 1,
        job: Some(job),
    };

    let outcome = owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1)));

    assert_eq!(outcome, ProcessOwnerShutdownOutcome::Completed);
    assert!(!owner.is_open());
}

#[test]
fn worker_panic_keeps_exact_generation_for_owner_correlation() {
    let job = PlaylistExportJob::spawn_runner(wake_port(), 5, "export-panic-test", |_| {
        panic!("injected export panic")
    })
    .expect("spawn panic job");
    let mut owner = PlaylistExportIoOwner {
        wake_port: wake_port(),
        generation: 5,
        job: Some(job),
    };

    let completion = loop {
        if let Some(completion) = owner.drain() {
            break completion;
        }
        thread::yield_now();
    };

    assert!(matches!(
        completion,
        PlaylistExportJobCompletion::Failed {
            generation: 5,
            error: PlaylistExportJobError::WorkerPanicked,
        }
    ));
}
