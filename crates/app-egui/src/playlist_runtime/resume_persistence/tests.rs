use std::num::NonZeroU64;
use std::path::PathBuf;

use player_core::{MediaInstanceId, PlaybackState, PlayerSnapshot};
use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};
use playlist_state::{
    PlaylistResumeStore, ResumeCheckpoint, ResumeInspectionOutcome, ResumeSaveRevision,
    ResumeWorker, ResumeWorkerShutdownOutcome, ResumeWriteSnapshot,
};

use super::*;

mod aud014_shutdown_checkpoint;
use crate::playlist_runtime::controller::ControllerAppendOutcome;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity must be non-zero")
}

fn playlist_draft(label: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(label)),
        None,
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
    )
}

fn exact_controller(
    label: &str,
) -> (
    PlaylistController,
    PlaylistBindingGeneration,
    MediaInstanceId,
) {
    let mut controller = PlaylistController::new();
    let item_id = match controller
        .append(vec![playlist_draft(label)])
        .expect("append exact resume item")
    {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids[0],
        ControllerAppendOutcome::NoItemsProvided => panic!("fixture must append one item"),
    };
    controller
        .queue
        .set_traversal_current(item_id)
        .expect("set exact current");
    let binding_generation = PlaylistBindingGeneration(7);
    let media_instance_id = MediaInstanceId::from_non_zero(non_zero(8));
    controller.active_media = Some(ActiveMediaIdentity::installed(
        Some(item_id),
        ActiveMediaLineageId::from_non_zero(non_zero(9)),
        media_instance_id,
        binding_generation,
    ));
    (controller, binding_generation, media_instance_id)
}

fn seekable_snapshot(
    media_instance_id: MediaInstanceId,
    playback_state: PlaybackState,
    position: Duration,
) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.media_instance_id = Some(media_instance_id);
    snapshot.playback_state = playback_state;
    snapshot.current_position = position;
    snapshot.duration = Some(Duration::from_secs(120));
    snapshot.timeline.seekable = true;
    snapshot
}

#[test]
fn exact_correlation_rejects_stale_generation_instance_and_tombstone() {
    let directory = tempfile::tempdir().expect("temp directory");
    let store = Arc::new(PlaylistResumeStore::new(
        directory.path().join("playlist-resume.json"),
    ));
    let (mut controller, generation, instance) = exact_controller("exact.mp4");
    let mut owner = PlaylistResumePersistenceOwner::new(5_000, true);
    owner.install_store(store.clone());
    owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    let now = Instant::now();

    owner.record_installed(
        &controller,
        generation,
        instance,
        InstalledCheckpointPosition::Seekable(Duration::from_secs(11)),
        now,
    );
    assert_eq!(owner.next_revision, 2);
    owner.record_installed(
        &controller,
        PlaylistBindingGeneration(6),
        instance,
        InstalledCheckpointPosition::Seekable(Duration::from_secs(22)),
        now,
    );
    owner.record_installed(
        &controller,
        generation,
        MediaInstanceId::from_non_zero(non_zero(10)),
        InstalledCheckpointPosition::Seekable(Duration::from_secs(33)),
        now,
    );
    controller.active_media = Some(ActiveMediaIdentity::installed(
        None,
        ActiveMediaLineageId::from_non_zero(non_zero(9)),
        instance,
        generation,
    ));
    owner.record_installed(
        &controller,
        generation,
        instance,
        InstalledCheckpointPosition::Seekable(Duration::from_secs(44)),
        now,
    );
    assert_eq!(owner.next_revision, 2);

    assert!(matches!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(2))),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));
    assert!(matches!(
        store.inspect(),
        ResumeInspectionOutcome::Loaded(Some(checkpoint))
            if checkpoint.position() == Duration::from_secs(11)
    ));
}

#[test]
fn periodic_and_immediate_edges_are_latest_only_and_ended_resets_to_zero() {
    let directory = tempfile::tempdir().expect("temp directory");
    let store = Arc::new(PlaylistResumeStore::new(
        directory.path().join("playlist-resume.json"),
    ));
    let (controller, generation, instance) = exact_controller("edges.mp4");
    let binding = PlaylistRuntimeBinding {
        lifecycle_generation: super::super::PlaylistLifecycleGeneration(1),
        binding_generation: generation,
    };
    let mut owner = PlaylistResumePersistenceOwner::new(5_000, true);
    owner.install_store(store.clone());
    owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    let start = Instant::now();

    let playing_ten = seekable_snapshot(instance, PlaybackState::Playing, Duration::from_secs(10));
    owner.observe_snapshot(&controller, binding, &playing_ten, start);
    assert_eq!(owner.next_revision, 2);
    owner.observe_snapshot(
        &controller,
        binding,
        &playing_ten,
        start + Duration::from_secs(5),
    );
    assert_eq!(owner.next_revision, 2);

    let playing_eleven =
        seekable_snapshot(instance, PlaybackState::Playing, Duration::from_secs(11));
    owner.observe_snapshot(
        &controller,
        binding,
        &playing_eleven,
        start + Duration::from_secs(10),
    );
    let paused = seekable_snapshot(instance, PlaybackState::Paused, Duration::from_secs(12));
    owner.observe_snapshot(
        &controller,
        binding,
        &paused,
        start + Duration::from_secs(11),
    );
    owner.observe_snapshot(
        &controller,
        binding,
        &paused,
        start + Duration::from_secs(12),
    );
    let stopped = seekable_snapshot(instance, PlaybackState::Stopped, Duration::from_secs(13));
    owner.observe_snapshot(
        &controller,
        binding,
        &stopped,
        start + Duration::from_secs(13),
    );
    let ended = seekable_snapshot(instance, PlaybackState::Ended, Duration::from_secs(120));
    owner.observe_snapshot(
        &controller,
        binding,
        &ended,
        start + Duration::from_secs(14),
    );
    assert_eq!(owner.next_revision, 6);

    assert!(matches!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(2))),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));
    assert!(matches!(
        store.inspect(),
        ResumeInspectionOutcome::Loaded(Some(checkpoint))
            if checkpoint.position() == Duration::ZERO
    ));
}

#[test]
fn non_seekable_media_writes_explicit_null_and_non_persistent_lineage_writes_nothing() {
    let directory = tempfile::tempdir().expect("temp directory");
    let persistent_path = directory.path().join("persistent-resume.json");
    let persistent_store = Arc::new(PlaylistResumeStore::new(&persistent_path));
    let (controller, generation, instance) = exact_controller("stream.mp4");
    let mut persistent_owner = PlaylistResumePersistenceOwner::new(5_000, true);
    persistent_owner.install_store(persistent_store.clone());
    persistent_owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    persistent_owner.record_installed(
        &controller,
        generation,
        instance,
        InstalledCheckpointPosition::NonSeekable,
        Instant::now(),
    );
    assert!(matches!(
        persistent_owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(2))),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));
    assert!(matches!(
        persistent_store.inspect(),
        ResumeInspectionOutcome::Loaded(None)
    ));

    let blocked_path = directory.path().join("blocked-resume.json");
    let blocked_store = Arc::new(PlaylistResumeStore::new(&blocked_path));
    let mut blocked_owner = PlaylistResumePersistenceOwner::new(5_000, true);
    blocked_owner.install_store(blocked_store);
    blocked_owner.record_installed(
        &controller,
        generation,
        instance,
        InstalledCheckpointPosition::Seekable(Duration::from_secs(55)),
        Instant::now(),
    );
    assert!(!blocked_path.exists());
}

#[test]
fn live_media_never_writes_or_clears_persistent_resume_checkpoint() {
    let directory = tempfile::tempdir().expect("temp directory");
    let resume_path = directory.path().join("live-resume.json");
    let store = Arc::new(PlaylistResumeStore::new(&resume_path));
    let (controller, generation, instance) = exact_controller("live.m3u8");
    let mut owner = PlaylistResumePersistenceOwner::new(5_000, true);
    owner.install_store(store);
    owner.activate_lineage(PlaylistLineagePersistence::Persistent);

    owner.record_installed(
        &controller,
        generation,
        instance,
        InstalledCheckpointPosition::Live,
        Instant::now(),
    );

    assert_eq!(owner.next_revision, 1);
    assert!(!resume_path.exists());
}

#[test]
fn clear_writes_null_even_when_regular_resume_capture_is_disabled() {
    let directory = tempfile::tempdir().expect("temp directory");
    let resume_path = directory.path().join("clear-resume.json");
    let resume_store = Arc::new(PlaylistResumeStore::new(&resume_path));
    let (controller, generation, instance) = exact_controller("clear.mp4");
    let mut owner = PlaylistResumePersistenceOwner::new(5_000, true);
    owner.install_store(resume_store.clone());
    owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    owner.record_installed(
        &controller,
        generation,
        instance,
        InstalledCheckpointPosition::Seekable(Duration::from_secs(55)),
        Instant::now(),
    );
    owner.set_enabled(false);

    owner.clear_after_playlist_clear(Instant::now());

    assert!(matches!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(2))),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));
    assert!(matches!(
        resume_store.inspect(),
        ResumeInspectionOutcome::Loaded(None)
    ));
}

#[test]
fn live_interval_reschedule_preserves_pending_snapshot_and_changes_next_deadline() {
    let mut schedule = ResumeIntervalSchedule::new(5_000);
    let start = Instant::now();
    assert!(schedule.periodic_capture_is_due(start));
    schedule.record_capture(start);
    schedule.reschedule(1_000);
    assert!(!schedule.periodic_capture_is_due(start + Duration::from_millis(999)));
    assert!(schedule.periodic_capture_is_due(start + Duration::from_millis(1_000)));
}

#[test]
fn startup_position_restores_only_exact_item_and_locator_fingerprint() {
    let directory = tempfile::tempdir().expect("temp directory");
    let resume_path = directory.path().join("playlist-resume.json");
    let store = Arc::new(PlaylistResumeStore::new(&resume_path));
    let (controller, _generation, _instance) = exact_controller("restored-current.mp4");
    let current_item_id = controller
        .queue()
        .traversal_current()
        .expect("current item")
        .item_id();
    let current_locator = controller
        .queue()
        .item(current_item_id)
        .expect("current item payload")
        .locator()
        .clone();
    let checkpoint =
        ResumeCheckpoint::for_locator(current_item_id, &current_locator, Duration::from_secs(42))
            .expect("exact fingerprint");
    let mut writer = ResumeWorker::start(store.clone()).expect("start fixture writer");
    assert!(matches!(
        writer.submit(ResumeWriteSnapshot::new(
            ResumeSaveRevision::new(non_zero(1)),
            Some(checkpoint),
        )),
        playlist_state::ResumeSubmitOutcome::Accepted
    ));
    assert!(matches!(
        writer.shutdown(None, Duration::from_secs(2)),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));

    let mut exact_owner = PlaylistResumePersistenceOwner::new(5_000, true);
    exact_owner.install_store(store);
    exact_owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    assert_eq!(
        exact_owner.startup_position(current_item_id, &current_locator),
        StartupPosition::Restore(Duration::from_secs(42))
    );

    let stale_path = directory.path().join("stale-resume.json");
    let stale_store = Arc::new(PlaylistResumeStore::new(&stale_path));
    let stale_locator = playlist_core::PlaylistLocator::Local(LocalLocator::Native(PathBuf::from(
        "other-file.mp4",
    )));
    let stale_checkpoint =
        ResumeCheckpoint::for_locator(current_item_id, &stale_locator, Duration::from_secs(77))
            .expect("stale fingerprint");
    let mut stale_writer = ResumeWorker::start(stale_store.clone()).expect("start stale writer");
    assert!(matches!(
        stale_writer.submit(ResumeWriteSnapshot::new(
            ResumeSaveRevision::new(non_zero(1)),
            Some(stale_checkpoint),
        )),
        playlist_state::ResumeSubmitOutcome::Accepted
    ));
    assert!(matches!(
        stale_writer.shutdown(None, Duration::from_secs(2)),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));
    let mut stale_owner = PlaylistResumePersistenceOwner::new(5_000, true);
    stale_owner.install_store(stale_store);
    stale_owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    assert_eq!(
        stale_owner.startup_position(current_item_id, &current_locator),
        StartupPosition::KeepStart
    );
}

#[test]
fn newer_resume_schema_blocks_worker_and_is_never_overwritten() {
    let directory = tempfile::tempdir().expect("temp directory");
    let resume_path = directory.path().join("playlist-resume.json");
    let newer_bytes = br#"{"schema_version":2,"checkpoint":null}"#;
    std::fs::write(&resume_path, newer_bytes).expect("write newer fixture");
    let store = Arc::new(PlaylistResumeStore::new(&resume_path));
    let (controller, generation, instance) = exact_controller("protected.mp4");
    let mut owner = PlaylistResumePersistenceOwner::new(5_000, true);

    owner.install_store(store);
    owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    owner.record_installed(
        &controller,
        generation,
        instance,
        InstalledCheckpointPosition::Seekable(Duration::from_secs(23)),
        Instant::now(),
    );

    assert!(owner.worker.is_none());
    assert_eq!(
        std::fs::read(&resume_path).expect("read protected fixture"),
        newer_bytes
    );
}
