use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;

use player_core::{
    ExactMediaTransportAction, MediaInstallCancellationCause, MediaInstallRequestId,
    MediaInstanceId, PlaybackIntentRevision, PlaybackState,
};
use playlist_core::{
    AddPlaylistEntriesOutcome, CachedPlaylistMetadata, LocalLocator, PlaylistCompoundGroupDraft,
    PlaylistEntryDraft, PlaylistEntryId, PlaylistItemDraft, PlaylistItemId, PlaylistLocator,
    PlaylistMediaKind, RemovalCurrentOutcome, RepeatMode, ReservedQueueMutation,
};

use super::{
    ControllerActiveMediaRebindOutcome, ControllerDestructiveRemovalOutcome,
    ControllerRemovalUndoOutcome,
};
use crate::media_open::{AuthorizationDispatchResolution, MediaOpenRequestId};
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, ControllerRemovalKind,
    EndedSnapshotKind, InstallReadyOutcome, PlaylistController, PlaylistInstallMutation,
    PlaylistInstallRequest,
};
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, ActiveMediaLineageId, PendingTargetOrigin,
};
use crate::playlist_runtime::{PlaylistBindingGeneration, controller::ControllerAppendOutcome};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
}

fn draft(index: usize) -> PlaylistItemDraft {
    let label = format!("removal-{index}.mkv");
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(&label)),
        None,
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
    )
}

fn controller_with_active(
    count: usize,
    current_index: usize,
) -> (PlaylistController, Vec<PlaylistItemId>, ActiveMediaIdentity) {
    let mut controller = PlaylistController::new();
    let ids = match controller
        .append((0..count).map(draft).collect())
        .expect("fixture append")
    {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
        ControllerAppendOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    controller
        .queue
        .set_traversal_current(ids[current_index])
        .expect("fixture current");
    let active = ActiveMediaIdentity::installed(
        Some(ids[current_index]),
        ActiveMediaLineageId::from_non_zero(non_zero(71)),
        MediaInstanceId::from_non_zero(non_zero(81)),
        PlaylistBindingGeneration(91),
    );
    controller.active_media = Some(active);
    controller.publish_view(false);
    (controller, ids, active)
}

fn removed(outcome: ControllerDestructiveRemovalOutcome) -> super::ControllerDestructiveRemoval {
    match outcome {
        ControllerDestructiveRemovalOutcome::Removed(removal) => *removal,
        other => panic!("expected removal, got {other:?}"),
    }
}

#[test]
fn selected_removal_uses_explicit_compound_identity_and_commits_full_group() {
    let mut controller = PlaylistController::new();
    let compound = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Local(LocalLocator::Native(PathBuf::from("album"))),
        CachedPlaylistMetadata::new("album", PlaylistMediaKind::Audio),
        vec![draft(1), draft(2)],
    )
    .expect("compound requires parts");
    let AddPlaylistEntriesOutcome::Added(allocated) = controller
        .queue
        .append_entries(vec![
            PlaylistEntryDraft::Single(draft(0)),
            PlaylistEntryDraft::Compound(compound),
        ])
        .expect("mixed append")
    else {
        panic!("fixture append is non-empty");
    };
    let entry_ids = allocated.iter_entry_ids().collect::<Vec<_>>();
    let item_ids = allocated.iter_playable_item_ids().collect::<Vec<_>>();
    let revision = controller.structural_revision;
    assert!(matches!(
        controller.remove_item(item_ids[1]),
        ControllerDestructiveRemovalOutcome::CompoundPartTarget {
            part_item_id,
            compound_entry_id,
        } if part_item_id == item_ids[1] && compound_entry_id == entry_ids[1]
    ));
    assert_eq!(
        controller.update_selection(crate::playlist_runtime::UpdateSelection::Replace {
            entry_id: entry_ids[1],
            structural_revision: revision,
        }),
        crate::playlist_runtime::UpdateSelectionOutcome::Updated
    );

    assert!(matches!(
        controller.remove_selected_items(Arc::from([entry_ids[1]]), revision),
        ControllerDestructiveRemovalOutcome::Removed(_)
    ));
    assert_eq!(
        controller.queue.iter_playable_ids().collect::<Vec<_>>(),
        [item_ids[0]]
    );
}

fn install_request(
    request_value: u64,
    player_request_value: u64,
    install: crate::playlist_runtime::controller::PlannedPlaylistInstall,
) -> PlaylistInstallRequest {
    PlaylistInstallRequest {
        request_id: MediaOpenRequestId::from_non_zero(non_zero(request_value)),
        player_request_id: MediaInstallRequestId::from_non_zero(non_zero(player_request_value)),
        target_item_id: Some(install.item_id),
        origin: install.pending_origin,
        intent_revision: install.intent_revision,
        expected_queue_revision: install.expected_queue_revision,
        mutation: install.mutation,
    }
}

#[test]
fn active_remove_detaches_identity_clears_persisted_current_and_preserves_player_instance() {
    let (mut controller, ids, active_before) = controller_with_active(3, 1);
    controller.select_row(Some(ids[1]));

    let removal = removed(controller.remove_item(ids[1]));

    assert_eq!(
        removal.current_outcome,
        RemovalCurrentOutcome::Detached {
            removed_item_id: ids[1]
        }
    );
    assert!(controller.queue.traversal_current().is_none());
    assert!(controller.queue.item(ids[1]).is_none());
    assert_eq!(controller.selected_item_id(), Some(ids[2]));
    let detached = controller.active_media().expect("detached active remains");
    assert_eq!(detached.item_id(), None);
    assert_eq!(detached.lineage_id(), active_before.lineage_id());
    assert_eq!(
        detached.media_instance_id(),
        active_before.media_instance_id()
    );
    assert_eq!(
        detached.player_binding_generation(),
        active_before.player_binding_generation()
    );
}

#[test]
fn playing_and_paused_observations_keep_tombstone_without_navigation() {
    for playback_state in [PlaybackState::Playing, PlaybackState::Paused] {
        let (mut controller, ids, active) = controller_with_active(3, 1);
        let _removal = removed(controller.remove_item(ids[1]));
        assert!(matches!(
            controller.observe_automatic_snapshot(
                active.player_binding_generation(),
                Some(active.media_instance_id()),
                playback_state,
                EndedSnapshotKind::Clean,
                AutomaticDeferredAvailability::Unavailable,
            ),
            AutomaticLifecycleOutcome::NoAction
        ));
        assert!(controller.detached_active_tombstone.is_some());
        assert!(
            controller
                .active_media()
                .is_some_and(|identity| identity.item_id().is_none())
        );
    }
}

#[test]
fn matching_lineage_undo_reattaches_after_same_lineage_new_player_instance() {
    let (mut controller, ids, active_before) = controller_with_active(3, 1);
    controller.select_row(Some(ids[1]));
    let removal = removed(controller.remove_item(ids[1]));
    let rebound_instance = MediaInstanceId::from_non_zero(non_zero(181));
    assert!(matches!(
        controller.rebind_active_media_same_lineage(
            active_before.detached(),
            rebound_instance,
            PlaylistBindingGeneration(191),
        ),
        ControllerActiveMediaRebindOutcome::Rebound { .. }
    ));

    let outcome = controller.restore_destructive_removal(removal);
    assert!(matches!(
        outcome,
        ControllerRemovalUndoOutcome::Restored {
            selected_entry_id: Some(selected),
            reattached_active: true,
            ..
        } if selected == PlaylistEntryId::Single(ids[1])
    ));
    let reattached = controller.active_media().expect("reattached active");
    assert_eq!(reattached.item_id(), Some(ids[1]));
    assert_eq!(reattached.media_instance_id(), rebound_instance);
    assert_eq!(
        controller.queue.traversal_current().map(|id| id.item_id()),
        Some(ids[1])
    );
}

#[test]
fn same_lineage_rebind_rejects_stale_old_instance_and_binding() {
    let (mut controller, ids, active_before) = controller_with_active(2, 0);
    let _removal = removed(controller.remove_item(ids[0]));
    let detached_before = active_before.detached();
    let first_rebound = controller.rebind_active_media_same_lineage(
        detached_before,
        MediaInstanceId::from_non_zero(non_zero(182)),
        PlaylistBindingGeneration(192),
    );
    assert!(matches!(
        first_rebound,
        ControllerActiveMediaRebindOutcome::Rebound { .. }
    ));

    let stale_rebound = controller.rebind_active_media_same_lineage(
        detached_before,
        MediaInstanceId::from_non_zero(non_zero(183)),
        PlaylistBindingGeneration(193),
    );
    assert!(matches!(
        stale_rebound,
        ControllerActiveMediaRebindOutcome::Stale {
            current_active_media: Some(current),
        } if current.media_instance_id() == MediaInstanceId::from_non_zero(non_zero(182))
            && current.player_binding_generation() == PlaylistBindingGeneration(192)
    ));
}

#[test]
fn every_repeat_mode_continues_tombstone_without_replaying_removed_item() {
    for repeat_mode in [
        RepeatMode::StopAtEnd,
        RepeatMode::RepeatQueue,
        RepeatMode::RepeatOne,
    ] {
        let (mut controller, ids, active) = controller_with_active(3, 0);
        controller.repeat_mode = repeat_mode;
        let _removal = removed(controller.remove_item(ids[0]));

        let outcome = controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        );
        assert!(matches!(
            outcome,
            AutomaticLifecycleOutcome::OpenItem { install } if install.item_id == ids[1]
        ));
    }
}

#[test]
fn continuation_revalidation_skips_later_removed_target() {
    let (mut controller, ids, active) = controller_with_active(4, 0);
    let _active_removal = removed(controller.remove_item(ids[0]));
    let _later_removal = removed(controller.remove_item(ids[1]));

    let outcome = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    assert!(matches!(
        outcome,
        AutomaticLifecycleOutcome::OpenItem { install } if install.item_id == ids[2]
    ));
}

#[test]
fn successful_continuation_installed_sets_current_and_releases_old_tombstone() {
    let (mut controller, ids, active) = controller_with_active(3, 0);
    let _active_removal = removed(controller.remove_item(ids[0]));
    let outcome = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    let AutomaticLifecycleOutcome::OpenItem { install } = outcome else {
        panic!("tombstone must continue to the next committed row");
    };
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(601));
    let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(701));
    controller
        .accept_install_request(install_request(601, 701, install))
        .expect("automatic request accepted");
    assert!(matches!(
        controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id)
        .expect("authorization dispatch begins");
    assert!(
        controller
            .resolve_authorization_dispatch(
                request_id,
                AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
            )
            .expect("enqueue resolution")
            .is_none()
    );
    let installed = controller
        .on_installed(
            request_id,
            player_request_id,
            MediaInstanceId::from_non_zero(non_zero(801)),
            PlaylistBindingGeneration(901),
        )
        .expect("continuation Installed commits");

    assert_eq!(
        controller
            .queue
            .traversal_current()
            .map(|current| current.item_id()),
        Some(ids[1])
    );
    assert_eq!(
        installed
            .active_media
            .and_then(ActiveMediaIdentity::item_id),
        Some(ids[1])
    );
    assert!(controller.detached_active_tombstone.is_none());
}

#[test]
fn failed_continuation_retains_detached_active_identity_and_tombstone() {
    let (mut controller, ids, active) = controller_with_active(3, 0);
    let _active_removal = removed(controller.remove_item(ids[0]));
    let outcome = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    let AutomaticLifecycleOutcome::OpenItem { install } = outcome else {
        panic!("tombstone must produce continuation candidate");
    };
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(611));
    controller
        .accept_install_request(install_request(611, 711, install))
        .expect("automatic request accepted");
    let _failure = controller
        .report_automatic_target_failure(request_id, Arc::from("candidate preparation failed"));

    assert!(controller.detached_active_tombstone.is_some());
    let detached = controller.active_media().expect("old active remains");
    assert_eq!(detached.item_id(), None);
    assert_eq!(detached.lineage_id(), active.lineage_id());
    assert_eq!(detached.media_instance_id(), active.media_instance_id());
}

#[test]
fn cancelled_continuation_retains_detached_active_identity_and_tombstone() {
    let (mut controller, ids, active) = controller_with_active(3, 0);
    let _active_removal = removed(controller.remove_item(ids[0]));
    let automatic = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    let AutomaticLifecycleOutcome::OpenItem { install } = automatic else {
        panic!("tombstone must produce continuation candidate");
    };
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(612));
    controller
        .accept_install_request(install_request(612, 712, install))
        .expect("automatic request accepted");
    assert!(matches!(
        controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id)
        .expect("authorization dispatch begins");
    let terminal = controller
        .resolve_authorization_dispatch(
            request_id,
            AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                cause: MediaInstallCancellationCause::UserCancelled,
            },
        )
        .expect("cancel winner is authoritative");

    assert!(terminal.is_some());
    assert!(controller.detached_active_tombstone.is_some());
    let detached = controller.active_media().expect("old active remains");
    assert_eq!(detached.item_id(), None);
    assert_eq!(detached.lineage_id(), active.lineage_id());
    assert_eq!(detached.media_instance_id(), active.media_instance_id());
}

#[test]
fn removal_retires_pre_ready_target_and_undo_does_not_resurrect_request() {
    let (mut controller, ids, _active) = controller_with_active(3, 0);
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(1_501));
    let expected_queue_revision = controller.queue.revision_snapshot();
    controller
        .accept_install_request(PlaylistInstallRequest {
            request_id,
            player_request_id: MediaInstallRequestId::from_non_zero(non_zero(1_601)),
            target_item_id: Some(ids[2]),
            origin: PendingTargetOrigin::ExplicitRowPlay,
            intent_revision: PlaybackIntentRevision::INITIAL,
            expected_queue_revision,
            mutation: PlaylistInstallMutation::Reserved(ReservedQueueMutation::select_committed(
                ids[2],
            )),
        })
        .expect("pre-Ready request accepted");

    let removal = removed(controller.remove_item(ids[2]));
    assert_eq!(removal.pending_request_to_cancel, Some(request_id));
    assert!(controller.install_phase().is_none());
    assert!(controller.pending_target.is_none());
    assert!(matches!(
        controller.restore_destructive_removal(removal),
        ControllerRemovalUndoOutcome::Restored { .. }
    ));
    assert!(controller.install_phase().is_none());
    assert!(controller.pending_target.is_none());
}

#[test]
fn removal_is_blocked_after_ready_reservation_and_does_not_abort_token() {
    let (mut controller, ids, _active) = controller_with_active(3, 0);
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(1_701));
    let expected_queue_revision = controller.queue.revision_snapshot();
    controller
        .accept_install_request(PlaylistInstallRequest {
            request_id,
            player_request_id: MediaInstallRequestId::from_non_zero(non_zero(1_801)),
            target_item_id: Some(ids[2]),
            origin: PendingTargetOrigin::ExplicitRowPlay,
            intent_revision: PlaybackIntentRevision::INITIAL,
            expected_queue_revision,
            mutation: PlaylistInstallMutation::Reserved(ReservedQueueMutation::select_committed(
                ids[2],
            )),
        })
        .expect("request accepted");
    assert!(matches!(
        controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));

    assert!(matches!(
        controller.remove_item(ids[1]),
        ControllerDestructiveRemovalOutcome::InstallCommitLinearizing
    ));
    assert!(controller.queue.item(ids[1]).is_some());
    assert!(controller.install_linearizing());
}

#[test]
fn clear_resets_active_media_without_tombstone_for_every_repeat_mode() {
    for repeat_mode in [
        RepeatMode::StopAtEnd,
        RepeatMode::RepeatQueue,
        RepeatMode::RepeatOne,
    ] {
        let (mut controller, _ids, active) = controller_with_active(3, 1);
        controller.repeat_mode = repeat_mode;
        let clear = removed(controller.clear_queue());
        assert_eq!(
            clear.media_reset_request,
            Some(player_core::ExactMediaTransportRequest {
                media_instance_id: active.media_instance_id(),
                action: ExactMediaTransportAction::ResetMedia,
            })
        );
        assert!(controller.detached_active_tombstone.is_none());
        assert!(controller.active_media().is_none());
        assert!(!controller.view_snapshot().has_active_tombstone());
        let ended_after_clear = controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        );
        assert!(!matches!(
            ended_after_clear,
            AutomaticLifecycleOutcome::OpenItem { .. }
        ));
    }
}

#[test]
fn clear_resets_non_playlist_active_identity_without_creating_tombstone() {
    let (mut controller, _ids, playlist_active) = controller_with_active(2, 0);
    let external_active = ActiveMediaIdentity::installed(
        None,
        ActiveMediaLineageId::from_non_zero(non_zero(2_001)),
        MediaInstanceId::from_non_zero(non_zero(2_101)),
        PlaylistBindingGeneration(2_201),
    );
    controller.active_media = Some(external_active);

    let clear = removed(controller.clear_queue());
    assert_eq!(
        clear.media_reset_request,
        Some(player_core::ExactMediaTransportRequest {
            media_instance_id: external_active.media_instance_id(),
            action: ExactMediaTransportAction::ResetMedia,
        })
    );
    assert_eq!(controller.active_media(), None);
    assert!(controller.detached_active_tombstone.is_none());
    assert_ne!(external_active.lineage_id(), playlist_active.lineage_id());
}

#[test]
fn clear_without_active_media_creates_no_reset_request() {
    let (mut controller, _ids, _active) = controller_with_active(2, 0);
    controller.active_media = None;

    let clear = removed(controller.clear_queue());

    assert_eq!(clear.media_reset_request, None);
    assert!(controller.active_media().is_none());
    assert!(controller.detached_active_tombstone.is_none());
}

#[test]
fn d47_remove_others_and_clear_selection_never_start_playback() {
    let (mut controller, ids, active) = controller_with_active(4, 2);
    controller.select_row(Some(ids[1]));
    let _remove_others = removed(controller.remove_other_items(ids[1]));
    assert_eq!(controller.selected_item_id(), Some(ids[1]));
    assert_eq!(controller.active_media(), Some(active.detached()));

    let clear = removed(controller.clear_queue());
    assert_eq!(controller.selected_item_id(), None);
    assert_eq!(
        clear
            .media_reset_request
            .map(|request| request.media_instance_id),
        Some(active.media_instance_id())
    );
    assert!(controller.active_media().is_none());
    assert!(controller.detached_active_tombstone.is_none());
}

#[test]
fn d47_remove_first_middle_last_and_only_choose_same_index_previous_or_none() {
    for (count, removed_index, expected_index) in [
        (3, 0, Some(1)),
        (3, 1, Some(2)),
        (3, 2, Some(1)),
        (1, 0, None),
    ] {
        let (mut controller, ids, _active) = controller_with_active(count, 0);
        controller.active_media = None;
        controller.select_row(Some(ids[removed_index]));
        let removal = removed(controller.remove_item(ids[removed_index]));
        let expected_selection = expected_index.map(|index| ids[index]);
        assert_eq!(
            removal.selection_after.interaction_cursor(),
            expected_selection.map(playlist_core::PlaylistEntryId::Single)
        );
        assert_eq!(controller.selected_item_id(), expected_selection);
        assert!(controller.active_media().is_none());
    }
}

#[test]
fn bulk_selected_removal_is_one_commit_and_undo_restores_full_selection() {
    let (mut controller, ids, _active) = controller_with_active(5, 2);
    let entry_ids = ids
        .iter()
        .copied()
        .map(playlist_core::PlaylistEntryId::Single)
        .collect::<Vec<_>>();
    let revision = controller.structural_revision;
    assert_eq!(
        controller.update_selection(crate::playlist_runtime::UpdateSelection::ReplaceRange {
            entry_ids: Arc::from([entry_ids[1], entry_ids[2], entry_ids[3]]),
            range_anchor: entry_ids[1],
            interaction_cursor: entry_ids[3],
            structural_revision: revision,
        }),
        crate::playlist_runtime::UpdateSelectionOutcome::Updated
    );
    assert_eq!(
        controller.update_selection(crate::playlist_runtime::UpdateSelection::MoveCursor {
            entry_id: entry_ids[2],
            structural_revision: revision,
        }),
        crate::playlist_runtime::UpdateSelectionOutcome::Updated
    );
    let dirty_before = controller.dirty_revision();

    let removal = removed(controller.remove_selected_items(
        Arc::from([entry_ids[1], entry_ids[2], entry_ids[3]]),
        revision,
    ));
    assert_eq!(removal.kind, ControllerRemovalKind::RemoveSelected);
    assert_eq!(
        controller.queue().iter_playable_ids().collect::<Vec<_>>(),
        vec![ids[0], ids[4]]
    );
    assert_eq!(controller.selected_item_id(), Some(ids[4]));
    assert_eq!(
        controller.dirty_revision(),
        dirty_before.checked_next().expect("one dirty revision")
    );

    assert!(matches!(
        controller.restore_destructive_removal(removal),
        ControllerRemovalUndoOutcome::Restored {
            selected_entry_id: Some(selected_entry_id),
            reattached_active: true,
            ..
        } if selected_entry_id == PlaylistEntryId::Single(ids[2])
    ));
    let restored_selection = controller.view_snapshot();
    assert_eq!(restored_selection.selection().selected_count(), 3);
    assert!(restored_selection.selection().is_selected(entry_ids[1]));
    assert!(restored_selection.selection().is_selected(entry_ids[2]));
    assert!(restored_selection.selection().is_selected(entry_ids[3]));
}

#[test]
fn bulk_unselected_removal_preserves_selected_active_current_and_rejects_stale_capture() {
    let (mut controller, ids, active) = controller_with_active(5, 2);
    let entry_ids = ids
        .iter()
        .copied()
        .map(playlist_core::PlaylistEntryId::Single)
        .collect::<Vec<_>>();
    let revision = controller.structural_revision;
    assert_eq!(
        controller.update_selection(crate::playlist_runtime::UpdateSelection::ReplaceRange {
            entry_ids: Arc::from([entry_ids[1], entry_ids[2], entry_ids[3]]),
            range_anchor: entry_ids[1],
            interaction_cursor: entry_ids[3],
            structural_revision: revision,
        }),
        crate::playlist_runtime::UpdateSelectionOutcome::Updated
    );
    assert_eq!(
        controller.update_selection(crate::playlist_runtime::UpdateSelection::MoveCursor {
            entry_id: entry_ids[2],
            structural_revision: revision,
        }),
        crate::playlist_runtime::UpdateSelectionOutcome::Updated
    );

    let removal = removed(
        controller.remove_unselected_items(Arc::from([entry_ids[0], entry_ids[4]]), revision),
    );
    assert_eq!(removal.kind, ControllerRemovalKind::RemoveUnselected);
    assert_eq!(controller.active_media(), Some(active));
    assert_eq!(
        controller
            .queue()
            .traversal_current()
            .map(|current| current.item_id()),
        Some(ids[2])
    );
    let selection = controller.view_snapshot();
    assert_eq!(selection.selection().selected_count(), 3);
    assert_eq!(
        selection.selection().interaction_cursor(),
        Some(entry_ids[2])
    );

    assert!(matches!(
        controller.remove_unselected_items(Arc::from([entry_ids[1]]), revision,),
        ControllerDestructiveRemovalOutcome::StaleStructuralRevision
    ));
}
