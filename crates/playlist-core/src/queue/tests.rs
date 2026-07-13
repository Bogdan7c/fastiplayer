use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use crate::{
    CachedPlaylistMetadata, ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath,
    LocalLocator, LocalSourceFingerprint, NextPlaylistItemId, PlaylistItemDraft, PlaylistItemId,
    PlaylistLocator, PlaylistMediaKind, RestoredPlaylistItem, SecretUrlLocator,
};

use super::*;

/// Строит deterministic local draft без filesystem I/O.
fn local_draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(format!("/media/{name}"))),
        Some(LocalSourceFingerprint::new(
            name.len() as u64,
            UNIX_EPOCH + Duration::from_secs(name.len() as u64),
        )),
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
    )
}

/// Строит deterministic URL draft с exact reopen identity.
fn url_draft(raw_url: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::url(
        SecretUrlLocator::from_reopenable_url(raw_url).expect("non-empty test URL"),
        CachedPlaylistMetadata::new("remote media", PlaylistMediaKind::Unknown),
    )
}

/// Короткий persistence adapter для читаемых fixed IDs в tests.
fn item_id(value: u64) -> PlaylistItemId {
    PlaylistItemId::from_persistence_value(value).expect("non-zero test ID")
}

/// Извлекает IDs успешного append без bool/Option ambiguity.
fn appended_ids(outcome: AddItemsOutcome) -> Vec<PlaylistItemId> {
    match outcome {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("test expected committed items"),
    }
}

#[test]
fn first_id_empty_one_and_capacity_boundary_are_exact() {
    // Новая queue пуста, а allocator ещё указывает на первый ID = 1.
    let mut queue = PlaylistQueue::new();
    assert!(queue.is_empty());
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        1
    );

    // Один atomic batch доводит размер ровно до 49_999.
    let initial_drafts = (0..49_999)
        .map(|index| local_draft(&format!("item-{index}")))
        .collect();
    let initial_ids = appended_ids(queue.append_batch(initial_drafts).expect("below cap"));
    assert_eq!(initial_ids.first(), Some(&item_id(1)));
    assert_eq!(initial_ids.last(), Some(&item_id(49_999)));
    assert_eq!(queue.len(), 49_999);

    // Последняя разрешённая строка получает ID 50_000.
    let final_id = appended_ids(queue.append_one(local_draft("last")).expect("at cap"));
    assert_eq!(final_id, vec![item_id(50_000)]);
    assert_eq!(queue.len(), MAX_PLAYLIST_ITEMS);

    // Следующий append typed-reject-ится без size/watermark mutation.
    let watermark_before_rejection = queue.next_item_id_snapshot();
    assert!(matches!(
        queue.append_one(local_draft("overflow")),
        Err(AddItemsError::CapacityExceeded { .. })
    ));
    assert_eq!(queue.len(), MAX_PLAYLIST_ITEMS);
    assert_eq!(queue.next_item_id_snapshot(), watermark_before_rejection);
}

#[test]
fn manual_duplicate_locators_receive_distinct_stable_ids() {
    // Domain не выполняет discovery dedup и принимает exact duplicate locator.
    let mut queue = PlaylistQueue::new();
    let duplicate = url_draft("https://example.com/media.mp4?token=secret");
    let ids = appended_ids(
        queue
            .append_batch(vec![duplicate.clone(), duplicate])
            .expect("manual duplicates are allowed"),
    );

    assert_eq!(ids, vec![item_id(1), item_id(2)]);
    assert_eq!(queue.items()[0].locator(), queue.items()[1].locator());
}

#[test]
fn allocation_exhaustion_rejects_whole_batch_without_partial_mutation() {
    // MAX watermark валиден для empty restore, но не имеет representable successor.
    let next_item_id =
        NextPlaylistItemId::from_persistence_value(u64::MAX).expect("non-zero watermark");
    let mut queue =
        PlaylistQueue::restore(PlaylistQueueRestore::new(Vec::new(), next_item_id, None))
            .expect("empty restore accepts high watermark");
    let revisions_before = queue.revision_snapshot();

    assert_eq!(
        queue.append_batch(vec![local_draft("a"), local_draft("b")]),
        Err(AddItemsError::ItemIdExhausted)
    );
    assert!(queue.is_empty());
    assert_eq!(queue.next_item_id_snapshot(), next_item_id);
    assert_eq!(queue.revision_snapshot(), revisions_before);
}

#[test]
fn remove_clear_and_replace_never_lower_allocator_high_watermark() {
    // IDs 1 и 2 считаются выданными после первого commit.
    let mut queue = PlaylistQueue::new();
    let ids = appended_ids(
        queue
            .append_batch(vec![local_draft("one"), local_draft("two")])
            .expect("initial append"),
    );
    assert!(matches!(
        queue.remove(ids[1]),
        RemoveItemOutcome::Removed { .. }
    ));
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        3
    );

    // Clear не возвращает allocator к 1.
    assert!(matches!(queue.clear(), ClearQueueOutcome::Cleared { .. }));
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        3
    );

    // Следующий append получает 3, replacement получает 4.
    let third = appended_ids(
        queue
            .append_one(local_draft("three"))
            .expect("append after clear"),
    );
    assert_eq!(third, vec![item_id(3)]);
    let replacement = queue
        .replace_all(vec![local_draft("replacement")])
        .expect("replacement");
    let ReplaceQueueOutcome::Replaced {
        allocated_item_ids, ..
    } = replacement
    else {
        panic!("expected non-empty replacement");
    };
    assert_eq!(allocated_item_ids.as_slice(), &[item_id(4)]);
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        5
    );
}

#[test]
fn validated_restore_then_append_does_not_reuse_removed_id() {
    // Snapshot моделирует queue после удаления прежнего highest ID 2.
    let restored = RestoredPlaylistItem::new(item_id(1), local_draft("kept"));
    let mut queue = PlaylistQueue::restore(PlaylistQueueRestore::new(
        vec![restored],
        NextPlaylistItemId::from_persistence_value(3).expect("watermark"),
        Some(item_id(1)),
    ))
    .expect("valid restore");

    let appended = appended_ids(queue.append_one(local_draft("new")).expect("append"));
    assert_eq!(appended, vec![item_id(3)]);
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(item_id(1))
    );
}

#[test]
fn invalid_restore_watermark_duplicate_and_current_are_rejected() {
    // Watermark обязан быть строго больше maximum restored ID.
    let invalid_watermark = PlaylistQueue::restore(PlaylistQueueRestore::new(
        vec![RestoredPlaylistItem::new(item_id(2), local_draft("two"))],
        NextPlaylistItemId::from_persistence_value(2).expect("non-zero"),
        None,
    ));
    assert!(matches!(
        invalid_watermark,
        Err(QueueRestoreError::InvalidAllocator(_))
    ));

    // Duplicate stable identity не превращается в две строки.
    let duplicate_id = PlaylistQueue::restore(PlaylistQueueRestore::new(
        vec![
            RestoredPlaylistItem::new(item_id(1), local_draft("a")),
            RestoredPlaylistItem::new(item_id(1), local_draft("b")),
        ],
        NextPlaylistItemId::from_persistence_value(2).expect("watermark"),
        None,
    ));
    assert!(matches!(
        duplicate_id,
        Err(QueueRestoreError::DuplicateItemId { .. })
    ));

    // Persisted current не может ссылаться на отсутствующий row.
    let missing_current = PlaylistQueue::restore(PlaylistQueueRestore::new(
        vec![RestoredPlaylistItem::new(item_id(1), local_draft("a"))],
        NextPlaylistItemId::from_persistence_value(2).expect("watermark"),
        Some(item_id(2)),
    ));
    assert!(matches!(
        missing_current,
        Err(QueueRestoreError::CurrentItemNotCommitted { .. })
    ));
}

#[test]
fn reservation_preflight_failure_installs_no_lock() {
    // Сохраняем revision до ordinary structural mutation, делая его stale.
    let mut queue = PlaylistQueue::new();
    let stale_revision = queue.revision_snapshot();
    let committed_id = appended_ids(queue.append_one(local_draft("one")).expect("append"))[0];

    let result = queue.prepare_reserved_mutation(
        stale_revision,
        ReservedQueueMutation::select_committed(committed_id),
    );
    assert!(matches!(
        result,
        Err(PrepareReservedMutationError::RevisionMismatch { .. })
    ));

    // Успешный следующий append доказывает отсутствие leaked lock.
    assert!(queue.append_one(local_draft("two")).is_ok());
}

#[test]
fn reservation_keeps_future_ids_private_and_blocks_conflicting_mutators() {
    // Replacement reserve должен оставить allocator/revisions неизменными.
    let mut queue = PlaylistQueue::new();
    let existing_id = appended_ids(queue.append_one(local_draft("old")).expect("append"))[0];
    queue
        .set_traversal_current(existing_id)
        .expect("set current before reserve");
    let watermark_before = queue.next_item_id_snapshot();
    let revisions_before = queue.revision_snapshot();
    let token = queue
        .prepare_reserved_mutation(
            revisions_before,
            ReservedQueueMutation::replace_with_current(
                vec![local_draft("before")],
                local_draft("target"),
                vec![local_draft("after")],
            ),
        )
        .expect("reservation");

    assert_eq!(format!("{token:?}"), "PreparedQueueMutationToken(<opaque>)");
    assert_eq!(queue.next_item_id_snapshot(), watermark_before);
    assert_eq!(queue.revision_snapshot(), revisions_before);
    assert_eq!(
        queue.append_one(local_draft("blocked")),
        Err(AddItemsError::InstallCommitLinearizing)
    );
    assert_eq!(
        queue.replace_all(vec![local_draft("blocked")]),
        Err(ReplaceQueueError::InstallCommitLinearizing)
    );
    assert_eq!(
        queue.remove(existing_id),
        RemoveItemOutcome::InstallCommitLinearizing
    );
    assert_eq!(
        queue.move_item(existing_id, MoveItemIntent::ToFront),
        MoveItemOutcome::InstallCommitLinearizing
    );
    assert_eq!(queue.clear(), ClearQueueOutcome::InstallCommitLinearizing);
    assert_eq!(
        queue.set_traversal_current(existing_id),
        Err(TraversalCurrentMutationError::InstallCommitLinearizing)
    );

    // Explicit abort не сжигает proposed range и не dirty-ит revisions.
    queue.abort_reserved(token);
    assert_eq!(queue.next_item_id_snapshot(), watermark_before);
    assert_eq!(queue.revision_snapshot(), revisions_before);
    let next_id = appended_ids(
        queue
            .append_one(local_draft("after-abort"))
            .expect("append"),
    );
    assert_eq!(next_id, vec![item_id(2)]);
}

#[test]
fn exact_token_commit_publishes_replacement_and_current_in_one_step() {
    // Proposed IDs не наблюдаются до infallible business commit.
    let mut queue = PlaylistQueue::new();
    let revisions_before = queue.revision_snapshot();
    let token = queue
        .prepare_reserved_mutation(
            revisions_before,
            ReservedQueueMutation::replace_with_current(
                vec![local_draft("before")],
                local_draft("target"),
                vec![local_draft("after")],
            ),
        )
        .expect("reservation");
    assert!(queue.is_empty());
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        1
    );

    let commit = queue.commit_reserved(token);
    assert_eq!(
        commit.allocated_item_ids().as_slice(),
        &[item_id(1), item_id(2), item_id(3)]
    );
    assert_eq!(commit.traversal_current().item_id(), item_id(2));
    assert_eq!(queue.len(), 3);
    assert_eq!(queue.traversal_current(), Some(commit.traversal_current()));
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        4
    );
}

#[test]
fn token_from_another_queue_is_an_invariant_diagnostic() {
    // Safe API не позволяет forge token fields, но cross-owner misuse диагностируется panic.
    let mut first_queue = PlaylistQueue::new();
    let first_id = appended_ids(
        first_queue
            .append_one(local_draft("first"))
            .expect("first append"),
    )[0];
    let token = first_queue
        .prepare_reserved_mutation(
            first_queue.revision_snapshot(),
            ReservedQueueMutation::select_committed(first_id),
        )
        .expect("first reservation");
    let mut second_queue = PlaylistQueue::new();

    let invariant_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        second_queue.commit_reserved(token)
    }));
    assert!(invariant_result.is_err());
}

#[test]
fn move_by_id_handles_edges_anchors_and_no_op() {
    // Canonical IDs позволяют move без public positional numeric API.
    let mut queue = PlaylistQueue::new();
    let ids = appended_ids(
        queue
            .append_batch(vec![local_draft("a"), local_draft("b"), local_draft("c")])
            .expect("append"),
    );
    assert_eq!(
        queue.move_item(ids[0], MoveItemIntent::ToFront),
        MoveItemOutcome::AlreadyInPlace { item_id: ids[0] }
    );
    assert_eq!(
        queue.move_item(ids[2], MoveItemIntent::Before(ids[0])),
        MoveItemOutcome::Moved { item_id: ids[2] }
    );
    assert_eq!(
        queue
            .items()
            .iter()
            .map(|item| item.item_id())
            .collect::<Vec<_>>(),
        vec![ids[2], ids[0], ids[1]]
    );
    assert_eq!(
        queue.move_item(ids[2], MoveItemIntent::After(ids[1])),
        MoveItemOutcome::Moved { item_id: ids[2] }
    );
    assert_eq!(
        queue
            .items()
            .iter()
            .map(|item| item.item_id())
            .collect::<Vec<_>>(),
        vec![ids[0], ids[1], ids[2]]
    );
    assert!(matches!(
        queue.move_item(ids[0], MoveItemIntent::Before(item_id(999))),
        MoveItemOutcome::AnchorNotFound { .. }
    ));
}

#[test]
fn removing_current_clears_cursor_without_assigning_successor() {
    // TraversalCurrentItemId валидируется отдельно от active player identity.
    let mut queue = PlaylistQueue::new();
    let ids = appended_ids(
        queue
            .append_batch(vec![local_draft("a"), local_draft("b")])
            .expect("append"),
    );
    queue
        .set_traversal_current(ids[0])
        .expect("committed current");

    assert!(matches!(
        queue.remove(ids[0]),
        RemoveItemOutcome::Removed {
            traversal_current_effect: TraversalCurrentEffect::Cleared,
            ..
        }
    ));
    assert_eq!(queue.traversal_current(), None);
    assert!(matches!(
        queue.validate_traversal_current(ids[0]),
        Err(TraversalCurrentValidationError::ItemNotCommitted { .. })
    ));
    assert!(queue.item(ids[1]).is_some());
}

#[test]
fn metadata_batch_distinguishes_all_outcomes_and_preserves_other_state() {
    // Две строки позволяют проверить applied/no-change/source mismatch/not-found.
    let mut queue = PlaylistQueue::new();
    let ids = appended_ids(
        queue
            .append_batch(vec![local_draft("a"), local_draft("b")])
            .expect("append"),
    );
    queue.set_traversal_current(ids[0]).expect("set current");
    let order_before: Vec<_> = queue.items().iter().map(|item| item.item_id()).collect();
    let revisions_before = queue.revision_snapshot();
    let first_locator = queue.items()[0].locator().clone();
    let first_fingerprint = queue.items()[0].local_fingerprint();
    let second_locator = queue.items()[1].locator().clone();
    let unchanged_metadata = queue.items()[1].cached_metadata().clone();
    let changed_metadata = CachedPlaylistMetadata::new("updated", PlaylistMediaKind::Audio)
        .with_title(Some("Verified title".to_owned()));
    let patches = vec![
        PlaylistMetadataPatch::new(
            ids[0],
            first_locator,
            first_fingerprint,
            changed_metadata.clone(),
        ),
        PlaylistMetadataPatch::new(
            ids[1],
            second_locator.clone(),
            queue.items()[1].local_fingerprint(),
            unchanged_metadata.clone(),
        ),
        PlaylistMetadataPatch::new(
            ids[1],
            PlaylistLocator::Local(LocalLocator::Native(PathBuf::from("/other/source"))),
            queue.items()[1].local_fingerprint(),
            changed_metadata.clone(),
        ),
        PlaylistMetadataPatch::new(item_id(999), second_locator, None, changed_metadata.clone()),
    ];

    let outcome = queue
        .apply_metadata_patch_batch(patches)
        .expect("metadata revision available");
    assert_eq!(
        outcome.item_outcomes(),
        &[
            MetadataPatchItemOutcome::Applied { item_id: ids[0] },
            MetadataPatchItemOutcome::NoChange { item_id: ids[1] },
            MetadataPatchItemOutcome::SourceMismatch { item_id: ids[1] },
            MetadataPatchItemOutcome::NotFound {
                item_id: item_id(999)
            },
        ]
    );
    assert_eq!(outcome.applied_count(), 1);
    assert_eq!(queue.items()[0].cached_metadata(), &changed_metadata);
    assert_eq!(queue.items()[1].cached_metadata(), &unchanged_metadata);
    assert_eq!(
        queue
            .items()
            .iter()
            .map(|item| item.item_id())
            .collect::<Vec<_>>(),
        order_before
    );
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(ids[0])
    );
    assert_eq!(
        queue.revision_snapshot().structural(),
        revisions_before.structural()
    );
    assert_eq!(
        queue.revision_snapshot().traversal(),
        revisions_before.traversal()
    );
    assert_ne!(
        queue.revision_snapshot().metadata(),
        revisions_before.metadata()
    );
}

#[test]
fn metadata_patch_is_allowed_during_reservation_and_does_not_break_commit() {
    // Metadata dimension не входит в D08 allocator/structural/traversal preconditions.
    let mut queue = PlaylistQueue::new();
    let id = appended_ids(queue.append_one(local_draft("a")).expect("append"))[0];
    let locator = queue.items()[0].locator().clone();
    let fingerprint = queue.items()[0].local_fingerprint();
    let token = queue
        .prepare_reserved_mutation(
            queue.revision_snapshot(),
            ReservedQueueMutation::select_committed(id),
        )
        .expect("reservation");
    let metadata = CachedPlaylistMetadata::new("fresh", PlaylistMediaKind::Video);

    let patch_outcome = queue
        .apply_metadata_patch_batch(vec![PlaylistMetadataPatch::new(
            id,
            locator,
            fingerprint,
            metadata.clone(),
        )])
        .expect("metadata patch");
    assert!(patch_outcome.changed_metadata());
    let commit = queue.commit_reserved(token);
    assert_eq!(commit.traversal_current().item_id(), id);
    assert_eq!(
        queue.item(id).expect("same item").cached_metadata(),
        &metadata
    );
}

#[test]
fn foreign_locator_survives_committed_item_without_lossy_conversion() {
    // Domain сохраняет foreign wide units как locator identity строки.
    let foreign_path = ForeignPlatformPath::new(
        ForeignPathPlatform::Windows,
        ForeignPathEncoding::Wide(vec![0xd800, b'Z' as u16]),
    );
    let draft = PlaylistItemDraft::local(
        LocalLocator::Foreign(foreign_path.clone()),
        None,
        CachedPlaylistMetadata::new("foreign", PlaylistMediaKind::Unknown),
    );
    let mut queue = PlaylistQueue::new();
    queue.append_one(draft).expect("append foreign locator");

    assert_eq!(
        queue.items()[0].locator(),
        &PlaylistLocator::Local(LocalLocator::Foreign(foreign_path))
    );
}

#[test]
fn item_id_is_stable_across_move_and_metadata_patch() {
    // Non-removal mutations не пересоздают stable row identity.
    let mut queue = PlaylistQueue::new();
    let ids = appended_ids(
        queue
            .append_batch(vec![local_draft("a"), local_draft("b")])
            .expect("append"),
    );
    queue.move_item(ids[0], MoveItemIntent::ToBack);
    let moved_item = queue.item(ids[0]).expect("same ID after move");
    let patch = PlaylistMetadataPatch::new(
        ids[0],
        moved_item.locator().clone(),
        moved_item.local_fingerprint(),
        CachedPlaylistMetadata::new("changed", PlaylistMediaKind::Video),
    );
    queue
        .apply_metadata_patch_batch(vec![patch])
        .expect("metadata patch");

    assert_eq!(
        queue.item(ids[0]).expect("same ID after patch").item_id(),
        ids[0]
    );
}
