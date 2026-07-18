use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackIntentRevision};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind,
    PrepareReservedMutationError, RepeatMode, ReservedQueueMutation,
};

use super::*;
use crate::media_open::{
    AuthorizationDispatchResolution, MediaOpenClientKey, MediaOpenRequestId,
    PlayerDispatchRejection,
};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::install::{
    ControllerInstallPhase, ControllerMediaOpenCommandError, ControllerMediaOpenDisposition,
    DeferredControllerIntent, DeferredTransportIntent, DesiredQueueModes, InstallReadyOutcome,
    LifecycleIntentOutcome, PlaylistInstallAdmissionError, PlaylistInstallMutation,
    PlaylistInstallRequest,
};
use crate::playlist_runtime::identity::{
    PendingTargetOrigin, PlaylistItemErrorCategory, PlaylistItemErrorPhase, TransportActionOrigin,
};
use crate::playlist_runtime::view::PlaylistWorkerAvailability;

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap()
}

fn request_id(value: u64) -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(non_zero(value))
}

fn player_request_id(value: u64) -> MediaInstallRequestId {
    MediaInstallRequestId::from_non_zero(non_zero(value))
}

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(non_zero(value))
}

fn draft(index: usize) -> PlaylistItemDraft {
    let display_name = format!("track-{index}.mkv");
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(&display_name)),
        None,
        CachedPlaylistMetadata::new(display_name, PlaylistMediaKind::Video),
    )
}

fn append_ids(controller: &mut PlaylistController, count: usize) -> Vec<PlaylistItemId> {
    let drafts = (0..count).map(draft).collect();
    match controller.append(drafts).unwrap() {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
        ControllerAppendOutcome::NoItemsProvided => panic!("non-empty fixture must append"),
    }
}

fn install_request(
    controller: &PlaylistController,
    request_value: u64,
    player_request_value: u64,
    item_id: PlaylistItemId,
) -> PlaylistInstallRequest {
    PlaylistInstallRequest {
        request_id: request_id(request_value),
        player_request_id: player_request_id(player_request_value),
        target_item_id: Some(item_id),
        origin: PendingTargetOrigin::ExplicitRowPlay,
        intent_revision: PlaybackIntentRevision::INITIAL,
        expected_queue_revision: controller.queue().revision_snapshot(),
        mutation: PlaylistInstallMutation::Reserved(ReservedQueueMutation::select_committed(
            item_id,
        )),
    }
}

fn reserve_existing(
    controller: &mut PlaylistController,
    request_value: u64,
    player_request_value: u64,
    item_id: PlaylistItemId,
) {
    controller
        .accept_install_request(install_request(
            controller,
            request_value,
            player_request_value,
            item_id,
        ))
        .unwrap();
    assert_eq!(
        controller.on_ready_to_commit(request_id(request_value)),
        InstallReadyOutcome::RequestAuthorization {
            request_id: request_id(request_value)
        }
    );
}

#[test]
fn append_does_not_start_playback_and_visible_access_reuses_shared_rows() {
    let mut controller = PlaylistController::new();
    let item_ids = append_ids(&mut controller, 128);
    let structural_snapshot = controller.view_snapshot();
    let shared_rows_identity = structural_snapshot.shared_rows_identity();
    let shared_title_identity = structural_snapshot.shared_title_identity(73);

    assert_eq!(controller.active_media(), None);
    assert_eq!(controller.queue().traversal_current(), None);
    assert_eq!(structural_snapshot.len(), 128);
    assert_eq!(structural_snapshot.visible_rows(40..47).len(), 7);

    assert!(controller.select_row(Some(item_ids[73])));
    let selection_snapshot = controller.view_snapshot();
    assert_eq!(
        selection_snapshot.shared_rows_identity(),
        shared_rows_identity
    );
    assert_eq!(
        selection_snapshot.shared_title_identity(73),
        shared_title_identity
    );
    assert_eq!(selection_snapshot.selected_item_id(), Some(item_ids[73]));
    assert_eq!(selection_snapshot.active_media(), None);
    assert_eq!(selection_snapshot.pending_target(), None);
}

#[test]
fn remove_selected_uses_d47_fallback_and_clears_persisted_current_without_active() {
    let mut controller = PlaylistController::new();
    let item_ids = append_ids(&mut controller, 3);
    controller.select_row(Some(item_ids[1]));
    controller.queue.set_traversal_current(item_ids[1]).unwrap();

    let outcome = controller.remove_item(item_ids[1]);
    assert!(matches!(
        outcome,
        ControllerDestructiveRemovalOutcome::Removed(ref removal)
            if removal.selection_after.selected_cursor() == Some(item_ids[2])
                && matches!(
                    removal.current_outcome,
                    playlist_core::RemovalCurrentOutcome::Detached { removed_item_id }
                        if removed_item_id == item_ids[1]
                )
    ));
    assert_eq!(controller.queue().traversal_current(), None);
    assert_eq!(controller.active_media(), None);
    assert!(matches!(
        controller.remove_item(item_ids[1]),
        ControllerDestructiveRemovalOutcome::NotFound { .. }
    ));
}

#[test]
fn ready_reservation_failure_preserves_queue_allocator_and_dirty_state() {
    let mut controller = PlaylistController::new();
    let item_ids = append_ids(&mut controller, 2);
    let item_id = item_ids[0];
    let missing_id = item_ids[1];
    assert!(matches!(
        controller.remove_item(missing_id),
        ControllerDestructiveRemovalOutcome::Removed(_)
    ));
    let dirty_before = controller.dirty_revision();
    let watermark_before = controller.queue().next_item_id_snapshot();
    let request = PlaylistInstallRequest {
        mutation: PlaylistInstallMutation::Reserved(ReservedQueueMutation::select_committed(
            missing_id,
        )),
        target_item_id: Some(missing_id),
        ..install_request(&controller, 1, 11, item_id)
    };
    controller.accept_install_request(request).unwrap();

    assert_eq!(
        controller.on_ready_to_commit(request_id(1)),
        InstallReadyOutcome::ReservationRejected {
            request_id: request_id(1),
            error: PrepareReservedMutationError::ItemNotCommitted {
                item_id: missing_id
            }
        }
    );
    assert_eq!(controller.dirty_revision(), dirty_before);
    assert_eq!(controller.queue().next_item_id_snapshot(), watermark_before);
    assert_eq!(controller.queue().items().len(), 1);
    assert_eq!(controller.active_media(), None);
}

#[test]
fn ready_propagates_revision_mismatch_without_installing_reservation() {
    let mut controller = PlaylistController::new();
    let item_id = append_ids(&mut controller, 1)[0];
    let stale_revision = controller.queue().revision_snapshot();
    append_ids(&mut controller, 1);
    let request = PlaylistInstallRequest {
        expected_queue_revision: stale_revision,
        ..install_request(&controller, 2, 12, item_id)
    };
    controller.accept_install_request(request).unwrap();

    assert!(matches!(
        controller.on_ready_to_commit(request_id(2)),
        InstallReadyOutcome::ReservationRejected {
            error: PrepareReservedMutationError::RevisionMismatch { .. },
            ..
        }
    ));
    assert!(matches!(
        controller.append(vec![draft(8)]),
        Ok(ControllerAppendOutcome::Added { .. })
    ));
}

#[test]
fn coordinator_acceptance_is_distinct_from_barrier_and_delayed_resolution_keeps_token() {
    let mut controller = PlaylistController::new();
    let item_id = append_ids(&mut controller, 1)[0];
    reserve_existing(&mut controller, 3, 13, item_id);
    let dirty_before = controller.dirty_revision();

    controller
        .begin_authorization_dispatch(request_id(3))
        .unwrap();
    assert_eq!(
        controller.install_phase(),
        Some(ControllerInstallPhase::AuthorizationDispatchPending)
    );
    assert_eq!(
        controller.request_lifecycle_intent(DeferredControllerIntent::Transport(
            DeferredTransportIntent::Stop {
                origin: TransportActionOrigin::Ui,
            },
        )),
        Ok(LifecycleIntentOutcome::AwaitAuthorizationResolution {
            request_id: request_id(3)
        })
    );
    assert!(matches!(
        controller.append(vec![draft(2)]),
        Err(ControllerAppendError::Domain(
            playlist_core::AddItemsError::InstallCommitLinearizing
        ))
    ));
    assert_eq!(controller.dirty_revision(), dirty_before);
}

#[test]
fn cancel_win_and_downstream_rejections_abort_without_watermark_burn() {
    let resolutions = [
        AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
            cause: player_core::MediaInstallCancellationCause::TransportStop,
        },
        AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
            rejection: PlayerDispatchRejection::Backpressure,
        },
        AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
            rejection: PlayerDispatchRejection::Disconnected,
        },
    ];
    for (index, resolution) in resolutions.into_iter().enumerate() {
        let mut controller = PlaylistController::new();
        let item_id = append_ids(&mut controller, 1)[0];
        let dirty_before = controller.dirty_revision();
        let watermark_before = controller.queue().next_item_id_snapshot();
        reserve_existing(
            &mut controller,
            10 + index as u64,
            20 + index as u64,
            item_id,
        );
        controller
            .begin_authorization_dispatch(request_id(10 + index as u64))
            .unwrap();
        let drain = controller
            .resolve_authorization_dispatch(request_id(10 + index as u64), resolution)
            .unwrap()
            .unwrap();

        assert_eq!(drain.active_media, None);
        assert_eq!(controller.dirty_revision(), dirty_before);
        assert_eq!(controller.queue().next_item_id_snapshot(), watermark_before);
        assert!(controller.view_snapshot().structural_actions_enabled());
    }
}

#[test]
fn enqueue_win_requires_exact_installed_and_commits_new_lineage_once() {
    let mut controller = PlaylistController::new();
    let item_id = append_ids(&mut controller, 1)[0];
    reserve_existing(&mut controller, 30, 40, item_id);
    controller
        .begin_authorization_dispatch(request_id(30))
        .unwrap();
    assert_eq!(
        controller.resolve_authorization_dispatch(
            request_id(30),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        ),
        Ok(None)
    );
    assert_eq!(
        controller.install_phase(),
        Some(ControllerInstallPhase::AuthorizationInFlight)
    );

    let drain = controller
        .on_installed(
            request_id(30),
            player_request_id(40),
            media_instance_id(50),
            PlaylistBindingGeneration(6),
        )
        .unwrap();
    let active = drain.active_media.unwrap();
    assert_eq!(active.item_id(), Some(item_id));
    assert_eq!(active.lineage_id().get(), 1);
    assert_eq!(
        controller.queue().traversal_current().unwrap().item_id(),
        item_id
    );
    assert!(drain.dirty.is_some());
    assert_eq!(
        controller.on_installed(
            request_id(30),
            player_request_id(40),
            media_instance_id(50),
            PlaylistBindingGeneration(6),
        ),
        Err(PlaylistControllerInvariantViolation::MissingInstalledTerminal)
    );
}

#[test]
fn replacement_ids_and_rows_stay_private_until_exact_installed_commit() {
    let mut controller = PlaylistController::new();
    let old_item = append_ids(&mut controller, 1)[0];
    let old_watermark = controller.queue().next_item_id_snapshot();
    let old_structural_revision = controller.view_snapshot().structural_revision();
    let request = PlaylistInstallRequest {
        request_id: request_id(31),
        player_request_id: player_request_id(41),
        target_item_id: None,
        origin: PendingTargetOrigin::ExplicitOpen,
        intent_revision: PlaybackIntentRevision::INITIAL,
        expected_queue_revision: controller.queue().revision_snapshot(),
        mutation: PlaylistInstallMutation::Reserved(ReservedQueueMutation::replace_with_current(
            vec![draft(10)],
            draft(11),
            vec![draft(12)],
        )),
    };
    controller.accept_install_request(request).unwrap();
    assert!(matches!(
        controller.on_ready_to_commit(request_id(31)),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    assert_eq!(controller.queue().items()[0].item_id(), old_item);
    assert_eq!(controller.queue().next_item_id_snapshot(), old_watermark);
    assert_eq!(controller.view_snapshot().len(), 1);

    controller
        .begin_authorization_dispatch(request_id(31))
        .unwrap();
    controller
        .resolve_authorization_dispatch(
            request_id(31),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .unwrap();
    controller
        .on_installed(
            request_id(31),
            player_request_id(41),
            media_instance_id(51),
            PlaylistBindingGeneration(2),
        )
        .unwrap();

    assert_eq!(controller.queue().len(), 3);
    assert_eq!(controller.view_snapshot().len(), 3);
    assert!(controller.view_snapshot().structural_revision() > old_structural_revision);
    assert_ne!(controller.queue().next_item_id_snapshot(), old_watermark);
}

#[test]
fn mismatched_player_terminal_is_fatal_and_keeps_structural_lock() {
    let mut controller = PlaylistController::new();
    let item_id = append_ids(&mut controller, 1)[0];
    reserve_existing(&mut controller, 32, 42, item_id);
    controller
        .begin_authorization_dispatch(request_id(32))
        .unwrap();
    controller
        .resolve_authorization_dispatch(
            request_id(32),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .unwrap();

    assert_eq!(
        controller.on_installed(
            request_id(32),
            player_request_id(999),
            media_instance_id(52),
            PlaylistBindingGeneration(2),
        ),
        Err(PlaylistControllerInvariantViolation::PlayerRequestMismatch)
    );
    assert!(matches!(
        controller.append(vec![draft(3)]),
        Err(ControllerAppendError::FatalInvariant)
    ));
}

#[test]
fn modes_drain_after_abort_and_commit_with_runtime_generation_not_dirty_by_itself() {
    let mut aborting = PlaylistController::new();
    let abort_item = append_ids(&mut aborting, 1)[0];
    reserve_existing(&mut aborting, 60, 70, abort_item);
    let dirty_before_modes = aborting.dirty_revision();
    assert_eq!(
        aborting.request_queue_modes(DesiredQueueModes {
            repeat_mode: RepeatMode::RepeatQueue,
            shuffle_enabled: false,
            protected_runtime_generation: 41,
        }),
        Ok(None)
    );
    assert_eq!(
        aborting.request_lifecycle_intent(DeferredControllerIntent::Suspend),
        Ok(LifecycleIntentOutcome::Immediate {
            intent: DeferredControllerIntent::Suspend,
            aborted_request_id: Some(request_id(60)),
            cancellation_cause: Some(
                player_core::MediaInstallCancellationCause::LifecycleSuspended,
            ),
            mode_dirty: aborting.latest_dirty_signal(),
        })
    );
    assert_eq!(aborting.repeat_mode, RepeatMode::RepeatQueue);
    assert_eq!(aborting.protected_modes_generation, 41);
    assert!(aborting.dirty_revision() > dirty_before_modes);

    let dirty_after_persistent_modes = aborting.dirty_revision();
    assert_eq!(
        aborting.request_queue_modes(DesiredQueueModes {
            repeat_mode: RepeatMode::RepeatQueue,
            shuffle_enabled: false,
            protected_runtime_generation: 42,
        }),
        Ok(None)
    );
    assert_eq!(aborting.protected_modes_generation, 42);
    assert_eq!(aborting.dirty_revision(), dirty_after_persistent_modes);

    let mut committing = PlaylistController::new();
    let commit_item = append_ids(&mut committing, 1)[0];
    reserve_existing(&mut committing, 61, 71, commit_item);
    committing
        .begin_authorization_dispatch(request_id(61))
        .unwrap();
    committing
        .request_queue_modes(DesiredQueueModes {
            repeat_mode: RepeatMode::RepeatOne,
            shuffle_enabled: true,
            protected_runtime_generation: 99,
        })
        .unwrap();
    committing
        .resolve_authorization_dispatch(
            request_id(61),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .unwrap();
    committing
        .on_installed(
            request_id(61),
            player_request_id(71),
            media_instance_id(81),
            PlaylistBindingGeneration(1),
        )
        .unwrap();
    assert_eq!(committing.repeat_mode, RepeatMode::RepeatOne);
    assert!(committing.queue().shuffle_enabled());
    assert_eq!(committing.protected_modes_generation, 99);
}

#[test]
fn lifecycle_intents_are_bounded_in_every_install_phase() {
    let mut awaiting = PlaylistController::new();
    let awaiting_item = append_ids(&mut awaiting, 1)[0];
    awaiting
        .accept_install_request(install_request(&awaiting, 90, 100, awaiting_item))
        .unwrap();
    assert!(matches!(
        awaiting.request_lifecycle_intent(DeferredControllerIntent::Transport(
            DeferredTransportIntent::Stop {
                origin: TransportActionOrigin::Ui,
            },
        )),
        Ok(LifecycleIntentOutcome::CancelPendingRequest { .. })
    ));

    let mut dispatching = PlaylistController::new();
    let dispatch_item = append_ids(&mut dispatching, 1)[0];
    reserve_existing(&mut dispatching, 91, 101, dispatch_item);
    dispatching
        .begin_authorization_dispatch(request_id(91))
        .unwrap();
    dispatching
        .request_lifecycle_intent(DeferredControllerIntent::Transport(
            DeferredTransportIntent::Stop {
                origin: TransportActionOrigin::Ui,
            },
        ))
        .unwrap();
    dispatching
        .request_lifecycle_intent(DeferredControllerIntent::Shutdown)
        .unwrap();
    let drain = dispatching
        .resolve_authorization_dispatch(
            request_id(91),
            AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                cause: player_core::MediaInstallCancellationCause::LifecycleShutdown,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        drain.deferred_intent,
        Some(DeferredControllerIntent::Shutdown)
    );

    let mut in_flight = PlaylistController::new();
    let in_flight_item = append_ids(&mut in_flight, 1)[0];
    reserve_existing(&mut in_flight, 92, 102, in_flight_item);
    in_flight
        .begin_authorization_dispatch(request_id(92))
        .unwrap();
    in_flight
        .resolve_authorization_dispatch(
            request_id(92),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .unwrap();
    assert_eq!(
        in_flight.request_lifecycle_intent(DeferredControllerIntent::Suspend),
        Ok(LifecycleIntentOutcome::AwaitInstalled {
            request_id: request_id(92)
        })
    );
}

#[test]
fn missing_resolution_and_missing_installed_are_fatal_without_token_abort() {
    let mut controller = PlaylistController::new();
    let item_id = append_ids(&mut controller, 1)[0];
    reserve_existing(&mut controller, 110, 120, item_id);
    controller
        .begin_authorization_dispatch(request_id(110))
        .unwrap();
    assert_eq!(
        controller.report_missing_authorization_resolution(request_id(110)),
        PlaylistControllerInvariantViolation::MissingAuthorizationResolution
    );
    assert!(matches!(
        controller.append(vec![draft(2)]),
        Err(ControllerAppendError::FatalInvariant)
    ));

    let mut installed_missing = PlaylistController::new();
    let item_id = append_ids(&mut installed_missing, 1)[0];
    reserve_existing(&mut installed_missing, 111, 121, item_id);
    installed_missing
        .begin_authorization_dispatch(request_id(111))
        .unwrap();
    installed_missing
        .resolve_authorization_dispatch(
            request_id(111),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .unwrap();
    assert_eq!(
        installed_missing.report_terminal_without_installed(request_id(111)),
        PlaylistControllerInvariantViolation::MissingInstalledTerminal
    );
}

#[test]
fn d49_badge_correlation_and_d70_retention_do_not_dirty_queue() {
    let mut controller = PlaylistController::new();
    let item_ids = append_ids(&mut controller, 2);
    let dirty_before_errors = controller.dirty_revision();
    controller
        .accept_install_request(install_request(&controller, 130, 140, item_ids[0]))
        .unwrap();
    assert_eq!(
        controller.record_request_error(
            item_ids[0],
            request_id(130),
            PlaylistItemErrorPhase::Preparation,
            PlaylistItemErrorCategory::Unavailable,
            Arc::from("источник временно недоступен"),
        ),
        RuntimeErrorCorrelationOutcome::Recorded
    );
    assert_eq!(
        controller.record_request_error(
            item_ids[1],
            request_id(130),
            PlaylistItemErrorPhase::Install,
            PlaylistItemErrorCategory::Rejected,
            Arc::from("stale"),
        ),
        RuntimeErrorCorrelationOutcome::StaleRequest
    );
    assert_eq!(
        controller.mark_committed_source_unavailable(item_ids[1], Arc::from("файл не найден"),),
        RuntimeErrorCorrelationOutcome::Recorded
    );
    assert_eq!(controller.queue().len(), 2);
    assert_eq!(controller.dirty_revision(), dirty_before_errors);
    let rows = controller.view_snapshot().visible_rows(0..2);
    assert!(rows[0].runtime_error().is_some());
    assert!(rows[0].is_pending());
    assert!(rows[1].runtime_error().is_some());
}

#[test]
fn active_removal_detaches_identity_and_keeps_selection_independent() {
    let mut controller = PlaylistController::new();
    let item_ids = append_ids(&mut controller, 2);
    controller.select_row(Some(item_ids[1]));
    reserve_existing(&mut controller, 150, 160, item_ids[0]);
    controller
        .begin_authorization_dispatch(request_id(150))
        .unwrap();
    controller
        .resolve_authorization_dispatch(
            request_id(150),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .unwrap();
    controller
        .on_installed(
            request_id(150),
            player_request_id(160),
            media_instance_id(170),
            PlaylistBindingGeneration(3),
        )
        .unwrap();

    assert_eq!(controller.selected_item_id(), Some(item_ids[1]));
    let installed_rows = controller.view_snapshot().visible_rows(0..2);
    assert!(installed_rows[0].is_active());
    assert!(!installed_rows[0].is_selected());
    assert!(installed_rows[1].is_selected());
    assert!(!installed_rows[1].is_active());
    assert!(matches!(
        controller.remove_item(item_ids[0]),
        ControllerDestructiveRemovalOutcome::Removed(_)
    ));
    assert!(
        controller
            .active_media()
            .is_some_and(|active| active.item_id().is_none())
    );
    assert!(controller.view_snapshot().has_active_tombstone());
}

#[test]
fn policy_commands_are_opaque_and_worker_unavailable_is_typed() {
    let mut controller = PlaylistController::new();
    let client = MediaOpenClientKey::from_non_zero(non_zero(1));
    assert!(matches!(
        controller.media_open_command(client, ControllerMediaOpenDisposition::Start),
        Ok(ControllerMediaOpenCommand::Start { .. })
    ));
    assert!(matches!(
        controller.media_open_command(client, ControllerMediaOpenDisposition::Coalesce),
        Ok(ControllerMediaOpenCommand::Coalesce { .. })
    ));
    assert!(matches!(
        controller.media_open_command(
            client,
            ControllerMediaOpenDisposition::Supersede {
                expected_request_id: request_id(1)
            }
        ),
        Ok(ControllerMediaOpenCommand::Supersede { .. })
    ));

    controller.set_worker_availability(PlaylistWorkerAvailability::Unavailable);
    assert_eq!(
        controller.media_open_command(client, ControllerMediaOpenDisposition::Start),
        Err(ControllerMediaOpenCommandError::WorkerUnavailable)
    );
    assert_eq!(
        controller.view_snapshot().worker_availability(),
        PlaylistWorkerAvailability::Unavailable
    );
}

#[test]
fn coalesce_and_pre_ready_supersede_keep_exact_single_pending_request() {
    let mut controller = PlaylistController::new();
    let item_ids = append_ids(&mut controller, 2);
    controller
        .accept_install_request(install_request(&controller, 180, 190, item_ids[0]))
        .unwrap();
    assert_eq!(
        controller.confirm_coalesced_install_request(request_id(180)),
        Ok(())
    );
    assert_eq!(
        controller.confirm_coalesced_install_request(request_id(181)),
        Err(PlaylistInstallAdmissionError::StaleSupersede)
    );

    controller
        .supersede_install_request_before_ready(
            request_id(180),
            install_request(&controller, 181, 191, item_ids[1]),
        )
        .unwrap();
    assert_eq!(
        controller
            .view_snapshot()
            .pending_target()
            .unwrap()
            .request_id(),
        request_id(181)
    );
    assert_eq!(
        controller.confirm_coalesced_install_request(request_id(180)),
        Err(PlaylistInstallAdmissionError::StaleSupersede)
    );
}
