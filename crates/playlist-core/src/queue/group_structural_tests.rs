//! Focused доказательства S01B для compound-safe structural boundaries.

use std::path::PathBuf;

use crate::{
    AddPlaylistEntriesOutcome, BulkRemoveError, BulkRemoveOutcome, CachedPlaylistMetadata,
    LocalLocator, MoveItemIntent, MoveItemOutcome, MoveItemsOutcome, PlaylistCompoundGroupDraft,
    PlaylistEntryDraft, PlaylistEntryId, PlaylistItemDraft, PlaylistLocator, PlaylistMediaKind,
    PlaylistQueue, PlaylistSortKey, RemovalSnapshotRestoreError, RemoveItemOutcome,
    SortCanonicalQueue, SortCanonicalQueueOutcome, SortDirection, StableInsertionAnchor,
};

/// Строит standalone local draft без filesystem I/O.
fn draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(name)),
        None,
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Audio),
    )
}

/// Строит compound с отдельным root summary и ordered parts.
fn compound(root_name: &str, summary_title: &str, part_names: &[&str]) -> PlaylistEntryDraft {
    let summary = CachedPlaylistMetadata::new(root_name, PlaylistMediaKind::Audio)
        .with_title(Some(summary_title.to_owned()));
    PlaylistEntryDraft::Compound(
        PlaylistCompoundGroupDraft::new(
            PlaylistLocator::Local(LocalLocator::Native(PathBuf::from(root_name))),
            summary,
            part_names.iter().map(|name| draft(name)).collect(),
        )
        .expect("focused compound draft must contain parts"),
    )
}

/// Коммитит Single + Compound + Single и возвращает structural/playable identities.
fn queue_with_compound() -> (
    PlaylistQueue,
    Vec<PlaylistEntryId>,
    Vec<crate::PlaylistItemId>,
) {
    let mut queue = PlaylistQueue::new();
    let AddPlaylistEntriesOutcome::Added(allocated) = queue
        .append_entries(vec![
            PlaylistEntryDraft::Single(draft("single-a.mp3")),
            compound("album-root", "Album summary", &["disc-1.mp3", "disc-2.mp3"]),
            PlaylistEntryDraft::Single(draft("single-z.mp3")),
        ])
        .expect("compound append")
    else {
        panic!("non-empty append must allocate entries");
    };
    (
        queue,
        allocated.iter_entry_ids().collect(),
        allocated.iter_playable_item_ids().collect(),
    )
}

#[test]
fn remove_requires_group_identity_and_detaches_current_part_with_whole_group() {
    let (mut queue, entry_ids, item_ids) = queue_with_compound();
    queue
        .set_traversal_current(item_ids[1])
        .expect("compound part is committed");

    assert_eq!(
        queue.remove(PlaylistEntryId::Single(item_ids[1])),
        RemoveItemOutcome::CompoundPartTarget {
            part_item_id: item_ids[1],
            compound_entry_id: entry_ids[1],
        }
    );
    assert_eq!(queue.retained_item_count(), 4);

    assert!(matches!(
        queue.remove(entry_ids[1]),
        RemoveItemOutcome::Removed {
            entry_id,
            current_outcome: super::RemovalCurrentOutcome::Detached {
                removed_item_id,
            },
            ..
        } if entry_id == entry_ids[1] && removed_item_id == item_ids[1]
    ));
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        [item_ids[0], item_ids[3]]
    );
    assert_eq!(queue.traversal_current(), None);
}

#[test]
fn move_and_multi_move_keep_compound_parts_ordered_and_reject_part_targets() {
    let (mut queue, entry_ids, item_ids) = queue_with_compound();

    assert_eq!(
        queue.move_item(
            PlaylistEntryId::Single(item_ids[1]),
            MoveItemIntent::ToFront,
        ),
        MoveItemOutcome::CompoundPartTarget {
            part_item_id: item_ids[1],
            compound_entry_id: entry_ids[1],
        }
    );
    assert_eq!(
        queue.move_item(
            entry_ids[2],
            MoveItemIntent::Before(PlaylistEntryId::Single(item_ids[2])),
        ),
        MoveItemOutcome::CompoundPartAnchor {
            part_item_id: item_ids[2],
            compound_entry_id: entry_ids[1],
        }
    );
    assert_eq!(
        queue.move_items(&[entry_ids[1], entry_ids[0]], MoveItemIntent::ToBack),
        MoveItemsOutcome::Moved { item_count: 2 }
    );
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        [item_ids[3], item_ids[0], item_ids[1], item_ids[2]]
    );
}

#[test]
fn bulk_remove_and_remove_others_accept_only_whole_top_level_entries() {
    let (mut queue, entry_ids, item_ids) = queue_with_compound();
    assert_eq!(
        queue.remove_batch(&[PlaylistEntryId::Single(item_ids[1])]),
        Err(BulkRemoveError::CompoundPartTarget {
            part_item_id: item_ids[1],
            compound_entry_id: entry_ids[1],
        })
    );
    assert_eq!(queue.retained_item_count(), 4);

    assert!(matches!(
        queue.remove_batch(&[entry_ids[1]]),
        Ok(BulkRemoveOutcome::Removed {
            removed_item_count: 2,
            ..
        })
    ));
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        [entry_ids[0], entry_ids[2]]
    );

    let (mut queue, entry_ids, item_ids) = queue_with_compound();
    assert_eq!(
        queue.remove_others(PlaylistEntryId::Single(item_ids[2])),
        Err(BulkRemoveError::CompoundPartTarget {
            part_item_id: item_ids[2],
            compound_entry_id: entry_ids[1],
        })
    );
    assert!(matches!(
        queue.remove_others(entry_ids[1]),
        Ok(BulkRemoveOutcome::Removed {
            removed_item_count: 2,
            ..
        })
    ));
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        [entry_ids[1]]
    );
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        [item_ids[1], item_ids[2]]
    );
}

#[test]
fn direct_and_prepared_sort_use_group_summary_and_preserve_current_part() {
    let (mut queue, entry_ids, item_ids) = queue_with_compound();
    queue
        .set_traversal_current(item_ids[2])
        .expect("second compound part is committed");
    let traversal_before = queue.traversal_current();

    assert_eq!(
        queue.sort_canonical(SortCanonicalQueue::new(
            PlaylistSortKey::Title,
            SortDirection::Ascending,
        )),
        SortCanonicalQueueOutcome::Reordered { entry_count: 3 }
    );
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        [entry_ids[1], entry_ids[0], entry_ids[2]]
    );
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        [item_ids[1], item_ids[2], item_ids[0], item_ids[3]]
    );
    assert_eq!(queue.traversal_current(), traversal_before);

    let prepared = queue
        .canonical_sort_snapshot()
        .prepare(
            &[],
            SortCanonicalQueue::new(PlaylistSortKey::NaturalFilename, SortDirection::Descending),
            || false,
        )
        .expect("prepared group sort");
    let outcome = queue
        .apply_prepared_canonical_sort(prepared, Vec::new())
        .expect("prepared group sort commit");
    assert!(outcome.reordered());
    let part_positions = queue
        .iter_playable_ids()
        .enumerate()
        .filter_map(|(index, item_id)| {
            (item_id == item_ids[1] || item_id == item_ids[2]).then_some((index, item_id))
        })
        .collect::<Vec<_>>();
    assert_eq!(part_positions[0].1, item_ids[1]);
    assert_eq!(part_positions[1].1, item_ids[2]);
    assert_eq!(queue.traversal_current(), traversal_before);
}

#[test]
fn discovery_anchor_accepts_group_and_rejects_part_or_stale_identity_atomically() {
    let (mut queue, entry_ids, item_ids) = queue_with_compound();
    let revision = queue.revision_snapshot();
    let inserted = queue
        .insert_discovery_batch(
            revision,
            StableInsertionAnchor::before(entry_ids[1]),
            vec![draft("discovered.mp3")],
        )
        .expect("group anchor is a top-level boundary");
    let inserted_id = inserted.item_ids.as_slice()[0];
    assert_eq!(
        queue.iter_playable_ids().collect::<Vec<_>>(),
        [
            item_ids[0],
            inserted_id,
            item_ids[1],
            item_ids[2],
            item_ids[3]
        ]
    );

    let revision = queue.revision_snapshot();
    let watermark = queue.next_item_id_snapshot();
    assert!(matches!(
        queue.insert_discovery_batch(
            revision,
            StableInsertionAnchor::before(PlaylistEntryId::Single(item_ids[1])),
            vec![draft("rejected-part.mp3")],
        ),
        Err(super::DiscoveryBatchInsertError::CompoundPartAnchor { .. })
    ));
    assert_eq!(queue.next_item_id_snapshot(), watermark);
    assert!(matches!(
        queue.insert_discovery_batch(
            revision,
            StableInsertionAnchor::before(PlaylistEntryId::Compound(
                crate::PlaylistCompoundGroupId::from_persistence_value(999)
                    .expect("non-zero group id"),
            )),
            vec![draft("stale.mp3")],
        ),
        Err(super::DiscoveryBatchInsertError::AnchorNotCommitted { .. })
    ));
    assert_eq!(queue.next_item_id_snapshot(), watermark);
}

#[test]
fn removal_undo_restores_exact_group_ids_and_rejects_unrelated_reorder() {
    let (mut queue, entry_ids, item_ids) = queue_with_compound();
    let snapshot = queue.capture_removal_snapshot();
    assert!(matches!(
        queue.remove(entry_ids[1]),
        RemoveItemOutcome::Removed { .. }
    ));
    queue
        .restore_removal_snapshot(snapshot)
        .expect("exact deletion-only state is undoable");
    assert_eq!(
        queue.iter_top_level_entry_ids().collect::<Vec<_>>(),
        entry_ids
    );
    assert_eq!(queue.iter_playable_ids().collect::<Vec<_>>(), item_ids);

    let snapshot = queue.capture_removal_snapshot();
    assert!(matches!(
        queue.move_item(entry_ids[0], MoveItemIntent::ToBack),
        MoveItemOutcome::Moved { .. }
    ));
    assert_eq!(
        queue.restore_removal_snapshot(snapshot),
        Err(RemovalSnapshotRestoreError::NotRemovalResult)
    );
}
