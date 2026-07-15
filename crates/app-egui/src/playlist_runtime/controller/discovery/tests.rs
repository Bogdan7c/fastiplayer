use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind,
    StableInsertionAnchor,
};

use super::*;
use crate::playlist_runtime::controller::{ControllerAppendOutcome, PlaylistController};

fn draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(name.into()),
        None,
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
    )
}

fn append_id(controller: &mut PlaylistController, name: &str) -> PlaylistItemId {
    match controller.append(vec![draft(name)]).expect("append") {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids[0],
        ControllerAppendOutcome::NoItemsProvided => panic!("one draft is not empty"),
    }
}

#[test]
fn accepted_batch_allocates_ids_only_with_natural_anchor_commit() {
    let mut controller = PlaylistController::new();
    let target_id = append_id(&mut controller, "target");
    let continuation = controller
        .begin_discovery_continuation()
        .expect("continuation");
    let watermark_before = controller.queue().next_item_id_snapshot();

    let committed = controller
        .commit_discovery_batch(
            continuation,
            StableInsertionAnchor::before(target_id),
            vec![draft("before-2"), draft("before-1")],
        )
        .expect("batch commit");

    assert_eq!(committed.item_ids.len(), 2);
    assert_eq!(controller.queue().items()[2].item_id(), target_id);
    assert_ne!(controller.queue().next_item_id_snapshot(), watermark_before);
    assert_eq!(committed.anchor.before_item_id(), Some(target_id));
}

#[test]
fn stale_continuation_and_external_edit_preserve_allocator() {
    let mut controller = PlaylistController::new();
    let target_id = append_id(&mut controller, "target");
    let continuation = controller
        .begin_discovery_continuation()
        .expect("continuation");
    let _external_id = append_id(&mut controller, "external");
    let watermark = controller.queue().next_item_id_snapshot();

    assert!(matches!(
        controller.commit_discovery_batch(
            continuation,
            StableInsertionAnchor::before(target_id),
            vec![draft("stale")],
        ),
        Err(DiscoveryBatchCommitError::ContinuationMismatch)
    ));
    assert_eq!(controller.queue().next_item_id_snapshot(), watermark);
}

#[test]
fn accepted_batch_advances_expected_revision_without_self_cancellation() {
    let mut controller = PlaylistController::new();
    let target_id = append_id(&mut controller, "target");
    let first = controller
        .begin_discovery_continuation()
        .expect("continuation");
    let first_commit = controller
        .commit_discovery_batch(
            first,
            StableInsertionAnchor::before(target_id),
            vec![draft("near")],
        )
        .expect("first batch");

    let second_commit = controller
        .commit_discovery_batch(
            first_commit.continuation,
            StableInsertionAnchor::before(first_commit.item_ids[0]),
            vec![draft("far")],
        )
        .expect("second batch");

    assert_eq!(second_commit.item_ids.len(), 1);
    assert_eq!(controller.queue().items()[2].item_id(), target_id);
}
