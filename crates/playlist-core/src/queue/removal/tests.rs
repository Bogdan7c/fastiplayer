use std::path::PathBuf;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::{
    AddItemsOutcome, CachedPlaylistMetadata, LocalLocator, ManualNavigationIntent,
    ManualNavigationNoItem, ManualNavigationOutcome, PlaylistItemDraft, PlaylistMediaKind,
    PlaylistMetadataPatch, PlaylistQueue, RemovalSnapshotRestoreError, RepeatMode,
    TraversalCurrentEffect,
};

/// Создаёт строку с heap-backed locator/metadata для проверки реального sharing.
fn draft(index: usize) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(format!("/media/long-item-{index}.mkv"))),
        None,
        CachedPlaylistMetadata::new(
            format!("Длинное отображаемое имя элемента {index}"),
            PlaylistMediaKind::Video,
        ),
    )
}

#[test]
fn removal_snapshot_restores_current_order_and_allocator_as_new_mutation() {
    let mut queue = PlaylistQueue::new();
    let ids = match queue
        .append_batch((0..4).map(draft).collect())
        .expect("fixture append")
    {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    queue
        .set_traversal_current(ids[1])
        .expect("fixture current");
    let allocator_before = queue.next_item_id_snapshot();
    let snapshot = queue.capture_removal_snapshot();

    let removal = queue.remove(crate::PlaylistEntryId::Single(ids[1]));
    assert!(matches!(
        removal,
        crate::RemoveItemOutcome::Removed {
            traversal_current_effect: TraversalCurrentEffect::Cleared,
            ..
        }
    ));
    assert!(queue.traversal_current().is_none());

    let restored = queue
        .restore_removal_snapshot(snapshot)
        .expect("immediate undo must restore");
    assert_eq!(
        restored
            .traversal_current()
            .map(|current| current.item_id()),
        Some(ids[1])
    );
    assert_eq!(queue.iter_playable_ids().collect::<Vec<_>>(), ids);
    assert_eq!(queue.next_item_id_snapshot(), allocator_before);
}

#[test]
fn removed_current_restart_state_is_idle_until_explicit_deterministic_navigation() {
    let mut queue = PlaylistQueue::new();
    let ids = match queue
        .append_batch((0..3).map(draft).collect())
        .expect("fixture append")
    {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    queue
        .set_traversal_current(ids[1])
        .expect("fixture current");
    let _removed = queue.remove(crate::PlaylistEntryId::Single(ids[1]));

    assert!(queue.traversal_current().is_none());
    assert!(matches!(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        ManualNavigationOutcome::OpenItem { item_id, .. } if item_id == ids[0]
    ));
    assert!(matches!(
        queue.begin_manual_navigation(ManualNavigationIntent::previous(RepeatMode::StopAtEnd)),
        ManualNavigationOutcome::NoItem(ManualNavigationNoItem::PreviousFromPersistedIdle)
    ));
    // Query/preview не назначили current: process restart сам ничего не открывает.
    assert!(queue.traversal_current().is_none());
}

#[test]
fn snapshot_rejects_later_structural_mutation_instead_of_overwriting_it() {
    let mut queue = PlaylistQueue::new();
    let ids = match queue
        .append_batch(vec![draft(1), draft(2)])
        .expect("append")
    {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    let snapshot = queue.capture_removal_snapshot();
    let _removed = queue.remove(crate::PlaylistEntryId::Single(ids[0]));
    let _later_append = queue.append_one(draft(3)).expect("later mutation");

    assert_eq!(
        queue.restore_removal_snapshot(snapshot),
        Err(RemovalSnapshotRestoreError::StaleStructuralRevision)
    );
}

#[test]
fn removal_undo_restores_exact_shuffle_history_cursor_and_upcoming() {
    let mut queue = PlaylistQueue::new();
    let ids = match queue
        .append_batch((0..6).map(draft).collect())
        .expect("fixture append")
    {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    queue
        .set_traversal_current(ids[2])
        .expect("fixture current");
    let mut random = StdRng::seed_from_u64(0x12_A0);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    let shuffle_before = queue
        .shuffle_traversal_snapshot()
        .expect("shuffle snapshot before removal");
    let snapshot = queue.capture_removal_snapshot();

    let _removed = queue.remove(crate::PlaylistEntryId::Single(ids[4]));
    queue
        .restore_removal_snapshot(snapshot)
        .expect("undo shuffle removal");

    assert_eq!(
        queue
            .shuffle_traversal_snapshot()
            .expect("shuffle restored"),
        shuffle_before
    );
}

#[test]
fn fifty_thousand_row_snapshot_shares_every_heavy_payload() {
    let mut queue = PlaylistQueue::new();
    let ids = match queue
        .append_batch((0..crate::MAX_PLAYLIST_ITEMS).map(draft).collect())
        .expect("hard-cap fixture")
    {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    let snapshot = queue.capture_removal_snapshot();

    // Проверяем весь hard-cap snapshot за один проход. Поиск каждого ID
    // от начала queue/snapshot превращал этот oracle в O(N²), хотя сам
    // production snapshot копирует shared handles за O(N).
    assert_eq!(snapshot.entries.len(), ids.len());
    assert_eq!(queue.entries.len(), ids.len());
    for ((snapshot_entry, queue_entry), item_id) in
        snapshot.entries.iter().zip(&queue.entries).zip(ids)
    {
        let snapshot_item = snapshot_entry.item(item_id).expect("snapshot row identity");
        let queue_item = queue_entry.item(item_id).expect("queue row identity");
        assert!(snapshot_item.shares_payload_with(queue_item));
    }
}

#[test]
fn snapshot_restore_after_metadata_patch_keeps_fresh_metadata_of_retained_rows() {
    let mut queue = PlaylistQueue::new();
    let ids = match queue
        .append_batch(vec![draft(1), draft(2), draft(3)])
        .expect("append")
    {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    let snapshot = queue.capture_removal_snapshot();
    let _removed = queue.remove(crate::PlaylistEntryId::Single(ids[1]));
    let retained = queue.item(ids[2]).expect("retained row").clone();
    let _patched = queue
        .apply_metadata_patch_batch(vec![PlaylistMetadataPatch::new(
            ids[2],
            retained.locator().clone(),
            retained.local_fingerprint(),
            retained
                .cached_metadata()
                .clone()
                .with_title(Some("Fresh title".to_owned())),
        )])
        .expect("patch applies");

    queue
        .restore_removal_snapshot(snapshot)
        .expect("metadata patch must not block restore");

    let restored_order = queue.iter_top_level_entry_ids().collect::<Vec<_>>();
    assert_eq!(
        restored_order,
        ids.iter()
            .copied()
            .map(crate::PlaylistEntryId::Single)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        queue
            .item(ids[2])
            .map(|item| item.cached_metadata().title()),
        Some(Some("Fresh title"))
    );
    assert_ne!(
        queue
            .item(ids[1])
            .map(|item| item.cached_metadata().title()),
        Some(Some("Fresh title"))
    );
}

/// Собирает очередь из `count` строк и возвращает id в каноническом порядке.
fn queue_with_rows(count: usize) -> (PlaylistQueue, Vec<crate::PlaylistItemId>) {
    let mut queue = PlaylistQueue::new();
    let ids = match queue
        .append_batch((0..count).map(draft).collect())
        .expect("fixture append")
    {
        AddItemsOutcome::Added(ids) => ids.into_vec(),
        AddItemsOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    (queue, ids)
}

#[test]
fn restoring_rows_keeps_current_pointer_that_moved_after_removal() {
    let (mut queue, ids) = queue_with_rows(4);
    queue
        .set_traversal_current(ids[0])
        .expect("fixture current");
    let snapshot = queue.capture_removal_snapshot();
    let _removed = queue.remove(crate::PlaylistEntryId::Single(ids[1]));
    // Автопереход A -> C и ещё один шаг C -> D: revision ушла вперёд больше чем на 1.
    queue.set_traversal_current(ids[2]).expect("advance to C");
    queue.set_traversal_current(ids[3]).expect("advance to D");

    let restored = queue
        .restore_removed_rows_keeping_current(snapshot)
        .expect("rows come back without touching current");

    assert_eq!(queue.iter_playable_ids().collect::<Vec<_>>(), ids);
    // Указатель продолжает указывать на D по id, хотя D сдвинулся с индекса 2 на индекс 3.
    assert_eq!(
        restored
            .traversal_current()
            .map(|current| current.item_id()),
        Some(ids[3])
    );
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(ids[3])
    );
}

#[test]
fn restoring_rows_keeping_current_refuses_active_shuffle_without_changing_queue() {
    let (mut queue, ids) = queue_with_rows(4);
    queue
        .set_traversal_current(ids[0])
        .expect("fixture current");
    let mut random = StdRng::seed_from_u64(0x9B);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("shuffle enabled");
    let snapshot = queue.capture_removal_snapshot();
    let _removed = queue.remove(crate::PlaylistEntryId::Single(ids[1]));
    let order_before = queue.iter_playable_ids().collect::<Vec<_>>();

    assert_eq!(
        queue.restore_removed_rows_keeping_current(snapshot),
        Err(RemovalSnapshotRestoreError::ShuffleActive)
    );
    assert_eq!(queue.iter_playable_ids().collect::<Vec<_>>(), order_before);
}

#[test]
fn restoring_rows_keeping_current_still_rejects_other_structural_changes() {
    let (mut queue, ids) = queue_with_rows(3);
    let snapshot = queue.capture_removal_snapshot();
    let _removed = queue.remove(crate::PlaylistEntryId::Single(ids[1]));
    let _later_append = queue.append_one(draft(9)).expect("later mutation");

    assert_eq!(
        queue.restore_removed_rows_keeping_current(snapshot),
        Err(RemovalSnapshotRestoreError::StaleStructuralRevision)
    );
}
