use std::path::PathBuf;

use crate::{
    CachedPlaylistMetadata, EmptyPlaylistCompoundDraft, MAX_PLAYLIST_ITEMS,
    NextPlaylistCompoundGroupId, PlaylistCompoundGroupDraft, PlaylistCompoundGroupIdAllocator,
    PlaylistEntryDraft, PlaylistEntryId, PlaylistItemDraft, PlaylistLocator, PlaylistMediaKind,
    PlaylistMetadataPatch, PlaylistQueue, ReservedQueueMutation, SecretUrlLocator,
};

use super::{
    AddPlaylistEntriesOutcome, PlaylistEntriesMutationError, ReplacePlaylistEntriesOutcome,
};

/// Создаёт безопасный URL locator для compact domain fixtures.
fn secret_url(path: &str) -> SecretUrlLocator {
    SecretUrlLocator::from_reopenable_url(format!("https://media.invalid/{path}"))
        .expect("test URL is non-empty")
}

/// Создаёт minimal playable draft с различимой identity.
fn item_draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::url(
        secret_url(name),
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
    )
}

/// Создаёт first-class compound draft с заданными source-order parts.
fn compound_draft(name: &str, part_count: usize) -> PlaylistEntryDraft {
    let parts = (0..part_count)
        .map(|part_index| item_draft(&format!("{name}-part-{part_index}")))
        .collect();
    let group = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Url(secret_url(&format!("{name}-root"))),
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
        parts,
    )
    .expect("fixture compound is non-empty");
    PlaylistEntryDraft::Compound(group)
}

#[test]
fn empty_compound_is_typed_issue_and_one_part_remains_compound() {
    let empty = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Url(secret_url("empty-root")),
        CachedPlaylistMetadata::new("empty", PlaylistMediaKind::Video),
        Vec::new(),
    );
    assert_eq!(empty, Err(EmptyPlaylistCompoundDraft));

    let mut queue = PlaylistQueue::new();
    let outcome = queue
        .append_entries(vec![compound_draft("one", 1)])
        .expect("one-part compound must commit");
    let allocated = match outcome {
        AddPlaylistEntriesOutcome::Added(allocated) => allocated,
        AddPlaylistEntriesOutcome::NoEntriesProvided => panic!("expected committed compound"),
    };

    assert_eq!(queue.top_level_entry_count(), 1);
    assert_eq!(queue.retained_item_count(), 1);
    assert!(matches!(
        allocated.iter_entry_ids().next(),
        Some(PlaylistEntryId::Compound(_))
    ));
    assert!(
        queue
            .iter_top_level_entries()
            .next()
            .and_then(crate::PlaylistEntry::as_compound)
            .is_some()
    );
}

#[test]
fn mixed_top_level_order_flattens_parts_with_stable_ids_and_ordinals() {
    let mut queue = PlaylistQueue::new();
    let outcome = queue
        .append_entries(vec![
            PlaylistEntryDraft::Single(item_draft("single-a")),
            compound_draft("group", 3),
            PlaylistEntryDraft::Single(item_draft("single-b")),
        ])
        .expect("mixed batch must commit");
    let allocated = match outcome {
        AddPlaylistEntriesOutcome::Added(allocated) => allocated,
        AddPlaylistEntriesOutcome::NoEntriesProvided => panic!("expected committed entries"),
    };

    assert_eq!(queue.top_level_entry_count(), 3);
    assert_eq!(queue.retained_item_count(), 5);
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        allocated.iter_entry_ids().collect::<Vec<_>>()
    );
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        allocated.iter_playable_item_ids().collect::<Vec<_>>()
    );

    let group = queue
        .iter_top_level_entries()
        .nth(1)
        .and_then(crate::PlaylistEntry::as_compound)
        .expect("middle entry remains compound");
    let group_id = group.group_id();
    assert_eq!(
        group
            .parts()
            .map(|part| (
                part.membership().group_id(),
                part.membership().ordinal().one_based(),
            ))
            .collect::<Vec<_>>(),
        vec![(group_id, 1), (group_id, 2), (group_id, 3)]
    );
}

#[test]
fn item_and_group_collision_failures_burn_neither_allocator() {
    let mut item_collision_queue = PlaylistQueue::new();
    item_collision_queue
        .append_one(item_draft("existing-single"))
        .expect("fixture single must commit");
    item_collision_queue.item_id_allocator = crate::PlaylistItemIdAllocator::initial();
    let item_before = item_collision_queue.next_item_id_snapshot();
    let group_before = item_collision_queue.next_compound_group_id_snapshot();

    let item_collision =
        item_collision_queue.append_entries(vec![compound_draft("item-collision", 1)]);
    assert_eq!(
        item_collision,
        Err(PlaylistEntriesMutationError::ItemIdCollision {
            item_id: crate::PlaylistItemId::from_persistence_value(1)
                .expect("fixture Item ID is non-zero")
        })
    );
    assert_eq!(item_collision_queue.next_item_id_snapshot(), item_before);
    assert_eq!(
        item_collision_queue.next_compound_group_id_snapshot(),
        group_before
    );
    assert_eq!(item_collision_queue.top_level_entry_count(), 1);

    let mut group_collision_queue = PlaylistQueue::new();
    group_collision_queue
        .append_entries(vec![compound_draft("existing-group", 1)])
        .expect("fixture group must commit");
    group_collision_queue.compound_group_id_allocator = PlaylistCompoundGroupIdAllocator::initial();
    let item_before = group_collision_queue.next_item_id_snapshot();
    let group_before = group_collision_queue.next_compound_group_id_snapshot();

    let group_collision =
        group_collision_queue.append_entries(vec![compound_draft("group-collision", 1)]);
    assert_eq!(
        group_collision,
        Err(PlaylistEntriesMutationError::CompoundGroupIdCollision {
            group_id: crate::PlaylistCompoundGroupId::from_persistence_value(1)
                .expect("fixture Group ID is non-zero")
        })
    );
    assert_eq!(group_collision_queue.next_item_id_snapshot(), item_before);
    assert_eq!(
        group_collision_queue.next_compound_group_id_snapshot(),
        group_before
    );
    assert_eq!(group_collision_queue.top_level_entry_count(), 1);
}

#[test]
fn group_allocator_exhaustion_rolls_back_item_plan_and_queue_state() {
    let mut queue = PlaylistQueue::new();
    queue.compound_group_id_allocator = PlaylistCompoundGroupIdAllocator::restore(
        NextPlaylistCompoundGroupId::from_persistence_value(u64::MAX)
            .expect("u64::MAX is a non-zero watermark"),
        &[],
    )
    .expect("watermark without restored groups is valid");
    let item_before = queue.next_item_id_snapshot();
    let group_before = queue.next_compound_group_id_snapshot();
    let revision_before = queue.revision_snapshot();

    let outcome = queue.append_entries(vec![compound_draft("exhausted", 1)]);

    assert_eq!(
        outcome,
        Err(PlaylistEntriesMutationError::CompoundGroupIdArithmeticExhausted)
    );
    assert_eq!(queue.next_item_id_snapshot(), item_before);
    assert_eq!(queue.next_compound_group_id_snapshot(), group_before);
    assert_eq!(queue.revision_snapshot(), revision_before);
    assert!(queue.is_empty());
}

#[test]
fn capped_append_stops_before_whole_group_and_rejects_following_tail() {
    let mut queue = PlaylistQueue::new();
    let initial = (0..MAX_PLAYLIST_ITEMS - 2)
        .map(|index| item_draft(&format!("initial-{index}")))
        .collect();
    queue
        .append_batch(initial)
        .expect("fixture must leave exactly two retained slots");
    let item_before = queue.next_item_id_snapshot();
    let group_before = queue.next_compound_group_id_snapshot();

    let outcome = queue
        .append_capped_entries(vec![
            PlaylistEntryDraft::Single(item_draft("accepted-single")),
            compound_draft("rejected-group", 2),
            PlaylistEntryDraft::Single(item_draft("rejected-tail")),
        ])
        .expect("capped append returns an exact prefix");
    let (allocated, rejected_entries, rejected_items) = outcome.into_parts();

    assert_eq!(allocated.top_level_entry_count(), 1);
    assert_eq!(allocated.retained_item_count(), 1);
    assert_eq!(rejected_entries, 2);
    assert_eq!(rejected_items, 3);
    assert_eq!(queue.retained_item_count(), MAX_PLAYLIST_ITEMS - 1);
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        item_before.expose_value_for_persistence() + 1
    );
    assert_eq!(queue.next_compound_group_id_snapshot(), group_before);
    assert!(
        queue
            .iter_top_level_entries()
            .all(|entry| entry.as_compound().is_none())
    );
}

#[test]
fn replace_capacity_counts_parts_and_failure_is_atomic() {
    let mut queue = PlaylistQueue::new();
    let exact_capacity = queue
        .replace_entries(vec![compound_draft("exact-capacity", MAX_PLAYLIST_ITEMS)])
        .expect("one whole group may fill retained capacity exactly");
    assert!(matches!(
        exact_capacity,
        ReplacePlaylistEntriesOutcome::Replaced { .. }
    ));
    assert_eq!(queue.top_level_entry_count(), 1);
    assert_eq!(queue.retained_item_count(), MAX_PLAYLIST_ITEMS);
    let ids_before = queue.iter_playable_ids().collect::<Vec<_>>();
    let entries_before = queue.iter_top_level_entry_ids().collect::<Vec<_>>();
    let item_before = queue.next_item_id_snapshot();
    let group_before = queue.next_compound_group_id_snapshot();
    let revision_before = queue.revision_snapshot();

    let outcome = queue.replace_entries(vec![compound_draft("too-large", MAX_PLAYLIST_ITEMS + 1)]);

    assert_eq!(
        outcome,
        Err(PlaylistEntriesMutationError::CapacityExceeded {
            current_retained_items: 0,
            requested_retained_items: MAX_PLAYLIST_ITEMS + 1,
            maximum: MAX_PLAYLIST_ITEMS,
        })
    );
    assert_eq!(queue.iter_playable_ids().collect::<Vec<_>>(), ids_before);
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        entries_before
    );
    assert_eq!(queue.next_item_id_snapshot(), item_before);
    assert_eq!(queue.next_compound_group_id_snapshot(), group_before);
    assert_eq!(queue.revision_snapshot(), revision_before);
}

#[test]
fn replace_preserves_one_part_compound_and_publishes_both_lineages_once() {
    let mut queue = PlaylistQueue::new();
    let outcome = queue
        .replace_entries(vec![
            compound_draft("one-part", 1),
            PlaylistEntryDraft::Single(item_draft("single")),
        ])
        .expect("mixed replace must commit");
    let allocated = match outcome {
        ReplacePlaylistEntriesOutcome::Replaced {
            allocated_entries, ..
        } => allocated_entries,
        other => panic!("unexpected replace outcome: {other:?}"),
    };

    assert_eq!(allocated.top_level_entry_count(), 2);
    assert_eq!(allocated.retained_item_count(), 2);
    assert_eq!(queue.top_level_entry_count(), 2);
    assert_eq!(queue.retained_item_count(), 2);
    assert!(matches!(
        queue.iter_top_level_entry_ids().next(),
        Some(PlaylistEntryId::Compound(_))
    ));
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        3
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        2
    );
}

#[test]
fn metadata_patch_inside_compound_changes_only_metadata_revision() {
    let mut queue = PlaylistQueue::new();
    queue
        .append_entries(vec![compound_draft("metadata", 2)])
        .expect("fixture compound must commit");
    let target = queue
        .iter_playable_items()
        .nth(1)
        .expect("second part exists")
        .clone();
    let revision_before = queue.revision_snapshot();
    let entry_ids_before = queue.iter_top_level_entry_ids().collect::<Vec<_>>();
    let playable_ids_before = queue.iter_playable_ids().collect::<Vec<_>>();
    let patched_metadata = CachedPlaylistMetadata::new("patched-part", PlaylistMediaKind::Video);

    queue
        .apply_metadata_patch_batch(vec![PlaylistMetadataPatch::new(
            target.item_id(),
            target.locator().clone(),
            target.local_fingerprint(),
            patched_metadata.clone(),
        )])
        .expect("metadata revision must fit");

    let revision_after = queue.revision_snapshot();
    assert_eq!(revision_after.structural(), revision_before.structural());
    assert_eq!(revision_after.traversal(), revision_before.traversal());
    assert_ne!(revision_after.metadata(), revision_before.metadata());
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        entry_ids_before
    );
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        playable_ids_before
    );
    assert_eq!(
        queue
            .item(target.item_id())
            .expect("patched part remains committed")
            .cached_metadata(),
        &patched_metadata
    );
}

#[test]
fn owned_snapshot_matches_derived_read_without_becoming_queue_cache() {
    let mut queue = PlaylistQueue::new();
    queue
        .append_entries(vec![
            compound_draft("snapshot-group", 2),
            PlaylistEntryDraft::Single(item_draft("snapshot-single")),
        ])
        .expect("fixture batch must commit");
    let snapshot = queue.owned_playable_items_snapshot();
    let captured_ids = snapshot.iter_playable_ids().collect::<Vec<_>>();

    queue
        .append_one(item_draft("later"))
        .expect("later queue mutation must succeed");

    assert_eq!(captured_ids.len(), 3);
    assert_eq!(
        snapshot.iter_playable_ids().collect::<Vec<_>>(),
        captured_ids
    );
    assert_eq!(queue.retained_item_count(), 4);
}

#[test]
fn item_and_group_high_watermarks_survive_clear_and_both_replacement_paths() {
    // Первый one-part compound одновременно открывает Item и Group lineage.
    let mut queue = PlaylistQueue::new();
    let first_append = queue
        .append_entries(vec![compound_draft("first-group", 1)])
        .expect("first compound must commit");
    let AddPlaylistEntriesOutcome::Added(first_allocation) = first_append else {
        panic!("non-empty compound append must allocate identities");
    };
    let Some(PlaylistEntryId::Compound(first_group_id)) = first_allocation.iter_entry_ids().next()
    else {
        panic!("one-part compound must retain compound identity");
    };

    // После первой публикации оба следующих ID обязаны быть равны двум.
    assert_eq!(first_group_id.expose_value_for_persistence(), 1);
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        2
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        2
    );

    // Clear удаляет entries, но не имеет права откатывать ни один allocator.
    assert!(matches!(
        queue.clear(),
        crate::ClearQueueOutcome::Cleared { .. }
    ));
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        2
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        2
    );

    // Compound replace обязан продолжить обе lineage, а не начать их заново.
    let replacement = queue
        .replace_entries(vec![compound_draft("replacement-group", 1)])
        .expect("compound replacement must commit");
    let ReplacePlaylistEntriesOutcome::Replaced {
        allocated_entries, ..
    } = replacement
    else {
        panic!("non-empty replacement must allocate identities");
    };
    let Some(PlaylistEntryId::Compound(replacement_group_id)) =
        allocated_entries.iter_entry_ids().next()
    else {
        panic!("replacement must remain a compound entry");
    };
    assert_eq!(replacement_group_id.expose_value_for_persistence(), 2);
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        3
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        3
    );

    // Legacy strong-install replacement создаёт только Singles и не расходует Group IDs.
    let reservation = queue
        .prepare_reserved_mutation(
            queue.revision_snapshot(),
            ReservedQueueMutation::replace_with_current(
                vec![item_draft("reserved-before")],
                item_draft("reserved-current"),
                vec![item_draft("reserved-after")],
            ),
        )
        .expect("reserved single replacement must preflight");
    let reservation_commit = queue.commit_reserved(reservation);
    assert_eq!(
        reservation_commit
            .allocated_item_ids()
            .as_slice()
            .iter()
            .map(|item_id| item_id.expose_value_for_persistence())
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        3
    );

    // Следующий compound доказывает отсутствие burn/regression после single-only replacement.
    let final_append = queue
        .append_entries(vec![compound_draft("final-group", 2)])
        .expect("final compound must commit");
    let AddPlaylistEntriesOutcome::Added(final_allocation) = final_append else {
        panic!("non-empty final append must allocate identities");
    };
    let Some(PlaylistEntryId::Compound(final_group_id)) = final_allocation.iter_entry_ids().next()
    else {
        panic!("final entry must retain compound identity");
    };
    assert_eq!(final_group_id.expose_value_for_persistence(), 3);
    assert_eq!(
        final_allocation
            .iter_playable_item_ids()
            .map(|item_id| item_id.expose_value_for_persistence())
            .collect::<Vec<_>>(),
        vec![6, 7]
    );
    assert_eq!(
        queue.next_item_id_snapshot().expose_value_for_persistence(),
        8
    );
    assert_eq!(
        queue
            .next_compound_group_id_snapshot()
            .expose_value_for_persistence(),
        4
    );
}

#[test]
fn playlist_core_dependency_boundary_stays_service_ui_serde_free() {
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("playlist-core manifest must be readable");

    for forbidden_dependency in [
        "serde",
        "egui",
        "player-core",
        "service-ytdlp",
        "service-direct-media",
    ] {
        assert!(
            !manifest.contains(forbidden_dependency),
            "forbidden dependency leaked into playlist-core: {forbidden_dependency}"
        );
    }
}
