use std::fs;
use std::sync::Arc;

use playlist_core::{
    CachedPlaylistMetadata, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue, RepeatMode,
    SecretUrlLocator,
};

use super::{AtomicSnapshotWriter, AtomicWriteOutcome, NotReplacedStage, SnapshotWriter};
use crate::{ImmutableSaveSnapshot, PlaylistStateSnapshot, PlaylistStateStore, SaveRevision};

fn captured_snapshot(revision: SaveRevision, item_count: usize) -> ImmutableSaveSnapshot {
    let mut queue = PlaylistQueue::new();
    for item_index in 0..item_count {
        let locator = SecretUrlLocator::from_reopenable_url(format!(
            "https://media.invalid/{item_index}.mp4?secret=redacted-in-debug"
        ))
        .expect("test URL непустой");
        let metadata =
            CachedPlaylistMetadata::new(format!("item-{item_index}"), PlaylistMediaKind::Video);
        queue
            .append_one(PlaylistItemDraft::url(locator, metadata))
            .expect("test queue не превышает capacity");
    }
    ImmutableSaveSnapshot::capture(
        revision,
        PlaylistStateSnapshot::new(&queue, RepeatMode::RepeatQueue),
    )
    .expect("малый test snapshot сериализуем")
}

#[test]
fn atomic_write_replaces_complete_json_and_preserves_allocator_watermark() {
    let directory = tempfile::tempdir().expect("tempdir доступен");
    let target_path = directory.path().join("playlist-state.json");
    fs::write(&target_path, b"old-state").expect("старый target создаётся");
    let store = Arc::new(PlaylistStateStore::new(&target_path));
    let writer = AtomicSnapshotWriter::new(store);

    let outcome = writer.write_snapshot(&captured_snapshot(SaveRevision::FIRST, 2));

    assert_eq!(outcome, AtomicWriteOutcome::Durable);
    let written_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&target_path).expect("target читается после replace"))
            .expect("target содержит complete JSON");
    assert_eq!(written_json["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(written_json["next_item_id"].as_u64(), Some(3));
}

#[cfg(unix)]
#[test]
fn atomic_write_creates_user_only_target_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("tempdir доступен");
    let target_path = directory.path().join("playlist-state.json");
    let store = Arc::new(PlaylistStateStore::new(&target_path));
    let writer = AtomicSnapshotWriter::new(store);

    assert_eq!(
        writer.write_snapshot(&captured_snapshot(SaveRevision::FIRST, 1)),
        AtomicWriteOutcome::Durable
    );
    let mode = fs::metadata(&target_path)
        .expect("target metadata доступна")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn rename_failure_keeps_target_and_cleans_only_owned_temp_file() {
    let directory = tempfile::tempdir().expect("tempdir доступен");
    let target_path = directory.path().join("playlist-state.json");
    fs::create_dir(&target_path).expect("directory target провоцирует rename failure");
    let target_marker = target_path.join("marker");
    fs::write(&target_marker, b"original").expect("marker создаётся");
    let foreign_temp = directory
        .path()
        .join(".playlist-state.json.save-foreign.tmp");
    fs::write(&foreign_temp, b"foreign").expect("чужой temp создаётся");
    let store = Arc::new(PlaylistStateStore::new(&target_path));
    let writer = AtomicSnapshotWriter::new(store);

    let outcome = writer.write_snapshot(&captured_snapshot(SaveRevision::FIRST, 1));

    assert!(matches!(
        outcome,
        AtomicWriteOutcome::NotReplaced(failure)
            if failure.stage == NotReplacedStage::RenameTempFile
    ));
    assert_eq!(
        fs::read(&target_marker).expect("marker не потерян"),
        b"original"
    );
    assert_eq!(
        fs::read(&foreign_temp).expect("чужой temp не удалён"),
        b"foreign"
    );
    let owned_temp_count = fs::read_dir(directory.path())
        .expect("directory перечисляется")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".playlist-state.json.save-")
                && entry.path() != foreign_temp
        })
        .count();
    assert_eq!(owned_temp_count, 0);
}
