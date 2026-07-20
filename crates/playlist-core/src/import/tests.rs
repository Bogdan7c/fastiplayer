use std::num::NonZeroU32;
use std::path::PathBuf;

use media_core::{MediaDuration, MediaTime};

use super::*;
use crate::{
    AddPlaylistEntriesOutcome, CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION, LocalLocator,
    MAX_PLAYLIST_ANCILLARY_TRACK_HINTS, PlaylistAncillaryTrackOrigin,
    PlaylistAncillaryTrackSelectionKind, PlaylistImportSourceKind, PlaylistItem, PlaylistMediaKind,
    PlaylistQueue, ServiceReopenMaterialKind,
};

#[test]
fn single_draft_keeps_payloads_without_allocating_identity() {
    let draft = single("album.flac", PlaylistImportAvailability::Available);

    assert_eq!(draft.availability(), PlaylistImportAvailability::Available);
    assert_eq!(
        draft
            .playback_span()
            .and_then(PlaylistPlaybackSpan::duration),
        Some(MediaDuration::from_secs(10))
    );
    assert_eq!(
        draft.cached_metadata().fallback_display_name(),
        "album.flac"
    );
    assert!(draft.ancillary_track_hints().is_empty());
    assert!(draft.reopen_locator().expose_local_for_reopen().is_some());
}

#[test]
fn one_part_compound_stays_compound_and_empty_is_rejected() {
    let root = local_locator("album.cue");
    let part = single("album.flac", PlaylistImportAvailability::Available);
    let group = PlaylistCompoundImportDraft::new(
        root.clone(),
        metadata("album"),
        provenance(root.clone()),
        vec![part],
    )
    .expect("one retained part remains compound");
    let entry = PlaylistImportEntryDraft::Compound(group);

    assert!(entry.is_compound());
    assert_eq!(entry.retained_item_count(), 1);
    assert_eq!(
        PlaylistCompoundImportDraft::new(
            root.clone(),
            metadata("empty"),
            provenance(root),
            Vec::new(),
        ),
        Err(PlaylistCompoundImportDraftError::EmptyCompound)
    );
}

#[test]
fn compound_part_bound_rejects_oversized_group_without_truncation() {
    let root = local_locator("oversized.cue");
    let part = single("oversized.flac", PlaylistImportAvailability::Available);
    let error = PlaylistCompoundImportDraft::new(
        root.clone(),
        metadata("oversized"),
        provenance(root),
        vec![part; MAX_PLAYLIST_IMPORT_COMPOUND_PARTS + 1],
    )
    .expect_err("oversized compound must be rejected");

    assert_eq!(
        error,
        PlaylistCompoundImportDraftError::PartLimitExceeded {
            provided: MAX_PLAYLIST_IMPORT_COMPOUND_PARTS + 1,
            maximum: MAX_PLAYLIST_IMPORT_COMPOUND_PARTS,
        }
    );
}

#[test]
fn unavailable_child_requires_and_preserves_stable_durable_identity() {
    let stable_payload = b"https://example.invalid/stable-child".to_vec();
    let reopen_locator = DurableReopenLocator::from_service_payload(
        "yt-dlp",
        CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
        ServiceReopenMaterialKind::StableOriginalIdentity,
        stable_payload.clone(),
    )
    .expect("stable child identity");
    let draft = PlaylistSingleImportDraft::new(
        reopen_locator.clone(),
        metadata("Unavailable child"),
        None,
        Vec::new(),
        PlaylistImportProvenance::new(
            reopen_locator,
            PlaylistImportSourceKind::Service,
            NonZeroU32::new(3),
        ),
        PlaylistImportAvailability::Unavailable,
    )
    .expect("unavailable stable child");

    assert_eq!(
        draft.availability(),
        PlaylistImportAvailability::Unavailable
    );
    assert_eq!(
        draft
            .reopen_locator()
            .expose_service_payload_for_reopen()
            .expect("service payload")
            .expose_payload_for_reopen(),
        stable_payload
    );
}

#[test]
fn ancillary_count_bound_is_enforced_without_truncation() {
    let hint = PlaylistAncillaryTrackHint::new(
        "subtitle",
        Some("en".to_owned()),
        None,
        PlaylistAncillaryTrackSelectionKind::Manual,
        PlaylistAncillaryTrackOrigin::Embedded,
        None,
    )
    .expect("bounded hint");
    let reopen_locator = local_locator("movie.mkv");
    let error = PlaylistSingleImportDraft::new(
        reopen_locator.clone(),
        metadata("movie"),
        None,
        vec![hint; MAX_PLAYLIST_ANCILLARY_TRACK_HINTS + 1],
        provenance(reopen_locator),
        PlaylistImportAvailability::Available,
    )
    .expect_err("oversized ancillary list");

    assert_eq!(
        error,
        PlaylistPayloadBuildError::AncillaryTrackLimitExceeded {
            provided: MAX_PLAYLIST_ANCILLARY_TRACK_HINTS + 1,
            maximum: MAX_PLAYLIST_ANCILLARY_TRACK_HINTS,
        }
    );
}

#[test]
fn import_draft_debug_never_exposes_service_payload() {
    let secret = b"https://secret.invalid/private?cookie=token".to_vec();
    let reopen_locator = DurableReopenLocator::from_service_payload(
        "yt-dlp",
        CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
        ServiceReopenMaterialKind::StableExtractorIdentity,
        secret,
    )
    .expect("stable service identity");
    let draft = PlaylistSingleImportDraft::new(
        reopen_locator.clone(),
        metadata("safe title"),
        None,
        Vec::new(),
        PlaylistImportProvenance::new(reopen_locator, PlaylistImportSourceKind::Service, None),
        PlaylistImportAvailability::Available,
    )
    .expect("bounded draft");
    let debug = format!("{draft:?}");

    assert!(!debug.contains("secret.invalid"));
    assert!(!debug.contains("cookie=token"));
}

#[test]
fn materialization_stays_idless_until_atomic_queue_commit() {
    let import = PlaylistImportEntryDraft::from(single(
        "materialized.flac",
        PlaylistImportAvailability::Available,
    ));
    let queue_draft = import
        .into_queue_draft()
        .expect("operational local locator");
    let mut queue = PlaylistQueue::new();

    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        1
    );
    let allocated = match queue.append_entries(vec![queue_draft]).expect("commit") {
        AddPlaylistEntriesOutcome::Added(allocated) => allocated,
        AddPlaylistEntriesOutcome::NoEntriesProvided => {
            panic!("focused import must commit one entry")
        }
    };
    assert_eq!(allocated.retained_item_count(), 1);
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        2
    );
    assert!(
        queue
            .iter_playable_items()
            .next()
            .and_then(PlaylistItem::durable_payload)
            .is_some()
    );
}

#[test]
fn service_child_uses_reopenable_root_without_exposing_service_payload() {
    let service_locator = DurableReopenLocator::from_service_payload(
        "yt-dlp",
        CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
        ServiceReopenMaterialKind::StableExtractorIdentity,
        b"stable-child-42".to_vec(),
    )
    .expect("service child");
    let root_url =
        SecretUrlLocator::from_reopenable_url("https://example.test/collection").expect("root");
    let import = PlaylistSingleImportDraft::new(
        service_locator,
        metadata("child"),
        None,
        Vec::new(),
        PlaylistImportProvenance::new(
            DurableReopenLocator::url(root_url.clone()),
            PlaylistImportSourceKind::Service,
            NonZeroU32::new(1),
        ),
        PlaylistImportAvailability::Available,
    )
    .expect("service import");

    let queue_draft = PlaylistImportEntryDraft::from(import)
        .into_queue_draft()
        .expect("URL root provides operational identity");
    let PlaylistEntryDraft::Single(queue_draft) = queue_draft else {
        panic!("single import stays single")
    };
    assert_eq!(queue_draft.locator().as_secret_url(), Some(&root_url));
    assert!(matches!(
        queue_draft
            .durable_payload()
            .map(PlaylistSingleDurablePayload::reopen_locator),
        Some(DurableReopenLocator::ServicePayload(_))
    ));
}

#[test]
fn service_child_without_local_or_url_root_fails_before_queue_mutation() {
    let service_locator = DurableReopenLocator::from_service_payload(
        "yt-dlp",
        CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
        ServiceReopenMaterialKind::StableExtractorIdentity,
        b"stable-child".to_vec(),
    )
    .expect("service child");
    let import = PlaylistSingleImportDraft::new(
        service_locator.clone(),
        metadata("child"),
        None,
        Vec::new(),
        PlaylistImportProvenance::new(
            service_locator,
            PlaylistImportSourceKind::Service,
            NonZeroU32::new(1),
        ),
        PlaylistImportAvailability::Available,
    )
    .expect("neutral draft may preserve service-only identity");

    assert_eq!(
        PlaylistImportEntryDraft::from(import).into_queue_draft(),
        Err(PlaylistImportMaterializationError::ServiceLocatorWithoutOperationalRoot)
    );
}

fn metadata(name: &str) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(name, PlaylistMediaKind::Audio)
}

fn local_locator(path: &str) -> DurableReopenLocator {
    DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(path)))
}

fn provenance(root: DurableReopenLocator) -> PlaylistImportProvenance {
    PlaylistImportProvenance::new(root, PlaylistImportSourceKind::Cue, NonZeroU32::new(1))
}

fn single(path: &str, availability: PlaylistImportAvailability) -> PlaylistSingleImportDraft {
    let reopen_locator = local_locator(path);
    PlaylistSingleImportDraft::new(
        reopen_locator.clone(),
        metadata(path),
        Some(
            PlaylistPlaybackSpan::from_start_and_duration(
                MediaTime::from_secs(5),
                MediaDuration::from_secs(10),
            )
            .expect("bounded span"),
        ),
        Vec::new(),
        provenance(reopen_locator),
        availability,
    )
    .expect("bounded single")
}
