use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackState};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind,
};

use super::{RemovalUndoOutcome, RuntimeRemovalOutcome};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::media_open::{AuthorizationDispatchResolution, MediaOpenRequestId};
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, ControllerActiveMediaRebindOutcome,
    ControllerAppendOutcome, EndedSnapshotKind, InstallReadyOutcome, PlaylistInstallRequest,
};
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};
use crate::playlist_runtime::{PlaylistBindingGeneration, PlaylistRuntime};
use crate::process_shutdown::ShutdownDeadline;

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
}

fn draft(index: usize) -> PlaylistItemDraft {
    let label = format!("runtime-removal-{index}.mkv");
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(&label)),
        None,
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
    )
}

fn runtime_with_items(count: usize) -> (PlaylistRuntime, Vec<PlaylistItemId>) {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let ids = match runtime
        .controller
        .append((0..count).map(draft).collect())
        .expect("fixture append")
    {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
        ControllerAppendOutcome::NoItemsProvided => panic!("fixture must append rows"),
    };
    (runtime, ids)
}

fn install_active(runtime: &mut PlaylistRuntime, item_id: PlaylistItemId) -> ActiveMediaIdentity {
    runtime
        .controller
        .queue
        .set_traversal_current(item_id)
        .expect("fixture current");
    let active = ActiveMediaIdentity::installed(
        Some(item_id),
        ActiveMediaLineageId::from_non_zero(non_zero(301)),
        MediaInstanceId::from_non_zero(non_zero(401)),
        PlaylistBindingGeneration(501),
    );
    runtime.controller.active_media = Some(active);
    active
}

#[test]
fn deadline_is_inclusive_at_expiry_and_countdown_uses_monotonic_boundaries() {
    let (mut runtime, ids) = runtime_with_items(2);
    let now = Instant::now();
    assert!(matches!(
        runtime.remove_playlist_item(ids[0], now),
        RuntimeRemovalOutcome::Removed { .. }
    ));
    let initial = runtime.removal_undo_status(now).expect("undo available");
    assert_eq!(initial.seconds_remaining, 8);
    assert_eq!(initial.next_wake_deadline, now + Duration::from_secs(1));
    // Отделение от transport model не теряет ни presentation, ни runtime wake.
    let ui_snapshot = runtime.playlist_undo_ui_snapshot(now);
    assert_eq!(
        ui_snapshot
            .undo
            .expect("authoritative Undo UI model")
            .action_label(),
        "Отменить удаление элемента (8 с)"
    );
    assert_eq!(
        ui_snapshot.next_wake_deadline,
        Some(now + Duration::from_secs(1))
    );

    let before_deadline = now + Duration::from_millis(7_999);
    assert_eq!(
        runtime
            .removal_undo_status(before_deadline)
            .expect("one millisecond remains")
            .seconds_remaining,
        1
    );
    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(8)),
        RemovalUndoOutcome::Expired
    );
    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(9)),
        RemovalUndoOutcome::Unavailable
    );
}

#[test]
fn second_removal_replaces_slot_with_immediate_pre_mutation_state() {
    let (mut runtime, ids) = runtime_with_items(3);
    let now = Instant::now();
    let _first = runtime.remove_playlist_item(ids[0], now);
    let _second = runtime.remove_playlist_item(ids[1], now + Duration::from_secs(1));

    assert!(matches!(
        runtime.undo_last_removal(now + Duration::from_secs(2)),
        RemovalUndoOutcome::Restored { .. }
    ));
    assert!(runtime.controller.queue.item(ids[0]).is_none());
    assert!(runtime.controller.queue.item(ids[1]).is_some());
    assert!(runtime.controller.queue.item(ids[2]).is_some());
}

#[test]
fn undo_clear_restores_queue_but_not_pre_clear_tombstone_or_playback() {
    let (mut runtime, ids) = runtime_with_items(3);
    let active = install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _first = runtime.remove_playlist_item(ids[0], now);
    let _second = runtime.clear_playlist(now + Duration::from_secs(1));

    assert!(matches!(
        runtime.undo_last_removal(now + Duration::from_secs(2)),
        RemovalUndoOutcome::Restored {
            reattached_active: false,
            ..
        }
    ));
    assert!(runtime.controller.queue.item(ids[0]).is_none());
    assert!(runtime.controller.queue.item(ids[1]).is_some());
    assert!(runtime.controller.queue.item(ids[2]).is_some());
    assert!(runtime.controller.active_media().is_none());
    assert!(runtime.controller.detached_active_tombstone.is_none());
    assert_eq!(
        runtime
            .pending_media_reset_request()
            .map(|request| request.media_instance_id),
        Some(active.media_instance_id())
    );
}

#[test]
fn clear_and_remove_others_use_same_undo_and_restore_selection() {
    let now = Instant::now();
    let (mut clear_runtime, clear_ids) = runtime_with_items(4);
    clear_runtime.controller.select_row(Some(clear_ids[2]));
    assert!(matches!(
        clear_runtime.clear_playlist(now),
        RuntimeRemovalOutcome::Removed { .. }
    ));
    assert!(clear_runtime.controller.queue.is_empty());
    assert!(!clear_runtime.has_pending_media_reset());
    assert_eq!(
        clear_runtime
            .playlist_persistence_view()
            .latest_committed_revision
            .map(playlist_state::SaveRevision::value),
        Some(1)
    );
    assert!(matches!(
        clear_runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Restored {
            selected_item_id: Some(selected),
            ..
        } if selected == clear_ids[2]
    ));
    assert_eq!(clear_runtime.controller.queue.len(), 4);
    assert_eq!(
        clear_runtime
            .playlist_persistence_view()
            .latest_committed_revision
            .map(playlist_state::SaveRevision::value),
        Some(2)
    );

    let (mut others_runtime, others_ids) = runtime_with_items(4);
    others_runtime.controller.select_row(Some(others_ids[1]));
    assert!(matches!(
        others_runtime.remove_other_playlist_items(others_ids[1], now),
        RuntimeRemovalOutcome::Removed { .. }
    ));
    assert_eq!(others_runtime.controller.queue.len(), 1);
    assert!(matches!(
        others_runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Restored {
            selected_item_id: Some(selected),
            ..
        } if selected == others_ids[1]
    ));
    assert_eq!(others_runtime.controller.queue.len(), 4);
}

#[test]
fn undo_active_clear_restores_queue_current_and_selection_only() {
    let now = Instant::now();
    let (mut runtime, ids) = runtime_with_items(3);
    let active = install_active(&mut runtime, ids[1]);
    runtime.controller.select_row(Some(ids[2]));

    assert!(matches!(
        runtime.clear_playlist(now),
        RuntimeRemovalOutcome::Removed { .. }
    ));
    assert!(runtime.controller.queue.is_empty());
    assert!(runtime.controller.active_media().is_none());
    assert!(runtime.controller.detached_active_tombstone.is_none());
    assert!(
        runtime
            .playlist_interaction_model()
            .go_current_target
            .is_none()
    );
    assert_eq!(
        runtime
            .pending_media_reset_request()
            .map(|request| request.media_instance_id),
        Some(active.media_instance_id())
    );

    assert!(matches!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Restored {
            selected_item_id: Some(selected),
            reattached_active: false,
        } if selected == ids[2]
    ));
    assert_eq!(runtime.controller.queue.len(), 3);
    assert_eq!(
        runtime
            .controller
            .queue
            .traversal_current()
            .map(|current| current.item_id()),
        Some(ids[1])
    );
    assert!(runtime.controller.active_media().is_none());
    assert!(runtime.controller.detached_active_tombstone.is_none());
    assert_eq!(
        runtime
            .pending_media_reset_request()
            .map(|request| request.media_instance_id),
        Some(active.media_instance_id())
    );
}

#[test]
fn real_mutation_invalidates_but_selection_and_noop_preserve_slot() {
    let (mut runtime, ids) = runtime_with_items(3);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    let revision_after_removal = runtime
        .playlist_persistence_view()
        .latest_committed_revision;
    assert!(runtime.controller.select_row(Some(ids[2])));
    assert!(matches!(
        runtime.remove_playlist_item(ids[0], now + Duration::from_secs(1)),
        RuntimeRemovalOutcome::NotFound { .. }
    ));
    assert_eq!(
        runtime
            .playlist_persistence_view()
            .latest_committed_revision,
        revision_after_removal
    );
    assert!(
        runtime
            .removal_undo_status(now + Duration::from_secs(2))
            .is_some()
    );

    let _append = runtime
        .controller
        .append(vec![draft(9)])
        .expect("later real mutation");
    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(3)),
        RemovalUndoOutcome::Invalidated
    );
}

#[test]
fn explicit_mutation_invalidation_releases_metadata_salvage_snapshot_before_commit() {
    let (mut runtime, ids) = runtime_with_items(2);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    runtime.invalidate_removal_undo_for_persistent_mutation();

    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Unavailable
    );
}

#[test]
fn same_lineage_rebind_preserves_undo_while_new_lineage_invalidates_it() {
    let (mut runtime, ids) = runtime_with_items(3);
    let active = install_active(&mut runtime, ids[1]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[1], now);
    assert!(matches!(
        runtime.controller.rebind_active_media_same_lineage(
            active.detached(),
            MediaInstanceId::from_non_zero(non_zero(402)),
            PlaylistBindingGeneration(502),
        ),
        ControllerActiveMediaRebindOutcome::Rebound { .. }
    ));
    assert!(matches!(
        runtime.undo_last_removal(now + Duration::from_secs(1)),
        RemovalUndoOutcome::Restored {
            reattached_active: true,
            ..
        }
    ));

    let _removed_again = runtime.remove_playlist_item(ids[1], now + Duration::from_secs(2));
    runtime.controller.active_media = Some(ActiveMediaIdentity::installed(
        Some(ids[2]),
        ActiveMediaLineageId::from_non_zero(non_zero(999)),
        MediaInstanceId::from_non_zero(non_zero(1_000)),
        PlaylistBindingGeneration(1_001),
    ));
    assert_eq!(
        runtime.undo_last_removal(now + Duration::from_secs(3)),
        RemovalUndoOutcome::Invalidated
    );
}

#[test]
fn shutdown_releases_tombstone_and_undo_without_dirty_mutation() {
    let (mut runtime, ids) = runtime_with_items(2);
    let _active = install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    let dirty_before_shutdown = runtime.controller.dirty_revision();

    let _shutdown = runtime.shutdown_until(ShutdownDeadline::after(Duration::from_secs(1)));
    assert!(runtime.removal_undo.is_none());
    assert!(runtime.controller.detached_active_tombstone.is_none());
    assert_eq!(runtime.controller.dirty_revision(), dirty_before_shutdown);
}

#[test]
fn successful_new_lineage_install_releases_undo_slot_immediately() {
    let (mut runtime, ids) = runtime_with_items(3);
    let active = install_active(&mut runtime, ids[0]);
    let now = Instant::now();
    let _removed = runtime.remove_playlist_item(ids[0], now);
    let automatic = runtime.controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    let AutomaticLifecycleOutcome::OpenItem { install } = automatic else {
        panic!("tombstone continuation must select next row");
    };
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(1_101));
    let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(1_201));
    runtime
        .controller
        .accept_install_request(PlaylistInstallRequest {
            request_id,
            player_request_id,
            target_item_id: Some(install.item_id),
            origin: install.pending_origin,
            intent_revision: install.intent_revision,
            expected_queue_revision: install.expected_queue_revision,
            mutation: install.mutation,
        })
        .expect("automatic request accepted");
    assert!(matches!(
        runtime.controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    runtime
        .controller
        .begin_authorization_dispatch(request_id)
        .expect("dispatch begins");
    assert!(
        runtime
            .controller
            .resolve_authorization_dispatch(
                request_id,
                AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
            )
            .expect("enqueue resolution")
            .is_none()
    );
    runtime
        .on_playlist_installed(
            request_id,
            player_request_id,
            MediaInstanceId::from_non_zero(non_zero(1_301)),
            PlaylistBindingGeneration(1_401),
        )
        .expect("Installed commits next lineage");

    assert!(runtime.removal_undo.is_none());
    assert!(runtime.controller.detached_active_tombstone.is_none());
    assert_eq!(
        runtime
            .controller
            .queue
            .traversal_current()
            .map(|current| current.item_id()),
        Some(ids[1])
    );
}
