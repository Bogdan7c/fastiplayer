use std::path::PathBuf;

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::{
    AddItemsOutcome, AutomaticEndedIntent, AutomaticTraversalAdvance, AutomaticTraversalStart,
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
    RepeatMode, ShuffleToggleOutcome,
};

fn draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(name)),
        None,
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
    )
}

fn queue_with_current(names: &[&str], current_index: usize) -> PlaylistQueue {
    let mut queue = PlaylistQueue::new();
    let AddItemsOutcome::Added(ids) = queue
        .append_batch(names.iter().map(|name| draft(name)).collect())
        .expect("append succeeds")
    else {
        panic!("batch is non-empty");
    };
    let ids = ids.into_vec();
    queue
        .set_traversal_current(ids[current_index])
        .expect("current revision remains available");
    queue
}

#[test]
fn fixed_error_chain_excludes_late_admission_and_skips_removed_member_without_replacement() {
    let mut queue = queue_with_current(&["a", "b", "c"], 0);
    let original_ids: Vec<_> = queue.items().iter().map(|item| item.item_id()).collect();
    let AutomaticTraversalStart::OpenItem {
        item_id: first_target,
        plan,
    } = queue.begin_automatic_error_traversal(AutomaticEndedIntent::new(RepeatMode::RepeatQueue))
    else {
        panic!("error skip must select the next committed row");
    };
    assert_eq!(first_target, original_ids[1]);

    let AddItemsOutcome::Added(late_ids) = queue
        .append_one(draft("late"))
        .expect("late append succeeds")
    else {
        panic!("one row was appended");
    };
    let late_id = late_ids.as_slice()[0];
    let _removed = queue.remove(first_target);

    let failure = queue
        .prepare_automatic_traversal(*plan)
        .expect_err("removed target must fail current reservation revalidation");
    let plan = failure.into_plan();

    let AutomaticTraversalAdvance::OpenItem {
        item_id: second_target,
        plan,
    } = queue.advance_automatic_traversal_after_failure(plan)
    else {
        panic!("removed snapshot member must not replace the remaining original target");
    };
    assert_eq!(second_target, original_ids[2]);
    assert_ne!(second_target, late_id);
    assert!(matches!(
        queue.advance_automatic_traversal_after_failure(*plan),
        AutomaticTraversalAdvance::AllFailed { attempted_count: 3 }
    ));
}

#[test]
fn shuffle_repeat_queue_generated_cycle_commits_through_opaque_token() {
    let mut queue = queue_with_current(&["a", "b"], 0);
    let mut random = StdRng::seed_from_u64(7);
    assert!(matches!(
        queue
            .enable_shuffle_with_rng(&mut random)
            .expect("shuffle revision remains available"),
        ShuffleToggleOutcome::Enabled
    ));

    let AutomaticTraversalStart::OpenItem { plan, .. } = queue.begin_automatic_traversal_with_rng(
        AutomaticEndedIntent::new(RepeatMode::StopAtEnd),
        false,
        &mut random,
    ) else {
        panic!("first shuffle cycle has one upcoming row");
    };
    let token = queue
        .prepare_automatic_traversal(*plan)
        .expect("first automatic plan prepares");
    let first_commit = queue.commit_automatic_traversal(token);

    let AutomaticTraversalStart::OpenItem {
        item_id: wrapped_target,
        plan,
    } = queue.begin_automatic_traversal_with_rng(
        AutomaticEndedIntent::new(RepeatMode::RepeatQueue),
        false,
        &mut random,
    )
    else {
        panic!("RepeatQueue must create an opaque new shuffle cycle");
    };
    assert_ne!(wrapped_target, first_commit.traversal_current().item_id());
    let token = queue
        .prepare_automatic_traversal(*plan)
        .expect("generated cycle plan prepares");
    let wrapped_commit = queue.commit_automatic_traversal(token);
    assert_eq!(wrapped_commit.traversal_current().item_id(), wrapped_target);
    assert_eq!(
        queue.traversal_current(),
        Some(wrapped_commit.traversal_current())
    );
}
