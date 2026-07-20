use std::fs;
use std::num::NonZeroU32;
use std::path::Path;

use media_core::{MediaDuration, MediaTime};
use playlist_core::{
    CachedPlaylistMetadata, ClearQueueOutcome, DurableReopenLocator, ForeignPathEncoding,
    ForeignPathPlatform, ForeignPlatformPath, LocalLocator, NextPlaylistCompoundGroupId,
    NextPlaylistItemId, PlaylistAncillaryTrackHint, PlaylistAncillaryTrackOrigin,
    PlaylistAncillaryTrackSelectionKind, PlaylistCompoundDurablePayload,
    PlaylistCompoundGroupDraft, PlaylistCompoundGroupId, PlaylistEntryId,
    PlaylistImportAvailability, PlaylistImportProvenance, PlaylistImportSourceKind,
    PlaylistItemDraft, PlaylistItemId, PlaylistLocator, PlaylistMediaKind, PlaylistPlaybackSpan,
    PlaylistQueue, PlaylistQueueRestore, PlaylistSingleDurablePayload, RepeatMode,
    RestoredPlaylistCompoundGroup, RestoredPlaylistEntry, RestoredPlaylistItem, SecretUrlLocator,
    ServiceReopenMaterialKind, ShuffleHistoryCursor, ShuffleTraversalSnapshot,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::{
    CorruptStateCause, ImmutableSaveSnapshot, InspectionOutcome, PlaylistStateSnapshot,
    PlaylistStateStore, SaveRevision, serialize_state,
};

const ACKNOWLEDGED_SECRET_URL: &str =
    "https://user:password@example.invalid/private/root?token=secret#fragment";
const SERVICE_CHILD_SECRET: &[u8] = b"stable-child-secret-identity";

fn item_id(value: u64) -> PlaylistItemId {
    PlaylistItemId::from_persistence_value(value).expect("fixture Item ID is non-zero")
}

fn group_id(value: u64) -> PlaylistCompoundGroupId {
    PlaylistCompoundGroupId::from_persistence_value(value).expect("fixture Group ID is non-zero")
}

fn next_item_id(value: u64) -> NextPlaylistItemId {
    NextPlaylistItemId::from_persistence_value(value).expect("fixture Item watermark is non-zero")
}

fn next_group_id(value: u64) -> NextPlaylistCompoundGroupId {
    NextPlaylistCompoundGroupId::from_persistence_value(value)
        .expect("fixture Group watermark is non-zero")
}

fn metadata(label: &str) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video)
}

fn secret_url(value: &str) -> SecretUrlLocator {
    SecretUrlLocator::from_reopenable_url(value.to_owned()).expect("fixture URL is valid")
}

fn root_provenance() -> PlaylistImportProvenance {
    PlaylistImportProvenance::new(
        DurableReopenLocator::url(secret_url(ACKNOWLEDGED_SECRET_URL)),
        PlaylistImportSourceKind::Service,
        NonZeroU32::new(2),
    )
}

fn service_child_payload() -> PlaylistSingleDurablePayload {
    let reopen_locator = DurableReopenLocator::from_service_payload(
        "yt_dlp",
        1,
        ServiceReopenMaterialKind::StableExtractorIdentity,
        SERVICE_CHILD_SECRET,
    )
    .expect("stable service child is durable");
    let external_non_utf_locator =
        DurableReopenLocator::local(LocalLocator::Foreign(ForeignPlatformPath::new(
            ForeignPathPlatform::Windows,
            ForeignPathEncoding::Wide(vec![b'C' as u16, b':' as u16, 0xD800]),
        )));
    let ancillary_hint = PlaylistAncillaryTrackHint::new(
        "subtitle-main",
        Some("uk".to_owned()),
        Some("Українські субтитри".to_owned()),
        PlaylistAncillaryTrackSelectionKind::Manual,
        PlaylistAncillaryTrackOrigin::External(external_non_utf_locator),
        Some("vtt-stable".to_owned()),
    )
    .expect("bounded ancillary fixture");
    let playback_span = PlaylistPlaybackSpan::from_start_and_duration(
        MediaTime::from_secs(5),
        MediaDuration::from_secs(10),
    )
    .expect("positive playback span");

    PlaylistSingleDurablePayload::new(
        reopen_locator,
        Some(playback_span),
        vec![ancillary_hint],
        root_provenance(),
        PlaylistImportAvailability::Unavailable,
    )
    .expect("bounded durable single payload")
}

fn restored_url_item(
    persisted_item_id: u64,
    label: &str,
    durable_payload: Option<PlaylistSingleDurablePayload>,
) -> RestoredPlaylistItem {
    let mut draft = PlaylistItemDraft::url(secret_url(ACKNOWLEDGED_SECRET_URL), metadata(label));
    if let Some(payload) = durable_payload {
        draft = draft.with_durable_payload(payload);
    }
    RestoredPlaylistItem::new(item_id(persisted_item_id), draft)
}

fn mixed_v2_queue() -> PlaylistQueue {
    let part_payload = service_child_payload();
    let part_drafts = vec![
        PlaylistItemDraft::url(secret_url(ACKNOWLEDGED_SECRET_URL), metadata("part-1"))
            .with_durable_payload(part_payload),
        PlaylistItemDraft::url(
            secret_url("https://example.invalid/part-2"),
            metadata("part-2"),
        ),
    ];
    let group_durable_payload = PlaylistCompoundDurablePayload::new(
        DurableReopenLocator::url(secret_url(ACKNOWLEDGED_SECRET_URL)),
        root_provenance(),
    );
    let group_draft = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Url(secret_url(ACKNOWLEDGED_SECRET_URL)),
        metadata("compound"),
        part_drafts,
    )
    .expect("compound fixture is non-empty")
    .with_durable_payload(group_durable_payload);
    let restored_group = RestoredPlaylistCompoundGroup::new(
        group_id(7),
        group_draft,
        vec![item_id(20), item_id(21)],
    )
    .expect("part identity count matches");
    let restore = PlaylistQueueRestore::from_entries(
        vec![
            RestoredPlaylistEntry::Single(restored_url_item(10, "single-before", None)),
            RestoredPlaylistEntry::Compound(restored_group),
            RestoredPlaylistEntry::Single(restored_url_item(30, "single-after", None)),
        ],
        next_item_id(31),
        next_group_id(8),
        Some(item_id(21)),
    );
    let shuffle = ShuffleTraversalSnapshot::new(
        vec![item_id(10), item_id(20), item_id(21)],
        Some(ShuffleHistoryCursor::from_index(2)),
        vec![PlaylistEntryId::Single(item_id(30))],
    );
    PlaylistQueue::restore_with_shuffle(restore, shuffle).expect("mixed v2 queue is valid")
}

fn serialized_mixed_state() -> Vec<u8> {
    let queue = mixed_v2_queue();
    serialize_state(PlaylistStateSnapshot::new(&queue, RepeatMode::RepeatQueue))
        .expect("mixed v2 state serializes")
}

fn inspect_loaded(path: &Path) -> crate::LoadedPlaylistState {
    match PlaylistStateStore::new(path).inspect_state() {
        InspectionOutcome::Loaded(state) => state,
        other => panic!("expected loaded state, got {other:?}"),
    }
}

fn inspect_corrupt_cause(path: &Path, value: &Value) -> CorruptStateCause {
    fs::write(
        path,
        serde_json::to_vec(value).expect("fixture JSON serializes"),
    )
    .expect("fixture file writes");
    match PlaylistStateStore::new(path).inspect_state() {
        InspectionOutcome::CorruptNeedsQuarantine { cause, .. } => cause,
        other => panic!("expected corrupt state, got {other:?}"),
    }
}

#[test]
fn v2_roundtrip_preserves_top_level_order_current_shuffle_allocators_and_payloads() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let encoded = serialized_mixed_state();
    fs::write(&state_path, &encoded).expect("v2 fixture writes");

    let encoded_text = std::str::from_utf8(&encoded).expect("state JSON is UTF-8");
    assert!(encoded_text.contains(ACKNOWLEDGED_SECRET_URL));
    let encoded_json: Value = serde_json::from_slice(&encoded).expect("state JSON parses");
    let encoded_service_bytes = encoded_json["entries"][1]["parts"][0]["item"]
        ["durable_payload"]["reopen_locator"]["payload_bytes"]
        .as_array()
        .expect("service payload is an exact byte array")
        .iter()
        .map(|value| value.as_u64().expect("payload unit is u8") as u8)
        .collect::<Vec<_>>();
    assert_eq!(encoded_service_bytes, SERVICE_CHILD_SECRET);

    let loaded = inspect_loaded(&state_path);
    let (queue, repeat_mode) = loaded.into_parts();
    assert_eq!(repeat_mode, RepeatMode::RepeatQueue);
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        vec![
            PlaylistEntryId::Single(item_id(10)),
            PlaylistEntryId::Compound(group_id(7)),
            PlaylistEntryId::Single(item_id(30)),
        ]
    );
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        vec![item_id(10), item_id(20), item_id(21), item_id(30)]
    );
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(item_id(21))
    );
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        31
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        8
    );
    let shuffle = queue
        .shuffle_traversal_snapshot()
        .expect("shuffle remains enabled");
    assert_eq!(shuffle.history(), &[item_id(10), item_id(20), item_id(21)]);
    assert_eq!(
        shuffle.history_cursor().map(ShuffleHistoryCursor::index),
        Some(2)
    );
    assert_eq!(shuffle.upcoming(), &[PlaylistEntryId::Single(item_id(30))]);

    let restored_payload = queue
        .item(item_id(20))
        .and_then(|item| item.durable_payload())
        .expect("service child durable payload survives");
    assert_eq!(restored_payload, &service_child_payload());
    let safe_debug = format!("{restored_payload:?}");
    assert!(!safe_debug.contains("password"));
    assert!(!safe_debug.contains("token=secret"));
    assert!(!safe_debug.contains("stable-child-secret-identity"));
}

#[test]
fn v1_fixture_migrates_to_single_entries_and_v2_writer() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let v1_fixture = json!({
        "schema_version": 1,
        "next_item_id": 12,
        "items": [
            {
                "item_id": 10,
                "locator": {
                    "kind": "url",
                    "reopenable_url": ACKNOWLEDGED_SECRET_URL
                },
                "local_fingerprint": null,
                "cached_metadata": {
                    "fallback_display_name": "legacy",
                    "media_kind": "unknown",
                    "duration": null,
                    "title": null,
                    "artists": [],
                    "album": null,
                    "disc_number": null,
                    "track_number": null,
                    "season_number": null,
                    "episode_number": null
                }
            }
        ],
        "current_item_id": 10,
        "repeat_mode": "repeat_one",
        "shuffle_enabled": true,
        "shuffle_history": [10],
        "shuffle_history_cursor": 0,
        "shuffle_upcoming": []
    });
    fs::write(
        &state_path,
        serde_json::to_vec(&v1_fixture).expect("v1 fixture serializes"),
    )
    .expect("v1 fixture writes");

    let loaded = inspect_loaded(&state_path);
    let (queue, repeat_mode) = loaded.into_parts();
    assert_eq!(repeat_mode, RepeatMode::RepeatOne);
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        vec![PlaylistEntryId::Single(item_id(10))]
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        1
    );
    assert!(
        queue
            .item(item_id(10))
            .expect("legacy item exists")
            .durable_payload()
            .is_none()
    );

    let migrated = serialize_state(PlaylistStateSnapshot::new(&queue, repeat_mode))
        .expect("migrated queue serializes");
    let migrated_json: Value = serde_json::from_slice(&migrated).expect("migrated v2 JSON parses");
    assert_eq!(migrated_json["schema_version"], json!(2));
    assert_eq!(migrated_json["entries"][0]["kind"], json!("single"));
    assert_eq!(migrated_json["next_compound_group_id"], json!(1));
}

#[test]
fn v2_rejects_corrupt_membership_ordinals_allocators_and_shuffle_id_classes() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let baseline: Value =
        serde_json::from_slice(&serialized_mixed_state()).expect("baseline JSON parses");

    let mut cases = Vec::new();
    let mut foreign_membership = baseline.clone();
    foreign_membership["entries"][1]["parts"][0]["membership"]["group_id"] = json!(99);
    cases.push((foreign_membership, CorruptStateCause::InvalidQueueState));
    let mut wrong_ordinal = baseline.clone();
    wrong_ordinal["entries"][1]["parts"][1]["membership"]["ordinal"] = json!(1);
    cases.push((wrong_ordinal, CorruptStateCause::InvalidQueueState));
    let mut invalid_group_allocator = baseline.clone();
    invalid_group_allocator["next_compound_group_id"] = json!(7);
    cases.push((
        invalid_group_allocator,
        CorruptStateCause::InvalidQueueState,
    ));
    let mut foreign_current_part = baseline.clone();
    foreign_current_part["current_item_id"] = json!(999);
    cases.push((foreign_current_part, CorruptStateCause::InvalidQueueState));
    let mut foreign_history_part = baseline.clone();
    foreign_history_part["shuffle_history"][1] = json!(999);
    cases.push((
        foreign_history_part,
        CorruptStateCause::InvalidShuffleTraversal,
    ));
    let mut subordinate_upcoming = baseline.clone();
    subordinate_upcoming["shuffle_upcoming"][0] = json!({"kind": "single", "item_id": 20});
    cases.push((
        subordinate_upcoming,
        CorruptStateCause::InvalidShuffleTraversal,
    ));
    let mut foreign_upcoming_group = baseline.clone();
    foreign_upcoming_group["shuffle_upcoming"][0] = json!({"kind": "compound", "group_id": 999});
    cases.push((
        foreign_upcoming_group,
        CorruptStateCause::InvalidShuffleTraversal,
    ));

    for (case, expected_cause) in cases {
        assert_eq!(inspect_corrupt_cause(&state_path, &case), expected_cause);
    }
}

#[test]
fn v2_rejects_invalid_span_and_bounded_payloads_without_secret_diagnostics() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let baseline: Value =
        serde_json::from_slice(&serialized_mixed_state()).expect("baseline JSON parses");
    let payload_path = &baseline["entries"][1]["parts"][0]["item"]["durable_payload"];
    assert_eq!(
        payload_path["reopen_locator"]["material_kind"],
        json!("extractor_identity")
    );

    let mut invalid_span = baseline.clone();
    invalid_span["entries"][1]["parts"][0]["item"]["durable_payload"]["playback_span"]["end_exclusive"] =
        invalid_span["entries"][1]["parts"][0]["item"]["durable_payload"]["playback_span"]["start"]
            .clone();
    assert_eq!(
        inspect_corrupt_cause(&state_path, &invalid_span),
        CorruptStateCause::InvalidDomainValue
    );

    let mut oversized_owner = baseline.clone();
    oversized_owner["entries"][1]["parts"][0]["item"]["durable_payload"]["reopen_locator"]["service_owner"] =
        json!("x".repeat(129));
    assert_eq!(
        inspect_corrupt_cause(&state_path, &oversized_owner),
        CorruptStateCause::ResourceLimitExceeded
    );

    let mut too_many_hints = baseline.clone();
    let hint = too_many_hints["entries"][1]["parts"][0]["item"]["durable_payload"]
        ["ancillary_track_hints"][0]
        .clone();
    too_many_hints["entries"][1]["parts"][0]["item"]["durable_payload"]["ancillary_track_hints"] =
        Value::Array(vec![hint; 33]);
    assert_eq!(
        inspect_corrupt_cause(&state_path, &too_many_hints),
        CorruptStateCause::ResourceLimitExceeded
    );

    let outcome = PlaylistStateStore::new(&state_path).inspect_state();
    let safe_diagnostic = format!("{outcome:?}");
    assert!(!safe_diagnostic.contains("password"));
    assert!(!safe_diagnostic.contains("token=secret"));
    assert!(!safe_diagnostic.contains("stable-child-secret-identity"));
}

#[test]
fn transient_request_material_is_structurally_unrepresentable_in_v2_dto() {
    let temp_dir = TempDir::new().expect("tempdir");
    let state_path = temp_dir.path().join("playlist-state.json");
    let baseline: Value =
        serde_json::from_slice(&serialized_mixed_state()).expect("baseline JSON parses");

    for forbidden_kind in [
        "format_url",
        "manifest_url",
        "fragment_url",
        "key_url",
        "signed_endpoint",
        "headers",
        "cookies",
        "authorization_or_session",
    ] {
        let mut case = baseline.clone();
        case["entries"][1]["parts"][0]["item"]["durable_payload"]["reopen_locator"]["material_kind"] =
            json!(forbidden_kind);
        assert_eq!(
            inspect_corrupt_cause(&state_path, &case),
            CorruptStateCause::InvalidV2Payload
        );
    }

    for forbidden_field in [
        "format_url",
        "manifest_url",
        "fragment_url",
        "key_url",
        "signed_endpoint",
        "headers",
        "cookies",
        "authorization",
    ] {
        let mut case = baseline.clone();
        case["entries"][1]["parts"][0]["item"]["durable_payload"]["reopen_locator"]
            .as_object_mut()
            .expect("service locator is an object")
            .insert(forbidden_field.to_owned(), json!("must-never-persist"));
        assert_eq!(
            inspect_corrupt_cause(&state_path, &case),
            CorruptStateCause::InvalidV2Payload
        );
    }
}

#[test]
fn immutable_snapshot_captures_item_and_group_allocators_from_one_queue_state() {
    let mut queue = PlaylistQueue::new();
    let first_group = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Url(secret_url(ACKNOWLEDGED_SECRET_URL)),
        metadata("first-group"),
        vec![
            PlaylistItemDraft::url(
                secret_url("https://example.invalid/first-a"),
                metadata("first-a"),
            ),
            PlaylistItemDraft::url(
                secret_url("https://example.invalid/first-b"),
                metadata("first-b"),
            ),
        ],
    )
    .expect("first group is non-empty");
    queue
        .append_entries(vec![playlist_core::PlaylistEntryDraft::Compound(
            first_group,
        )])
        .expect("first group appends");
    let captured = ImmutableSaveSnapshot::capture(
        SaveRevision::FIRST,
        PlaylistStateSnapshot::new(&queue, RepeatMode::StopAtEnd),
    )
    .expect("snapshot captures both allocator owners");

    assert!(matches!(queue.clear(), ClearQueueOutcome::Cleared { .. }));
    let second_group = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Url(secret_url(ACKNOWLEDGED_SECRET_URL)),
        metadata("second-group"),
        vec![PlaylistItemDraft::url(
            secret_url("https://example.invalid/second"),
            metadata("second"),
        )],
    )
    .expect("second group is non-empty");
    queue
        .append_entries(vec![playlist_core::PlaylistEntryDraft::Compound(
            second_group,
        )])
        .expect("second group appends");

    let captured_json: Value =
        serde_json::from_slice(&captured.serialize_json().expect("captured JSON serializes"))
            .expect("captured JSON parses");
    assert_eq!(captured_json["next_item_id"], json!(3));
    assert_eq!(captured_json["next_compound_group_id"], json!(2));
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        4
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        3
    );
}
