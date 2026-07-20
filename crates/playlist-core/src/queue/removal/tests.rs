use std::path::PathBuf;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::{
    AddItemsOutcome, CachedPlaylistMetadata, LocalLocator, ManualNavigationIntent,
    ManualNavigationNoItem, ManualNavigationOutcome, PlaylistItemDraft, PlaylistMediaKind,
    PlaylistQueue, RemovalSnapshotRestoreError, RepeatMode, TraversalCurrentEffect,
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

    assert!(
        ids.iter()
            .all(|item_id| snapshot.shares_item_payload_with(&queue, *item_id))
    );
}
