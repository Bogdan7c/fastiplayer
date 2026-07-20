//! Focused S01C tests для part traversal, reservation и group-block shuffle.

use std::collections::HashSet;
use std::path::PathBuf;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::{
    AddPlaylistEntriesOutcome, AutomaticEndedIntent, AutomaticNavigationOutcome,
    AutomaticTraversalAdvance, AutomaticTraversalStart, CachedPlaylistMetadata,
    DiscoveryBatchInsertOutcome, LocalLocator, ManualNavigationIntent, ManualNavigationNoItem,
    ManualNavigationOutcome, MoveItemIntent, MoveItemOutcome, PlaylistCompoundGroupDraft,
    PlaylistEntryDraft, PlaylistEntryId, PlaylistItemDraft, PlaylistItemId, PlaylistLocator,
    PlaylistMediaKind, PlaylistQueue, PlaylistSortKey, PrepareReservedMutationError,
    RemoveItemOutcome, RepeatMode, ReservedQueueMutation, ShuffleToggleOutcome, SortCanonicalQueue,
    SortCanonicalQueueOutcome, SortDirection, StableInsertionAnchor,
};

/// Строит standalone draft без filesystem I/O.
fn draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(name)),
        None,
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Audio),
    )
}

/// Строит compound entry с ordered source parts.
fn compound(root_name: &str, part_names: &[&str]) -> PlaylistEntryDraft {
    PlaylistEntryDraft::Compound(
        PlaylistCompoundGroupDraft::new(
            PlaylistLocator::Local(LocalLocator::Native(PathBuf::from(root_name))),
            CachedPlaylistMetadata::new(root_name, PlaylistMediaKind::Audio),
            part_names
                .iter()
                .map(|part_name| draft(part_name))
                .collect(),
        )
        .expect("focused compound fixture is non-empty"),
    )
}

/// Коммитит Single + трехчастный Compound + Single.
fn queue_fixture() -> (PlaylistQueue, Vec<PlaylistEntryId>, Vec<PlaylistItemId>) {
    let mut queue = PlaylistQueue::new();
    let AddPlaylistEntriesOutcome::Added(allocated) = queue
        .append_entries(vec![
            PlaylistEntryDraft::Single(draft("a-single.mp3")),
            compound(
                "b-compound",
                &["b-part-1.mp3", "b-part-2.mp3", "b-part-3.mp3"],
            ),
            PlaylistEntryDraft::Single(draft("z-single.mp3")),
        ])
        .expect("append S01C fixture")
    else {
        panic!("non-empty S01C fixture must allocate identities");
    };
    (
        queue,
        allocated.iter_entry_ids().collect(),
        allocated.iter_playable_item_ids().collect(),
    )
}

/// Извлекает exact target и opaque preview.
fn open_preview(
    outcome: ManualNavigationOutcome,
) -> (PlaylistItemId, super::ManualNavigationPreview) {
    match outcome {
        ManualNavigationOutcome::OpenItem { item_id, preview } => (item_id, preview),
        ManualNavigationOutcome::NoItem(reason) => {
            panic!("expected manual target, got {reason:?}")
        }
    }
}

/// Проводит preview через Ready reservation и exact Installed commit.
fn install_preview(
    queue: &mut PlaylistQueue,
    preview: super::ManualNavigationPreview,
) -> PlaylistItemId {
    let token = queue
        .prepare_manual_navigation(preview)
        .expect("Ready target must reserve exact committed part");
    let target_item_id = token.target_item_id();
    let commit = queue.commit_manual_navigation(token);
    assert_eq!(commit.traversal_current().item_id(), target_item_id);
    target_item_id
}

#[test]
fn canonical_manual_and_automatic_traversal_walk_parts_before_adjacent_entries() {
    let (mut queue, _entry_ids, item_ids) = queue_fixture();
    let [single_a, part_1, part_2, part_3, single_z] = item_ids.as_slice() else {
        panic!("fixture must expose five playable IDs");
    };
    queue
        .set_traversal_current(*part_2)
        .expect("start from middle part");

    let (previous_target, previous_preview) = open_preview(
        queue.begin_manual_navigation(ManualNavigationIntent::previous(RepeatMode::StopAtEnd)),
    );
    assert_eq!(previous_target, *part_1);
    install_preview(&mut queue, previous_preview);

    let (next_target, next_preview) = open_preview(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
    );
    assert_eq!(next_target, *part_2);
    install_preview(&mut queue, next_preview);

    assert_eq!(
        queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::StopAtEnd)),
        AutomaticNavigationOutcome::OpenItem { item_id: *part_3 }
    );
    queue
        .set_traversal_current(*part_3)
        .expect("simulate exact Installed of last part");
    assert_eq!(
        queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::StopAtEnd)),
        AutomaticNavigationOutcome::OpenItem { item_id: *single_z }
    );
    assert_eq!(
        queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::RepeatOne)),
        AutomaticNavigationOutcome::ReplayCurrent { item_id: *part_3 }
    );
    queue
        .set_traversal_current(*single_z)
        .expect("move to canonical tail");
    assert_eq!(
        queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::RepeatQueue)),
        AutomaticNavigationOutcome::OpenItem { item_id: *single_a }
    );
}

#[test]
fn shuffle_from_middle_part_keeps_group_suffix_and_uses_only_factual_previous_history() {
    let (mut queue, entry_ids, item_ids) = queue_fixture();
    let [_, _part_1, part_2, part_3, _] = item_ids.as_slice() else {
        panic!("fixture must expose five playable IDs");
    };
    queue
        .set_traversal_current(*part_2)
        .expect("start from middle part");
    let mut random = StdRng::seed_from_u64(0x501c);
    assert_eq!(
        queue.enable_shuffle_with_rng(&mut random),
        Ok(ShuffleToggleOutcome::Enabled)
    );

    let enabled = queue
        .shuffle_traversal_snapshot()
        .expect("shuffle snapshot");
    assert_eq!(enabled.history(), &[*part_2]);
    assert_eq!(
        enabled.upcoming().iter().copied().collect::<HashSet<_>>(),
        HashSet::from([entry_ids[0], entry_ids[2]])
    );
    assert!(matches!(
        queue.begin_manual_navigation_with_rng(
            ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
            &mut random,
        ),
        ManualNavigationOutcome::NoItem(ManualNavigationNoItem::QueueBoundary { .. })
    ));
    assert_eq!(queue.disable_shuffle(), Ok(ShuffleToggleOutcome::Disabled));
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(*part_2)
    );
    let (canonical_previous, canonical_preview) = open_preview(
        queue.begin_manual_navigation(ManualNavigationIntent::previous(RepeatMode::StopAtEnd)),
    );
    assert_eq!(canonical_previous, item_ids[1]);
    queue.discard_manual_navigation(canonical_preview);
    assert_eq!(
        queue.enable_shuffle_with_rng(&mut random),
        Ok(ShuffleToggleOutcome::Enabled)
    );

    let (part_suffix_target, suffix_preview) =
        open_preview(queue.begin_manual_navigation_with_rng(
            ManualNavigationIntent::next(RepeatMode::StopAtEnd),
            &mut random,
        ));
    assert_eq!(part_suffix_target, *part_3);
    install_preview(&mut queue, suffix_preview);

    let (next_block_target, block_preview) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::next(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert!(next_block_target == item_ids[0] || next_block_target == item_ids[4]);
    install_preview(&mut queue, block_preview);
    let committed = queue
        .shuffle_traversal_snapshot()
        .expect("committed group-aware traversal");
    assert_eq!(committed.history(), &[*part_2, *part_3, next_block_target]);

    let (previous_target, _) = open_preview(queue.begin_manual_navigation_with_rng(
        ManualNavigationIntent::previous(RepeatMode::StopAtEnd),
        &mut random,
    ));
    assert_eq!(previous_target, *part_3);
}

#[test]
fn shuffle_cycle_reenters_compound_at_first_part_and_preserves_internal_order() {
    let mut queue = PlaylistQueue::new();
    let AddPlaylistEntriesOutcome::Added(allocated) = queue
        .append_entries(vec![
            compound("group-a", &["a-1.mp3", "a-2.mp3", "a-3.mp3"]),
            compound("group-b", &["b-1.mp3", "b-2.mp3"]),
        ])
        .expect("append two groups")
    else {
        panic!("two groups must allocate identities");
    };
    let item_ids = allocated.iter_playable_item_ids().collect::<Vec<_>>();
    queue
        .set_traversal_current(item_ids[2])
        .expect("start at final part of first group");
    let mut random = StdRng::seed_from_u64(0x51c1e);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable group shuffle");

    for expected_target in [item_ids[3], item_ids[4]] {
        let (target, preview) = open_preview(queue.begin_manual_navigation_with_rng(
            ManualNavigationIntent::next(RepeatMode::StopAtEnd),
            &mut random,
        ));
        assert_eq!(target, expected_target);
        install_preview(&mut queue, preview);
    }

    for expected_target in [item_ids[0], item_ids[1], item_ids[2]] {
        let (target, preview) = open_preview(queue.begin_manual_navigation_with_rng(
            ManualNavigationIntent::next(RepeatMode::RepeatQueue),
            &mut random,
        ));
        assert_eq!(target, expected_target);
        install_preview(&mut queue, preview);
    }
}

#[test]
fn part_reservation_commits_only_on_installed_and_blocks_remove_until_abort() {
    let (mut queue, entry_ids, item_ids) = queue_fixture();
    queue
        .set_traversal_current(item_ids[1])
        .expect("set first compound part");
    let (target, preview) = open_preview(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
    );
    assert_eq!(target, item_ids[2]);
    let before_ready = queue.revision_snapshot();
    let token = queue
        .prepare_manual_navigation(preview)
        .expect("Ready reserves exact next part");
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(item_ids[1])
    );
    assert_eq!(
        queue.remove(entry_ids[1]),
        RemoveItemOutcome::InstallCommitLinearizing
    );
    let preview = queue.abort_manual_navigation(token);
    assert_eq!(queue.revision_snapshot(), before_ready);
    assert_eq!(
        queue.traversal_current().map(|current| current.item_id()),
        Some(item_ids[1])
    );

    let token = queue
        .prepare_manual_navigation(preview)
        .expect("retry reuses exact part target");
    let committed = queue.commit_manual_navigation(token);
    assert_eq!(committed.traversal_current().item_id(), item_ids[2]);

    let mut random = StdRng::seed_from_u64(0x5a1e);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle for same-part reinstall");
    let reinstall = queue
        .prepare_reserved_mutation(
            queue.revision_snapshot(),
            ReservedQueueMutation::select_committed(item_ids[2]),
        )
        .expect("Ready reserves exact current group part");
    queue.commit_reserved(reinstall);
    let reinstalled = queue
        .shuffle_traversal_snapshot()
        .expect("same-part reinstall keeps shuffle enabled");
    assert_eq!(
        reinstalled.history(),
        &[item_ids[2], item_ids[2]],
        "exact Installed reinstall records one additional factual visit"
    );

    let (_stale_target, stale_preview) = open_preview(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
    );
    queue
        .append_entries(vec![PlaylistEntryDraft::Single(draft("late.mp3"))])
        .expect("structural change makes preview stale");
    let stale = queue
        .prepare_manual_navigation(stale_preview)
        .expect_err("stale preview cannot acquire reservation");
    assert!(matches!(
        stale.reason(),
        PrepareReservedMutationError::RevisionMismatch { .. }
    ));
}

#[test]
fn automatic_failures_attempt_remaining_parts_then_next_block_without_fake_visits() {
    let (mut queue, _entry_ids, item_ids) = queue_fixture();
    queue
        .set_traversal_current(item_ids[1])
        .expect("start at first compound part");
    let mut random = StdRng::seed_from_u64(0xa070);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");

    let (first_target, first_plan) = match queue.begin_automatic_traversal_with_rng(
        AutomaticEndedIntent::new(RepeatMode::StopAtEnd),
        false,
        &mut random,
    ) {
        AutomaticTraversalStart::OpenItem { item_id, plan } => (item_id, plan),
        _ => panic!("automatic traversal must target second compound part"),
    };
    assert_eq!(first_target, item_ids[2]);

    let (second_target, second_plan) =
        match queue.advance_automatic_traversal_after_failure_with_rng(*first_plan, &mut random) {
            AutomaticTraversalAdvance::OpenItem { item_id, plan } => (item_id, plan),
            AutomaticTraversalAdvance::AllFailed { .. } => {
                panic!("third compound part remains eligible")
            }
        };
    assert_eq!(second_target, item_ids[3]);

    let (next_block_target, next_block_plan) =
        match queue.advance_automatic_traversal_after_failure_with_rng(*second_plan, &mut random) {
            AutomaticTraversalAdvance::OpenItem { item_id, plan } => (item_id, plan),
            AutomaticTraversalAdvance::AllFailed { .. } => {
                panic!("another top-level block remains eligible")
            }
        };
    assert!(next_block_target == item_ids[0] || next_block_target == item_ids[4]);
    let token = queue
        .prepare_automatic_traversal(*next_block_plan)
        .expect("Ready reserves successful external block");
    queue.commit_automatic_traversal(token);

    let snapshot = queue
        .shuffle_traversal_snapshot()
        .expect("automatic success commits shuffle delta");
    assert_eq!(snapshot.history(), &[item_ids[1], next_block_target]);
    assert!(!snapshot.history().contains(&first_target));
    assert!(!snapshot.history().contains(&second_target));
}

#[test]
fn structural_mutations_keep_shuffle_entry_ids_valid_and_never_split_group() {
    let (mut queue, entry_ids, item_ids) = queue_fixture();
    queue
        .set_traversal_current(item_ids[2])
        .expect("current is middle group part");
    let mut random = StdRng::seed_from_u64(0x57ac7);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable shuffle");

    assert!(matches!(
        queue.sort_canonical(SortCanonicalQueue::new(
            PlaylistSortKey::NaturalFilename,
            SortDirection::Descending,
        )),
        SortCanonicalQueueOutcome::Reordered { .. }
    ));
    assert!(matches!(
        queue.move_item(entry_ids[1], MoveItemIntent::ToFront),
        MoveItemOutcome::Moved { entry_id } if entry_id == entry_ids[1]
    ));
    let discovery_revision = queue.revision_snapshot();
    let DiscoveryBatchInsertOutcome {
        item_ids: discovered,
        ..
    } = queue
        .insert_discovery_batch_with_rng(
            discovery_revision,
            StableInsertionAnchor::at_end(),
            vec![draft("discovered.mp3")],
            &mut random,
        )
        .expect("discovery inserts one whole Single entry");
    let discovered_item_id = *discovered
        .as_slice()
        .first()
        .expect("one discovered Item ID");
    assert!(matches!(
        queue.remove(PlaylistEntryId::Single(discovered_item_id)),
        RemoveItemOutcome::Removed { .. }
    ));

    let snapshot = queue
        .shuffle_traversal_snapshot()
        .expect("shuffle remains enabled");
    let committed_entry_ids = queue.iter_top_level_entry_ids().collect::<HashSet<_>>();
    let unique_upcoming = snapshot.upcoming().iter().copied().collect::<HashSet<_>>();
    assert_eq!(unique_upcoming.len(), snapshot.upcoming().len());
    assert!(unique_upcoming.is_subset(&committed_entry_ids));
    assert!(
        snapshot
            .history()
            .iter()
            .all(|item_id| queue.item(*item_id).is_some())
    );
    assert!(
        !snapshot
            .upcoming()
            .contains(&PlaylistEntryId::Single(item_ids[1]))
    );
    assert!(
        !snapshot
            .upcoming()
            .contains(&PlaylistEntryId::Single(item_ids[2]))
    );
    assert!(
        !snapshot
            .upcoming()
            .contains(&PlaylistEntryId::Single(item_ids[3]))
    );
    assert_eq!(
        queue
            .iter_playable_ids()
            .filter(|item_id| { [item_ids[1], item_ids[2], item_ids[3]].contains(item_id) })
            .collect::<Vec<_>>(),
        vec![item_ids[1], item_ids[2], item_ids[3]]
    );
}
