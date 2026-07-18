use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::SystemTime;

use player_core::{MediaInstanceId, PlaybackIntentRevision};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, LocalSourceFingerprint, PlaylistItemDraft,
    PlaylistMediaKind, PlaylistSortKey, SortCanonicalQueue, SortDirection,
};

use super::*;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::ControllerAppendOutcome;
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, ActiveMediaLineageId, PendingTarget, PendingTargetOrigin,
};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity must be non-zero")
}

fn local_draft(path: &str, title: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(path)),
        Some(LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH)),
        CachedPlaylistMetadata::new(path, PlaylistMediaKind::Audio)
            .with_title(Some(title.to_owned())),
    )
}

#[test]
fn reorder_keeps_selected_active_and_pending_bound_to_exact_item_ids() {
    let mut controller = PlaylistController::new();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(vec![
            local_draft("/music/c.flac", "Charlie"),
            local_draft("/music/a.flac", "Alpha"),
            local_draft("/music/b.flac", "Bravo"),
        ])
        .expect("fixture append must succeed")
    else {
        panic!("non-empty fixture must append rows");
    };

    controller.select_row(Some(item_ids[2]));
    controller
        .queue
        .commit_manual_play(item_ids[0])
        .expect("fixture current must commit");
    let active = ActiveMediaIdentity::installed(
        Some(item_ids[0]),
        ActiveMediaLineageId::from_non_zero(non_zero(11)),
        MediaInstanceId::from_non_zero(non_zero(12)),
        PlaylistBindingGeneration(13),
    );
    let pending = PendingTarget::new(
        MediaOpenRequestId::from_non_zero(non_zero(14)),
        Some(item_ids[1]),
        PendingTargetOrigin::ExplicitRowPlay,
        PlaybackIntentRevision::INITIAL,
    );
    controller.active_media = Some(active);
    controller.pending_target = Some(pending);

    let expected_structural_revision = controller.view_snapshot().structural_revision();
    let prepared = controller
        .queue()
        .canonical_sort_snapshot()
        .prepare(
            &[],
            SortCanonicalQueue::new(PlaylistSortKey::Title, SortDirection::Ascending),
            || false,
        )
        .expect("fixture preparation must not cancel");
    let commit = controller
        .preflight_canonical_sort(expected_structural_revision, prepared, Vec::new())
        .expect("matching sort must pass preflight");
    let outcome = controller.commit_canonical_sort(commit);

    assert!(outcome.domain.reordered());
    assert_eq!(controller.selected_item_id(), Some(item_ids[2]));
    assert_eq!(controller.active_media, Some(active));
    assert_eq!(controller.pending_target, Some(pending));
    assert_eq!(
        controller
            .queue
            .traversal_current()
            .map(|current| current.item_id()),
        Some(item_ids[0])
    );
}
