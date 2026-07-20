use std::collections::HashSet;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::{
    AutomaticEndedIntent, AutomaticNavigationOutcome, CachedPlaylistMetadata, LocalLocator,
    MAX_SHUFFLE_HISTORY_ENTRIES, ManualNavigationIntent, ManualNavigationNoItem,
    ManualNavigationOutcome, NextPlaylistItemId, PlaylistItemDraft, PlaylistItemId,
    PlaylistMediaKind, PlaylistQueue, PlaylistQueueRestore, RepeatMode, RestoredPlaylistItem,
    ShuffleHistoryCursor, ShuffleQueueRestoreError, ShuffleToggleOutcome,
    ShuffleTraversalRestoreError, ShuffleTraversalSnapshot, TraversalCurrentMutationOutcome,
};

fn local_draft(index: usize) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(format!("/music/{index}.flac").into()),
        None,
        CachedPlaylistMetadata::new(format!("track-{index}"), PlaylistMediaKind::Audio),
    )
}

fn queue_with_items(item_count: usize) -> (PlaylistQueue, Vec<PlaylistItemId>) {
    let mut queue = PlaylistQueue::new();
    let drafts = (0..item_count).map(local_draft).collect();
    let outcome = queue.append_batch(drafts).expect("append shuffle fixture");
    let ids = match outcome {
        crate::AddItemsOutcome::Added(ids) => ids.into_vec(),
        crate::AddItemsOutcome::NoItemsProvided => Vec::new(),
    };
    (queue, ids)
}

fn open_preview(
    outcome: ManualNavigationOutcome,
) -> (PlaylistItemId, crate::ManualNavigationPreview) {
    match outcome {
        ManualNavigationOutcome::OpenItem { item_id, preview } => (item_id, preview),
        ManualNavigationOutcome::NoItem(reason) => panic!("expected target, got {reason:?}"),
    }
}

fn commit_preview(
    queue: &mut PlaylistQueue,
    preview: crate::ManualNavigationPreview,
) -> PlaylistItemId {
    let token = queue
        .prepare_manual_navigation(preview)
        .expect("prepare manual shuffle transition");
    let target = token.target_item_id();
    let commit = queue.commit_manual_navigation(token);
    assert_eq!(commit.traversal_current().item_id(), target);
    target
}

fn restored_item(item_id: PlaylistItemId, index: usize) -> RestoredPlaylistItem {
    RestoredPlaylistItem::new(item_id, local_draft(index))
}

#[test]
fn seeded_enable_has_exact_order_and_preserves_current() {
    let (mut queue, ids) = queue_with_items(6);
    queue
        .set_traversal_current(ids[2])
        .expect("set current before shuffle");
    let mut random = StdRng::seed_from_u64(0x5eed);

    assert_eq!(
        queue.enable_shuffle_with_rng(&mut random),
        Ok(ShuffleToggleOutcome::Enabled)
    );
    let snapshot = queue
        .shuffle_traversal_snapshot()
        .expect("enabled snapshot");
    assert_eq!(snapshot.history(), &[ids[2]]);
    assert_eq!(
        snapshot.history_cursor().map(|cursor| cursor.index()),
        Some(0)
    );
    assert_eq!(
        snapshot.upcoming(),
        &[ids[1], ids[0], ids[5], ids[3], ids[4]]
    );
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(ids[2])
    );
}

#[test]
fn restored_idle_shuffle_uses_first_upcoming_and_has_no_previous() {
    let (mut queue, ids) = queue_with_items(4);
    let mut random = StdRng::seed_from_u64(7);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable idle shuffle");
    let snapshot = queue.shuffle_traversal_snapshot().expect("idle snapshot");
    assert!(snapshot.history().is_empty());
    assert_eq!(
        snapshot.upcoming().iter().copied().collect::<HashSet<_>>(),
        ids.iter().copied().collect()
    );

    let first_upcoming = snapshot.upcoming()[0];
    let (target, _) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert_eq!(target, first_upcoming);
    assert!(matches!(
        queue.begin_manual_navigation_with_rng(
            ManualNavigationIntent::previous(RepeatMode::RepeatQueue),
            &mut random,
        ),
        ManualNavigationOutcome::NoItem(ManualNavigationNoItem::PreviousFromPersistedIdle)
    ));
}

#[test]
fn fast_next_consumes_intermediate_without_fake_history_and_cancel_is_exact() {
    let (mut queue, ids) = queue_with_items(5);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(11);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    let committed_base = queue.shuffle_traversal_snapshot().expect("base snapshot");

    let (first_target, first_preview) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::StopAtEnd),
        &mut random,
    ));
    let (latest_target, latest_preview) = open_preview(
        queue
            .continue_manual_navigation_with_rng(
                first_preview,
                ManualNavigationIntent::next(RepeatMode::StopAtEnd),
                &mut random,
            )
            .expect("continue fast Next"),
    );
    assert_eq!(
        queue.shuffle_traversal_snapshot(),
        Some(committed_base.clone())
    );
    commit_preview(&mut queue, latest_preview);

    let committed = queue
        .shuffle_traversal_snapshot()
        .expect("committed traversal");
    assert_eq!(committed.history(), &[ids[0], latest_target]);
    assert!(!committed.upcoming().contains(&first_target));
    assert!(!committed.upcoming().contains(&latest_target));
    let (previous_target, _) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert_eq!(previous_target, ids[0]);

    let (cancel_target, cancel_preview) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::StopAtEnd),
        &mut random,
    ));
    let before_cancel = queue.shuffle_traversal_snapshot();
    let discarded = queue.discard_manual_navigation(cancel_preview);
    assert_eq!(discarded.latest_target_item_id(), cancel_target);
    assert_eq!(queue.shuffle_traversal_snapshot(), before_cancel);
}

#[test]
fn failed_latest_target_retains_preview_for_retry_next_previous_and_discard() {
    let (mut queue, ids) = queue_with_items(5);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(21);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    let committed_base = queue.shuffle_traversal_snapshot();
    let (first_target, first_preview) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::StopAtEnd),
        &mut random,
    ));
    let (failed_target, failed_preview) = open_preview(
        queue
            .continue_manual_navigation_with_rng(
                first_preview,
                ManualNavigationIntent::next(RepeatMode::StopAtEnd),
                &mut random,
            )
            .expect("choose failed latest target"),
    );
    let token = queue
        .prepare_manual_navigation(failed_preview)
        .expect("prepare failing request");
    let failed_preview = queue.fail_manual_navigation(token);
    assert_eq!(failed_preview.latest_target_item_id(), failed_target);
    assert_eq!(queue.shuffle_traversal_snapshot(), committed_base);

    let retry_token = queue
        .prepare_manual_navigation(failed_preview)
        .expect("retry exact failed target");
    assert_eq!(retry_token.target_item_id(), failed_target);
    let failed_preview = queue.fail_manual_navigation(retry_token);
    let (backtracked_target, backtracked_preview) = open_preview(
        queue
            .continue_manual_navigation_with_rng(
                failed_preview,
                ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
                &mut random,
            )
            .expect("backtrack failed preview"),
    );
    assert_eq!(backtracked_target, first_target);
    let (next_target, next_preview) = open_preview(
        queue
            .continue_manual_navigation_with_rng(
                backtracked_preview,
                ManualNavigationIntent::next(RepeatMode::StopAtEnd),
                &mut random,
            )
            .expect("continue same preview again"),
    );
    assert_eq!(next_target, failed_target);
    queue.discard_manual_navigation(next_preview);
    assert_eq!(queue.shuffle_traversal_snapshot(), committed_base);
}

#[test]
fn factual_history_supports_back_forward_branching_and_duplicate_visits() {
    let (mut queue, ids) = queue_with_items(4);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(31);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    queue.commit_manual_play(ids[1]).expect("play B");
    queue.commit_manual_play(ids[2]).expect("play C");

    let (_, speculative_previous) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert!(matches!(
        queue
            .continue_manual_navigation_with_rng(
                speculative_previous,
                ManualNavigationIntent::next(RepeatMode::StopAtEnd),
                &mut random,
            )
            .expect("speculative return to origin"),
        ManualNavigationOutcome::NoItem(
            ManualNavigationNoItem::ReturnedToCommittedOrigin { item_id }
        ) if item_id == ids[2]
    ));

    let (_, previous_preview) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert_eq!(commit_preview(&mut queue, previous_preview), ids[1]);
    let snapshot = queue.shuffle_traversal_snapshot().expect("back snapshot");
    assert_eq!(snapshot.history(), &[ids[0], ids[1], ids[2]]);
    assert_eq!(
        snapshot.history_cursor().map(|cursor| cursor.index()),
        Some(1)
    );

    let (forward_target, forward_preview) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert_eq!(forward_target, ids[2]);
    commit_preview(&mut queue, forward_preview);
    let (_, previous_preview) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
        &mut random,
    ));
    commit_preview(&mut queue, previous_preview);
    queue.commit_manual_play(ids[3]).expect("new manual branch");
    queue
        .commit_manual_play(ids[3])
        .expect("repeated factual visit");
    let branched = queue
        .shuffle_traversal_snapshot()
        .expect("branched snapshot");
    assert_eq!(branched.history(), &[ids[0], ids[1], ids[3], ids[3]]);
}

#[test]
fn rolling_history_prunes_exactly_on_1025th_entry() {
    let (mut queue, ids) = queue_with_items(2);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(41);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    for transition_index in 1..MAX_SHUFFLE_HISTORY_ENTRIES {
        let target = ids[transition_index % 2];
        queue.commit_manual_play(target).expect("fill history cap");
    }
    let at_cap = queue.shuffle_traversal_snapshot().expect("history at cap");
    assert_eq!(at_cap.history().len(), MAX_SHUFFLE_HISTORY_ENTRIES);
    assert_eq!(at_cap.history()[0], ids[0]);

    queue
        .commit_manual_play(ids[MAX_SHUFFLE_HISTORY_ENTRIES % 2])
        .expect("push 1025th visit");
    let pruned = queue.shuffle_traversal_snapshot().expect("pruned history");
    assert_eq!(pruned.history().len(), MAX_SHUFFLE_HISTORY_ENTRIES);
    assert_eq!(pruned.history()[0], ids[1]);
    assert_eq!(
        pruned.history_cursor().map(|cursor| cursor.index()),
        Some(MAX_SHUFFLE_HISTORY_ENTRIES - 1)
    );
}

#[test]
fn repeat_queue_starts_new_cycle_without_last_to_same_first() {
    let (mut queue, ids) = queue_with_items(4);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(51);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    while let Some(next) = queue
        .shuffle_traversal_snapshot()
        .and_then(|snapshot| snapshot.upcoming().first().copied())
    {
        queue.commit_manual_play(next).expect("consume cycle");
    }
    let last_item_id = queue
        .traversal_current()
        .expect("current after cycle")
        .item_id();
    assert!(matches!(
        queue.begin_manual_navigation_with_rng(
            ManualNavigationIntent::next(RepeatMode::StopAtEnd),
            &mut random,
        ),
        ManualNavigationOutcome::NoItem(ManualNavigationNoItem::QueueBoundary { .. })
    ));
    assert!(matches!(
        queue.begin_manual_navigation_with_rng(
            ManualNavigationIntent::next(RepeatMode::RepeatOne),
            &mut random,
        ),
        ManualNavigationOutcome::NoItem(ManualNavigationNoItem::QueueBoundary { .. })
    ));
    let (new_cycle_first, _) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::RepeatQueue),
        &mut random,
    ));
    assert_ne!(new_cycle_first, last_item_id);

    let (mut single, single_ids) = queue_with_items(1);
    single
        .set_traversal_current(single_ids[0])
        .expect("single current");
    single
        .enable_shuffle_with_rng(&mut random)
        .expect("single shuffle");
    let (single_target, _) = open_preview(single.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::RepeatQueue),
        &mut random,
    ));
    assert_eq!(single_target, single_ids[0]);
}

#[test]
fn batch_add_preserves_old_upcoming_order_and_bulk_remove_cleans_references() {
    let (mut queue, ids) = queue_with_items(6);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(61);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    queue.commit_manual_play(ids[1]).expect("history visit");
    queue.commit_manual_play(ids[2]).expect("history visit");
    let old_upcoming = queue
        .shuffle_traversal_snapshot()
        .expect("before add")
        .upcoming()
        .to_vec();
    let added = queue
        .append_batch_with_rng(
            vec![local_draft(10), local_draft(11), local_draft(12)],
            &mut random,
        )
        .expect("batch add");
    let new_ids = match added {
        crate::AddItemsOutcome::Added(ids) => ids.into_vec(),
        crate::AddItemsOutcome::NoItemsProvided => panic!("expected added IDs"),
    };
    let after_add = queue.shuffle_traversal_snapshot().expect("after add");
    let retained_old: Vec<_> = after_add
        .upcoming()
        .iter()
        .copied()
        .filter(|item_id| old_upcoming.contains(item_id))
        .collect();
    assert_eq!(retained_old, old_upcoming);
    assert!(
        new_ids
            .iter()
            .all(|item_id| after_add.upcoming().contains(item_id))
    );

    queue
        .remove_batch(&[
            crate::PlaylistEntryId::Single(ids[1]),
            crate::PlaylistEntryId::Single(ids[4]),
            crate::PlaylistEntryId::Single(new_ids[1]),
        ])
        .expect("one bulk remove");
    let after_remove = queue.shuffle_traversal_snapshot().expect("after remove");
    for removed in [ids[1], ids[4], new_ids[1]] {
        assert!(!after_remove.history().contains(&removed));
        assert!(!after_remove.upcoming().contains(&removed));
    }
    let (previous_after_repair, _) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert_eq!(previous_after_repair, ids[0]);
    assert!(matches!(
        queue.remove(crate::PlaylistEntryId::Single(ids[2])),
        crate::RemoveItemOutcome::Removed { .. }
    ));
    let idle_after_current_removal = queue
        .shuffle_traversal_snapshot()
        .expect("enabled idle after current removal");
    assert!(idle_after_current_removal.history().is_empty());
    assert_eq!(
        idle_after_current_removal
            .upcoming()
            .iter()
            .copied()
            .collect::<HashSet<_>>(),
        queue.iter_playable_ids().collect()
    );
}

#[test]
fn bulk_algorithms_report_one_linear_pass_over_each_input_collection() {
    let (mut queue, ids) = queue_with_items(8);
    let mut random = StdRng::seed_from_u64(66);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    let old_upcoming_len = queue
        .shuffle_traversal_snapshot()
        .expect("snapshot before characterization")
        .upcoming()
        .len();
    let synthetic_new_ids = [
        PlaylistItemId::from_persistence_value(100).expect("synthetic ID"),
        PlaylistItemId::from_persistence_value(101).expect("synthetic ID"),
        PlaylistItemId::from_persistence_value(102).expect("synthetic ID"),
    ];
    let merge_work = queue
        .shuffle_traversal
        .as_mut()
        .expect("enabled traversal")
        .merge_new_items(&synthetic_new_ids, &mut random);
    assert_eq!(merge_work, (old_upcoming_len, synthetic_new_ids.len()));

    queue
        .commit_manual_play(ids[0])
        .expect("first factual visit");
    queue
        .commit_manual_play(ids[1])
        .expect("second factual visit");
    let before_remove = queue.shuffle_traversal_snapshot().expect("before remove");
    let remove_work = queue
        .shuffle_traversal
        .as_mut()
        .expect("enabled traversal")
        .remove_items(&HashSet::from([ids[0]]), &ids[1..], false);
    assert_eq!(
        remove_work,
        (
            before_remove.upcoming().len(),
            before_remove.history().len()
        )
    );
}

#[test]
fn toggle_reset_and_reorder_preserve_required_boundaries() {
    let (mut queue, ids) = queue_with_items(5);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(71);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    queue.commit_manual_play(ids[2]).expect("create history");
    let before_reorder = queue.shuffle_traversal_snapshot();
    queue.move_item(
        crate::PlaylistEntryId::Single(ids[4]),
        crate::MoveItemIntent::ToFront,
    );
    assert_eq!(queue.shuffle_traversal_snapshot(), before_reorder);
    assert_eq!(queue.disable_shuffle(), Ok(ShuffleToggleOutcome::Disabled));
    assert!(queue.shuffle_traversal_snapshot().is_none());
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(ids[2])
    );
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable new cycle");
    let restarted = queue
        .shuffle_traversal_snapshot()
        .expect("restarted traversal");
    assert_eq!(restarted.history(), &[ids[2]]);
    assert_eq!(restarted.upcoming().len(), ids.len() - 1);
}

#[test]
fn restore_accepts_repeated_history_and_rejects_all_corrupt_reference_shapes() {
    let ids: Vec<_> = (1..=3)
        .map(|value| PlaylistItemId::from_persistence_value(value).expect("non-zero test ID"))
        .collect();
    let queue_snapshot = || {
        PlaylistQueueRestore::new(
            ids.iter()
                .enumerate()
                .map(|(index, item_id)| restored_item(*item_id, index))
                .collect(),
            NextPlaylistItemId::from_persistence_value(4).expect("next ID"),
            Some(ids[1]),
        )
    };
    let valid = ShuffleTraversalSnapshot::new(
        vec![ids[0], ids[1], ids[0], ids[1]],
        Some(ShuffleHistoryCursor::from_index(3)),
        vec![ids[2]],
    );
    assert!(PlaylistQueue::restore_with_shuffle(queue_snapshot(), valid).is_ok());

    let duplicate_upcoming = ShuffleTraversalSnapshot::new(
        vec![ids[1]],
        Some(ShuffleHistoryCursor::from_index(0)),
        vec![ids[2], ids[2]],
    );
    assert!(matches!(
        PlaylistQueue::restore_with_shuffle(queue_snapshot(), duplicate_upcoming),
        Err(ShuffleQueueRestoreError::Traversal(
            ShuffleTraversalRestoreError::DuplicateUpcomingItem { .. }
        ))
    ));
    let missing = PlaylistItemId::from_persistence_value(99).expect("missing ID");
    let invalid_reference = ShuffleTraversalSnapshot::new(
        vec![ids[1]],
        Some(ShuffleHistoryCursor::from_index(0)),
        vec![missing],
    );
    assert!(matches!(
        PlaylistQueue::restore_with_shuffle(queue_snapshot(), invalid_reference),
        Err(ShuffleQueueRestoreError::Traversal(
            ShuffleTraversalRestoreError::UpcomingItemNotCommitted { .. }
        ))
    ));

    let idle_queue_snapshot = PlaylistQueueRestore::new(
        ids.iter()
            .enumerate()
            .map(|(index, item_id)| restored_item(*item_id, index))
            .collect(),
        NextPlaylistItemId::from_persistence_value(4).expect("next ID"),
        None,
    );
    let incomplete_idle = ShuffleTraversalSnapshot::new(Vec::new(), None, vec![ids[0], ids[1]]);
    assert!(matches!(
        PlaylistQueue::restore_with_shuffle(idle_queue_snapshot, incomplete_idle),
        Err(ShuffleQueueRestoreError::Traversal(
            ShuffleTraversalRestoreError::IdleUpcomingDoesNotCoverCanonicalQueue
        ))
    ));
}

#[test]
fn automatic_shuffle_respects_repeat_one_stop_and_next_upcoming() {
    let (mut queue, ids) = queue_with_items(3);
    queue.set_traversal_current(ids[0]).expect("set origin");
    let mut random = StdRng::seed_from_u64(81);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");
    let first_upcoming = queue
        .shuffle_traversal_snapshot()
        .expect("snapshot")
        .upcoming()[0];
    assert_eq!(
        queue.automatic_navigation_with_rng(
            AutomaticEndedIntent::new(RepeatMode::StopAtEnd),
            &mut random,
        ),
        AutomaticNavigationOutcome::OpenItem {
            item_id: first_upcoming
        }
    );
    assert_eq!(
        queue.automatic_navigation_with_rng(
            AutomaticEndedIntent::new(RepeatMode::RepeatOne),
            &mut random,
        ),
        AutomaticNavigationOutcome::ReplayCurrent { item_id: ids[0] }
    );
}

#[test]
fn empty_shuffle_is_deterministic_and_idempotent() {
    let mut queue = PlaylistQueue::new();
    let mut random = StdRng::seed_from_u64(91);
    assert_eq!(
        queue.enable_shuffle_with_rng(&mut random),
        Ok(ShuffleToggleOutcome::Enabled)
    );
    assert_eq!(
        queue.enable_shuffle_with_rng(&mut random),
        Ok(ShuffleToggleOutcome::AlreadyEnabled)
    );
    let snapshot = queue.shuffle_traversal_snapshot().expect("empty snapshot");
    assert!(snapshot.history().is_empty());
    assert!(snapshot.upcoming().is_empty());
    assert_eq!(
        queue.commit_manual_play(PlaylistItemId::from_persistence_value(1).expect("ID")),
        Err(crate::TraversalCurrentMutationError::ItemNotCommitted {
            item_id: PlaylistItemId::from_persistence_value(1).expect("ID")
        })
    );
    assert_eq!(queue.disable_shuffle(), Ok(ShuffleToggleOutcome::Disabled));
    assert_eq!(
        queue.clear_traversal_current(),
        Ok(TraversalCurrentMutationOutcome::AlreadyAbsent)
    );
}
