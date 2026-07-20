use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use media_core::{DiscNumber, MediaDuration, TrackNumber, TvEpisodeNumber, TvSeasonNumber};
use playlist_core::{
    CachedPlaylistMetadata, ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath,
    LocalLocator, LocalSourceFingerprint, NextPlaylistItemId, PlaylistItem, PlaylistItemDraft,
    PlaylistItemId, PlaylistMediaKind, PlaylistQueue, PlaylistQueueRestore, RepeatMode,
    RestoredPlaylistItem, SecretUrlLocator, ShuffleHistoryCursor, ShuffleTraversalSnapshot,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::store::inspect_state_with_test_limits;
use crate::{
    CorruptStateCause, InspectionOutcome, PlaylistStateSnapshot, PlaylistStateStore,
    ProtectedStateCause, QuarantineFailureCause, QuarantineFileName, QuarantineOutcome,
    serialize_state,
};

fn item_id(value: u64) -> PlaylistItemId {
    PlaylistItemId::from_persistence_value(value).expect("test Item ID must be non-zero")
}

fn next_item_id(value: u64) -> NextPlaylistItemId {
    NextPlaylistItemId::from_persistence_value(value)
        .expect("test allocator watermark must be non-zero")
}

fn minimal_metadata(label: &str) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(label, PlaylistMediaKind::Unknown)
}

fn url_draft(url: &str, label: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::url(
        SecretUrlLocator::from_reopenable_url(url.to_owned()).expect("test URL is non-empty"),
        minimal_metadata(label),
    )
}

fn restored_url_item(id: u64, url: &str) -> RestoredPlaylistItem {
    RestoredPlaylistItem::new(item_id(id), url_draft(url, "url"))
}

fn queue_with_ids(ids: &[u64], next_id: u64, current: Option<u64>) -> PlaylistQueue {
    let restored_items = ids
        .iter()
        .map(|id| restored_url_item(*id, &format!("https://example.invalid/{id}")))
        .collect();
    PlaylistQueue::restore(PlaylistQueueRestore::new(
        restored_items,
        next_item_id(next_id),
        current.map(item_id),
    ))
    .expect("test queue must satisfy domain invariants")
}

/// Разрешает test position через borrowed playable read boundary.
fn playable_item_at(queue: &PlaylistQueue, index: usize) -> &PlaylistItem {
    queue
        .iter_playable_items()
        .nth(index)
        .expect("test playable index must exist")
}

fn write_state(path: &Path, queue: &PlaylistQueue, repeat_mode: RepeatMode) -> Vec<u8> {
    let encoded = serialize_state(PlaylistStateSnapshot::new(queue, repeat_mode))
        .expect("test snapshot must serialize");
    fs::write(path, &encoded).expect("test state write must succeed");
    encoded
}

fn loaded(store: &PlaylistStateStore) -> crate::LoadedPlaylistState {
    match store.inspect_state() {
        InspectionOutcome::Loaded(state) => state,
        other => panic!("expected Loaded, got {other:?}"),
    }
}

fn corrupt_identity(
    outcome: InspectionOutcome,
) -> (crate::InspectedFileIdentity, CorruptStateCause) {
    match outcome {
        InspectionOutcome::CorruptNeedsQuarantine {
            inspected_identity,
            cause,
        } => (inspected_identity, cause),
        other => panic!("expected CorruptNeedsQuarantine, got {other:?}"),
    }
}

fn full_metadata() -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new("fallback.mkv", PlaylistMediaKind::Video)
        .with_duration(Some(MediaDuration::from_nanos(9_876_543_210)))
        .with_title(Some("Полный заголовок".to_owned()))
        .with_artists(vec!["Первый артист".to_owned(), "Second Artist".to_owned()])
        .expect("two artists are inside domain cap")
        .with_album(Some("Album".to_owned()))
        .with_sequence(
            Some(DiscNumber::new(2)),
            Some(TrackNumber::new(11)),
            Some(TvSeasonNumber::new(4)),
            Some(TvEpisodeNumber::new(7)),
        )
}

#[test]
fn full_d12_cache_urls_and_every_local_encoding_roundtrip_exactly() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let modified_at = UNIX_EPOCH
        .checked_sub(Duration::new(3, 250))
        .expect("representable pre-epoch timestamp");
    let fingerprint = LocalSourceFingerprint::new(123_456, modified_at);

    let local_variants = [
        LocalLocator::Native(PathBuf::from("/media/видео.mkv")),
        LocalLocator::Foreign(ForeignPlatformPath::new(
            ForeignPathPlatform::MacOs,
            ForeignPathEncoding::Utf8("/Volumes/Media/movie.mov".to_owned()),
        )),
        LocalLocator::Foreign(ForeignPlatformPath::new(
            ForeignPathPlatform::Windows,
            ForeignPathEncoding::Utf8("C:\\Media\\movie.mkv".to_owned()),
        )),
        LocalLocator::Foreign(ForeignPlatformPath::new(
            ForeignPathPlatform::Windows,
            ForeignPathEncoding::Wide(vec![b'C' as u16, b':' as u16, 0xD800]),
        )),
        LocalLocator::Foreign(ForeignPlatformPath::new(
            ForeignPathPlatform::Other("future-os".to_owned()),
            ForeignPathEncoding::Opaque {
                encoding_name: "future-units".to_owned(),
                raw_units: vec![0, 7, u32::MAX],
            },
        )),
        LocalLocator::Foreign(ForeignPlatformPath::new(
            ForeignPathPlatform::Linux,
            ForeignPathEncoding::Bytes(vec![b'/', b'm', 0xFF]),
        )),
    ];
    let mut restored_items = Vec::new();
    for (index, locator) in local_variants.into_iter().enumerate() {
        restored_items.push(RestoredPlaylistItem::new(
            item_id(index as u64 + 10),
            PlaylistItemDraft::local(locator, Some(fingerprint), full_metadata()),
        ));
    }
    let secret_direct_url =
        "https://user:password@example.invalid/private/video.mp4?token=secret#part";
    restored_items.push(RestoredPlaylistItem::new(
        item_id(20),
        url_draft(secret_direct_url, "direct"),
    ));
    restored_items.push(RestoredPlaylistItem::new(
        item_id(21),
        url_draft("https://www.youtube.com/watch?v=stable-id", "yt_dlp"),
    ));

    let queue = PlaylistQueue::restore(PlaylistQueueRestore::new(
        restored_items,
        next_item_id(100),
        None,
    ))
    .expect("full queue restore");
    let snapshot = PlaylistStateSnapshot::new(&queue, RepeatMode::RepeatOne);
    assert!(!format!("{snapshot:?}").contains("password"));
    assert!(!format!("{snapshot:?}").contains("token=secret"));

    let encoded = write_state(&state_path, &queue, RepeatMode::RepeatOne);
    let json_text = std::str::from_utf8(&encoded).expect("serializer emits UTF-8");
    assert!(json_text.contains(secret_direct_url));
    assert!(json_text.contains("\"kind\": \"mac_os\""));
    assert!(json_text.contains("\"kind\": \"windows\""));
    assert!(json_text.contains("\"kind\": \"opaque\""));

    let loaded_state = loaded(&PlaylistStateStore::new(&state_path));
    assert_eq!(loaded_state.repeat_mode(), RepeatMode::RepeatOne);
    assert!(
        loaded_state
            .queue()
            .iter_playable_items()
            .take(5)
            .eq(queue.iter_playable_items().take(5))
    );
    assert!(
        loaded_state
            .queue()
            .iter_playable_items()
            .skip(6)
            .eq(queue.iter_playable_items().skip(6))
    );
    assert_eq!(
        playable_item_at(loaded_state.queue(), 5).cached_metadata(),
        playable_item_at(&queue, 5).cached_metadata()
    );
    assert_eq!(
        playable_item_at(loaded_state.queue(), 5).local_fingerprint(),
        playable_item_at(&queue, 5).local_fingerprint()
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let native_path = playable_item_at(loaded_state.queue(), 5)
            .locator()
            .as_local()
            .and_then(LocalLocator::expose_native_path_for_open)
            .expect("matching Linux bytes become an exact native path");
        assert_eq!(native_path.as_os_str().as_bytes(), &[b'/', b'm', 0xFF]);
    }
    assert!(loaded_state.queue().traversal_current().is_none());
    assert_eq!(
        loaded_state
            .queue()
            .next_item_id_snapshot()
            .expose_value_for_persistence(),
        100
    );
}

#[cfg(unix)]
#[test]
fn native_non_utf8_path_roundtrips_without_loss() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let original_bytes = vec![b'/', b'm', b'e', b'd', b'i', b'a', b'/', 0xFF];
    let native_path = PathBuf::from(std::ffi::OsString::from_vec(original_bytes.clone()));
    let restored = RestoredPlaylistItem::new(
        item_id(1),
        PlaylistItemDraft::local(
            LocalLocator::Native(native_path),
            None,
            minimal_metadata("non-utf"),
        ),
    );
    let queue = PlaylistQueue::restore(PlaylistQueueRestore::new(
        vec![restored],
        next_item_id(2),
        None,
    ))
    .expect("native non-UTF queue");
    let encoded = write_state(&state_path, &queue, RepeatMode::StopAtEnd);
    let json_text = std::str::from_utf8(&encoded).expect("JSON UTF-8");
    assert!(json_text.contains("\"kind\": \"linux\""));
    assert!(json_text.contains("\"kind\": \"bytes\""));

    let loaded_state = loaded(&PlaylistStateStore::new(&state_path));
    let loaded_path = playable_item_at(loaded_state.queue(), 0)
        .locator()
        .as_local()
        .and_then(LocalLocator::expose_native_path_for_open)
        .expect("same-platform raw bytes become native");
    assert_eq!(loaded_path.as_os_str().as_bytes(), original_bytes);
}

#[test]
fn repeated_factual_history_and_exact_upcoming_roundtrip() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let base = PlaylistQueueRestore::new(
        vec![
            restored_url_item(1, "https://example.invalid/1"),
            restored_url_item(2, "https://example.invalid/2"),
            restored_url_item(3, "https://example.invalid/3"),
        ],
        next_item_id(10),
        Some(item_id(2)),
    );
    let shuffle = ShuffleTraversalSnapshot::new(
        vec![item_id(1), item_id(2), item_id(1), item_id(2)],
        Some(ShuffleHistoryCursor::from_index(3)),
        vec![item_id(3)],
    );
    let queue = PlaylistQueue::restore_with_shuffle(base, shuffle).expect("valid repeated history");
    let expected_shuffle = queue.shuffle_traversal_snapshot();
    write_state(&state_path, &queue, RepeatMode::RepeatQueue);

    let loaded_state = loaded(&PlaylistStateStore::new(&state_path));
    assert_eq!(loaded_state.repeat_mode(), RepeatMode::RepeatQueue);
    assert_eq!(
        loaded_state.queue().shuffle_traversal_snapshot(),
        expected_shuffle
    );
    assert_eq!(
        loaded_state
            .queue()
            .traversal_current()
            .expect("current retained")
            .item_id(),
        item_id(2)
    );
}

#[test]
fn allocator_high_watermark_never_reuses_removed_or_cleared_ids_across_loads() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let store = PlaylistStateStore::new(&state_path);
    let mut queue = PlaylistQueue::new();

    queue
        .append_one(url_draft("https://example.invalid/one", "one"))
        .expect("append id 1");
    write_state(&state_path, &queue, RepeatMode::StopAtEnd);
    let (mut queue, _) = loaded(&store).into_parts();

    queue
        .append_one(url_draft("https://example.invalid/two", "two"))
        .expect("append id 2");
    assert_eq!(queue.iter_playable_ids().next_back(), Some(item_id(2)));
    let removed_id = queue
        .iter_playable_ids()
        .next_back()
        .expect("id 2 must be retained");
    let _removed = queue.remove(removed_id);
    write_state(&state_path, &queue, RepeatMode::StopAtEnd);
    let (mut queue, _) = loaded(&store).into_parts();

    queue
        .append_one(url_draft("https://example.invalid/three", "three"))
        .expect("append id 3 after removed high ID");
    assert_eq!(queue.iter_playable_ids().next_back(), Some(item_id(3)));
    let _cleared = queue.clear();
    write_state(&state_path, &queue, RepeatMode::StopAtEnd);
    let (mut queue, _) = loaded(&store).into_parts();

    queue
        .append_one(url_draft("https://example.invalid/four", "four"))
        .expect("append id 4 after persisted empty Clear");
    assert_eq!(queue.iter_playable_ids().next_back(), Some(item_id(4)));
}

#[test]
fn invalid_allocator_values_are_supported_corrupt_without_repair() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let queue = queue_with_ids(&[5], 9, None);
    let encoded = serialize_state(PlaylistStateSnapshot::new(&queue, RepeatMode::StopAtEnd))
        .expect("valid baseline");
    let baseline: Value = serde_json::from_slice(&encoded).expect("baseline JSON");

    let mut cases = Vec::new();
    let mut missing = baseline.clone();
    missing
        .as_object_mut()
        .expect("object")
        .remove("next_item_id");
    cases.push(missing);
    for invalid in [0_u64, 5] {
        let mut value = baseline.clone();
        value["next_item_id"] = json!(invalid);
        cases.push(value);
    }

    for case in cases {
        fs::write(&state_path, serde_json::to_vec(&case).expect("case JSON")).expect("write");
        let (_, cause) = corrupt_identity(PlaylistStateStore::new(&state_path).inspect_state());
        assert!(matches!(
            cause,
            CorruptStateCause::InvalidV1Payload | CorruptStateCause::InvalidQueueState
        ));
    }

    let overflowing = br#"{"schema_version":1,"next_item_id":18446744073709551616,"items":[],"current_item_id":null,"repeat_mode":"stop_at_end","shuffle_enabled":false,"shuffle_history":[],"shuffle_history_cursor":null,"shuffle_upcoming":[]}"#;
    fs::write(&state_path, overflowing).expect("write overflowing integer");
    let (_, cause) = corrupt_identity(PlaylistStateStore::new(&state_path).inspect_state());
    assert_eq!(cause, CorruptStateCause::InvalidV1Payload);
}

#[test]
fn envelope_budget_precedes_v1_limit_and_protects_incomplete_proof() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let queue = PlaylistQueue::new();
    let mut valid = serialize_state(PlaylistStateSnapshot::new(&queue, RepeatMode::StopAtEnd))
        .expect("valid empty state");
    valid.extend(std::iter::repeat_n(b' ', 256));
    fs::write(&state_path, &valid).expect("write oversized supported v1");
    let (_, cause) = corrupt_identity(inspect_state_with_test_limits(&state_path, 1024, 128));
    assert_eq!(cause, CorruptStateCause::SupportedFileTooLarge);

    let preamble = format!(
        "{{\"padding\":\"{}\",\"schema_version\":1}}",
        "x".repeat(300)
    );
    fs::write(&state_path, preamble).expect("write beyond envelope");
    assert!(matches!(
        inspect_state_with_test_limits(&state_path, 128, 64),
        InspectionOutcome::UnrecognizedVersionSaveBlocked {
            cause: ProtectedStateCause::EnvelopeBudgetExhausted
        }
    ));

    for tail in [
        "\"schema_version\":2",
        "\"schema_version\":1,\"schema_version\":2",
    ] {
        let protected = format!("{{\"padding\":\"{}\",{tail}}}", "x".repeat(300));
        fs::write(&state_path, protected).expect("write protected tail");
        assert!(matches!(
            inspect_state_with_test_limits(&state_path, 128, 64),
            InspectionOutcome::UnrecognizedVersionSaveBlocked {
                cause: ProtectedStateCause::EnvelopeBudgetExhausted
            }
        ));
    }

    let newer = format!(
        "{{\"padding\":\"{}\",\"schema_version\":2}}",
        "x".repeat(200)
    );
    fs::write(&state_path, &newer).expect("write newer");
    assert!(matches!(
        inspect_state_with_test_limits(&state_path, 512, 32),
        InspectionOutcome::NewerSchemaSaveBlocked { schema_version: 2 }
    ));
    assert_eq!(
        fs::read_to_string(&state_path).expect("source retained"),
        newer
    );
}

#[test]
fn capacity_history_cap_and_unknown_enum_are_supported_corrupt() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let metadata = json!({
        "fallback_display_name": "x",
        "media_kind": "unknown",
        "duration": null,
        "title": null,
        "artists": [],
        "album": null,
        "disc_number": null,
        "track_number": null,
        "season_number": null,
        "episode_number": null
    });
    let mut oversized_items = Vec::with_capacity(50_001);
    for id in 1_u64..=50_001 {
        oversized_items.push(json!({
            "item_id": id,
            "locator": {"kind": "url", "reopenable_url": "https://example.invalid/x"},
            "local_fingerprint": null,
            "cached_metadata": metadata.clone()
        }));
    }
    let over_capacity = json!({
        "schema_version": 1,
        "next_item_id": 50_002,
        "items": oversized_items,
        "current_item_id": null,
        "repeat_mode": "stop_at_end",
        "shuffle_enabled": false,
        "shuffle_history": [],
        "shuffle_history_cursor": null,
        "shuffle_upcoming": []
    });
    let over_capacity_bytes = serde_json::to_vec(&over_capacity).expect("capacity JSON");
    assert!(
        over_capacity_bytes.len() as u64 <= crate::MAX_SUPPORTED_V1_STATE_BYTES,
        "fixture must exercise domain cap rather than file cap"
    );
    fs::write(&state_path, over_capacity_bytes).expect("write over-cap state");
    let (_, cause) = corrupt_identity(PlaylistStateStore::new(&state_path).inspect_state());
    assert_eq!(cause, CorruptStateCause::ResourceLimitExceeded);

    let queue = queue_with_ids(&[1], 2, Some(1));
    let valid = serialize_state(PlaylistStateSnapshot::new(&queue, RepeatMode::StopAtEnd))
        .expect("valid baseline");
    let mut history_cap: Value = serde_json::from_slice(&valid).expect("baseline JSON");
    history_cap["shuffle_enabled"] = json!(true);
    history_cap["shuffle_history"] = Value::Array(vec![json!(1); 1_025]);
    history_cap["shuffle_history_cursor"] = json!(1_024);
    fs::write(
        &state_path,
        serde_json::to_vec(&history_cap).expect("history JSON"),
    )
    .expect("write history-cap state");
    let (_, cause) = corrupt_identity(PlaylistStateStore::new(&state_path).inspect_state());
    assert_eq!(cause, CorruptStateCause::ResourceLimitExceeded);

    let mut invalid_enum: Value = serde_json::from_slice(&valid).expect("baseline JSON");
    invalid_enum["repeat_mode"] = json!("future_repeat_mode");
    fs::write(
        &state_path,
        serde_json::to_vec(&invalid_enum).expect("enum JSON"),
    )
    .expect("write enum state");
    let (_, cause) = corrupt_identity(PlaylistStateStore::new(&state_path).inspect_state());
    assert_eq!(cause, CorruptStateCause::InvalidV1Payload);
}

#[test]
fn top_level_version_classification_rejects_spoof_and_both_duplicate_orders() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let cases = [
        ("{}", ProtectedStateCause::MissingSchemaVersion),
        (
            r#"{"schema_version":"#,
            ProtectedStateCause::InvalidEnvelope,
        ),
        (
            r#"{"payload":{"schema_version":1}}"#,
            ProtectedStateCause::MissingSchemaVersion,
        ),
        (
            r#"{"schema_version":"1"}"#,
            ProtectedStateCause::NonIntegerSchemaVersion,
        ),
        (
            r#"{"schema_version":1,"schema_version":2}"#,
            ProtectedStateCause::DuplicateSchemaVersion,
        ),
        (
            r#"{"schema_version":2,"schema_version":1}"#,
            ProtectedStateCause::DuplicateSchemaVersion,
        ),
    ];

    for (source, expected_cause) in cases {
        fs::write(&state_path, source).expect("write protected case");
        assert!(matches!(
            PlaylistStateStore::new(&state_path).inspect_state(),
            InspectionOutcome::UnrecognizedVersionSaveBlocked { cause }
                if cause == expected_cause
        ));
        assert_eq!(fs::read_to_string(&state_path).expect("no touch"), source);
    }
}

#[test]
fn duplicate_after_large_preamble_inside_budget_is_no_touch() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let source = format!(
        "{{\"padding\":\"{}\",\"schema_version\":1,\"schema_version\":2}}",
        "x".repeat(400)
    );
    fs::write(&state_path, &source).expect("write duplicate");
    assert!(matches!(
        inspect_state_with_test_limits(&state_path, 1024, 128),
        InspectionOutcome::UnrecognizedVersionSaveBlocked {
            cause: ProtectedStateCause::DuplicateSchemaVersion
        }
    ));
    assert_eq!(fs::read_to_string(&state_path).expect("no touch"), source);
}

#[test]
fn malformed_supported_payload_is_inspected_then_explicitly_quarantined() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    fs::write(&state_path, r#"{"schema_version":1,"items":[]}"#).expect("write malformed v1");
    let store = PlaylistStateStore::new(&state_path);
    let (identity, cause) = corrupt_identity(store.inspect_state());
    assert_eq!(cause, CorruptStateCause::InvalidV1Payload);
    assert!(state_path.exists(), "inspection must not rename source");

    let name = QuarantineFileName::from_timestamp(UNIX_EPOCH + Duration::from_secs(42));
    let quarantine_path = match store.apply_quarantine(&identity, &name) {
        QuarantineOutcome::Applied { quarantine_path } => quarantine_path,
        other => panic!("expected applied quarantine, got {other:?}"),
    };
    assert!(!state_path.exists());
    assert!(quarantine_path.exists());
    assert_eq!(
        quarantine_path.file_name().expect("filename"),
        "playlist-state.corrupt-42-000000000.json"
    );
}

#[test]
fn quarantine_rejects_collision_and_changed_source_without_overwrite() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let source = r#"{"schema_version":1,"items":[]}"#;
    fs::write(&state_path, source).expect("write corrupt source");
    let store = PlaylistStateStore::new(&state_path);
    let (identity, _) = corrupt_identity(store.inspect_state());
    let name = QuarantineFileName::from_timestamp(UNIX_EPOCH + Duration::from_secs(7));
    let collision_path = temp_dir
        .path()
        .join("playlist-state.corrupt-7-000000000.json");
    fs::write(&collision_path, "keep me").expect("write collision");

    assert!(matches!(
        store.apply_quarantine(&identity, &name),
        QuarantineOutcome::FailedSaveBlocked {
            cause: QuarantineFailureCause::DestinationAlreadyExists
        }
    ));
    assert_eq!(
        fs::read_to_string(&collision_path).expect("collision retained"),
        "keep me"
    );
    assert_eq!(
        fs::read_to_string(&state_path).expect("source retained"),
        source
    );

    fs::write(&state_path, r#"{"schema_version":1,"items":[1]}"#).expect("change inspected source");
    assert!(matches!(
        store.apply_quarantine(
            &identity,
            &QuarantineFileName::from_timestamp(UNIX_EPOCH + Duration::from_secs(8))
        ),
        QuarantineOutcome::SourceChanged
    ));
    assert!(state_path.exists());
}

#[cfg(unix)]
#[test]
fn quarantine_rename_failure_keeps_source_and_blocks_save() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    fs::write(&state_path, r#"{"schema_version":1,"items":[]}"#).expect("write corrupt source");
    let store = PlaylistStateStore::new(&state_path);
    let (identity, _) = corrupt_identity(store.inspect_state());
    let original_permissions = fs::metadata(temp_dir.path())
        .expect("dir metadata")
        .permissions();
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o500))
        .expect("make directory non-writable");

    let outcome = store.apply_quarantine(
        &identity,
        &QuarantineFileName::from_timestamp(UNIX_EPOCH + Duration::from_secs(9)),
    );
    fs::set_permissions(temp_dir.path(), original_permissions).expect("restore permissions");
    assert!(matches!(
        outcome,
        QuarantineOutcome::FailedSaveBlocked {
            cause: QuarantineFailureCause::MoveFailed(_)
        }
    ));
    assert!(state_path.exists());
}

#[test]
fn invalid_references_cursor_and_upcoming_duplicates_are_corrupt() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let base = queue_with_ids(&[1, 2], 3, Some(1));
    let encoded = serialize_state(PlaylistStateSnapshot::new(&base, RepeatMode::StopAtEnd))
        .expect("baseline");
    let baseline: Value = serde_json::from_slice(&encoded).expect("baseline JSON");

    let mut invalid_current = baseline.clone();
    invalid_current["current_item_id"] = json!(99);
    let mut invalid_history = baseline.clone();
    invalid_history["shuffle_enabled"] = json!(true);
    invalid_history["shuffle_history"] = json!([99]);
    invalid_history["shuffle_history_cursor"] = json!(0);
    invalid_history["shuffle_upcoming"] = json!([2]);
    let mut duplicate_upcoming = baseline.clone();
    duplicate_upcoming["shuffle_enabled"] = json!(true);
    duplicate_upcoming["shuffle_history"] = json!([1]);
    duplicate_upcoming["shuffle_history_cursor"] = json!(0);
    duplicate_upcoming["shuffle_upcoming"] = json!([2, 2]);
    let mut invalid_cursor = baseline;
    invalid_cursor["shuffle_enabled"] = json!(true);
    invalid_cursor["shuffle_history"] = json!([1]);
    invalid_cursor["shuffle_history_cursor"] = json!(7);
    invalid_cursor["shuffle_upcoming"] = json!([2]);

    for value in [
        invalid_current,
        invalid_history,
        duplicate_upcoming,
        invalid_cursor,
    ] {
        fs::write(&state_path, serde_json::to_vec(&value).expect("case JSON")).expect("write");
        assert!(matches!(
            PlaylistStateStore::new(&state_path).inspect_state(),
            InspectionOutcome::CorruptNeedsQuarantine { .. }
        ));
    }
}

#[test]
fn missing_state_is_normal_and_newer_schema_is_never_modified() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let store = PlaylistStateStore::new(&state_path);
    assert!(matches!(store.inspect_state(), InspectionOutcome::Missing));

    let newer = r#"{"schema_version":77,"future":{"secret":"untouched"}}"#;
    fs::write(&state_path, newer).expect("write newer schema");
    assert!(matches!(
        store.inspect_state(),
        InspectionOutcome::NewerSchemaSaveBlocked { schema_version: 77 }
    ));
    assert_eq!(
        fs::read_to_string(&state_path).expect("newer retained"),
        newer
    );
}

#[cfg(unix)]
#[test]
fn no_follow_inspection_rejects_symlink_source() {
    use std::os::unix::fs::symlink;

    let temp_dir = TempDir::new().expect("tempdir");
    let target_path = temp_dir.path().join("target.json");
    let state_path = temp_dir.path().join("playlist-state.json");
    fs::write(&target_path, r#"{"schema_version":2}"#).expect("write target");
    symlink(&target_path, &state_path).expect("create symlink");
    assert!(matches!(
        PlaylistStateStore::new(&state_path).inspect_state(),
        InspectionOutcome::UnrecognizedVersionSaveBlocked {
            cause: ProtectedStateCause::SourceIsNotRegularFile
        }
    ));
}
