//! AUD-014: shutdown persistence принимает только typed settled seek position.

use super::*;

/// Записывает checkpoint из transient `Seeking` snapshot и читает его с настоящего sidecar store.
fn persisted_position_for_settlement(
    timeline_position: LifecycleTimelineCheckpointPosition,
) -> Duration {
    let directory = tempfile::tempdir().expect("temp resume directory");
    let store = Arc::new(PlaylistResumeStore::new(
        directory.path().join("playlist-resume.json"),
    ));
    let (controller, generation, media_instance_id) = exact_controller("aud014-shutdown.mp4");
    let binding = PlaylistRuntimeBinding {
        lifecycle_generation: super::super::super::PlaylistLifecycleGeneration(1),
        binding_generation: generation,
    };
    let mut owner = PlaylistResumePersistenceOwner::new(5_000, true);
    owner.install_store(store.clone());
    owner.activate_lineage(PlaylistLineagePersistence::Persistent);
    let seeking_snapshot = seekable_snapshot(
        media_instance_id,
        PlaybackState::Seeking,
        Duration::from_secs(10),
    );

    owner.force_snapshot(
        &controller,
        binding,
        &seeking_snapshot,
        timeline_position,
        Instant::now(),
    );
    assert!(matches!(
        owner.shutdown_until(ShutdownDeadline::after(Duration::from_secs(2))),
        ResumeWorkerShutdownOutcome::Completed { .. }
    ));

    match store.inspect() {
        ResumeInspectionOutcome::Loaded(Some(checkpoint)) => checkpoint.position(),
        unexpected => panic!("shutdown sidecar не сохранил settled checkpoint: {unexpected:?}"),
    }
}

#[test]
fn n14b_lifecycle_graceful_shutdown_persists_settled_seek_not_transient_state() {
    assert_eq!(
        persisted_position_for_settlement(LifecycleTimelineCheckpointPosition::SettledSeek(
            Duration::from_secs(90)
        ),),
        Duration::from_secs(90)
    );
    assert_eq!(
        persisted_position_for_settlement(
            LifecycleTimelineCheckpointPosition::CancelledPendingSeek(Duration::from_secs(10)),
        ),
        Duration::from_secs(10)
    );
}
