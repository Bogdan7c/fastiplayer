use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackState};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, RepeatMode,
};

use super::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, AutomaticStopCause,
    AutomaticTargetFailureOutcome, EndedSnapshotKind, PlaylistErrorBehavior,
};
use crate::media_open::AuthorizationDispatchResolution;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::{
    ControllerManualNavigationOutcome, InstallReadyOutcome, ManualNavigationCancelOutcome,
    ManualNavigationFailureOutcome, ManualNavigationTerminalAction, PlannedPlaylistInstall,
    PlaylistController, PlaylistInstallRequest, PreviousRestartThreshold,
};
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, ActiveMediaLineageId, TransportActionOrigin,
};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
}

fn draft(index: usize) -> PlaylistItemDraft {
    let label = format!("track-{index}.mkv");
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(&label)),
        None,
        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
    )
}

fn controller_with_active(
    count: usize,
    current_index: usize,
) -> (
    PlaylistController,
    Vec<playlist_core::PlaylistItemId>,
    ActiveMediaIdentity,
) {
    let mut controller = PlaylistController::new();
    let ids = match controller
        .append((0..count).map(draft).collect())
        .expect("append succeeds")
    {
        crate::playlist_runtime::controller::ControllerAppendOutcome::Added {
            item_ids, ..
        } => item_ids,
        _ => panic!("test append is non-empty"),
    };
    controller
        .queue
        .set_traversal_current(ids[current_index])
        .expect("current revision remains available");
    let active = ActiveMediaIdentity::installed(
        Some(ids[current_index]),
        ActiveMediaLineageId::from_non_zero(non_zero(71)),
        MediaInstanceId::from_non_zero(non_zero(81)),
        PlaylistBindingGeneration(91),
    );
    controller.active_media = Some(active);
    (controller, ids, active)
}

fn install_request(
    request_value: u64,
    player_request_value: u64,
    install: PlannedPlaylistInstall,
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

fn planned_manual_next(controller: &mut PlaylistController) -> PlannedPlaylistInstall {
    let outcome = controller.manual_navigation(
        playlist_core::ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        std::time::Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).expect("zero is valid"),
        super::super::transport::DiscoveryManualWaitAvailability::Exhausted,
    );
    let ControllerManualNavigationOutcome::StartInstall { install } = outcome else {
        panic!("next committed row starts manual install");
    };
    install
}

#[test]
fn playing_draining_ended_is_one_edge_and_rearms_after_replay_state() {
    let (mut controller, ids, active) = controller_with_active(2, 0);
    for state in [PlaybackState::Playing, PlaybackState::Draining] {
        assert!(matches!(
            controller.observe_automatic_snapshot(
                active.player_binding_generation(),
                Some(active.media_instance_id()),
                state,
                EndedSnapshotKind::Clean,
                AutomaticDeferredAvailability::Unavailable,
            ),
            AutomaticLifecycleOutcome::NoAction
        ));
    }
    let AutomaticLifecycleOutcome::OpenItem { install } = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    ) else {
        panic!("first Ended edge must advance");
    };
    assert_eq!(install.item_id, ids[1]);
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::NoAction
    ));
    let _rearmed = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Playing,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::OpenItem { .. }
    ));
}

#[test]
fn stale_binding_or_instance_cannot_consume_new_active_edge() {
    let (mut controller, _, active) = controller_with_active(1, 0);
    assert!(matches!(
        controller.observe_automatic_snapshot(
            PlaylistBindingGeneration(90),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::StaleObservation
    ));
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(MediaInstanceId::from_non_zero(non_zero(82))),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::StaleObservation
    ));
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::Stop { .. }
    ));
}

#[test]
fn repeat_one_replays_clean_ended_but_stops_error_associated_ended() {
    let (mut controller, _, active) = controller_with_active(1, 0);
    controller.repeat_mode = RepeatMode::RepeatOne;
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::ReplayCurrent { .. }
    ));
    let _rearmed = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Playing,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::ErrorAssociated {
                safe_summary: Arc::from("decode failed"),
            },
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::Stop {
            cause: AutomaticStopCause::RepeatOneError,
            ..
        }
    ));
}

#[test]
fn failed_snapshot_is_edge_triggered_error_policy_not_clean_eof() {
    let (mut controller, _, active) = controller_with_active(2, 0);
    controller.set_error_behavior(PlaylistErrorBehavior::Skip);
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Failed,
            EndedSnapshotKind::ErrorAssociated {
                safe_summary: Arc::from("renderer failed"),
            },
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::OpenItem { .. }
    ));
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Failed,
            EndedSnapshotKind::ErrorAssociated {
                safe_summary: Arc::from("renderer failed again"),
            },
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::NoAction
    ));
}

#[test]
fn d42_manual_failure_keeps_hold_and_d56_cancel_consumes_it_as_stop() {
    let (mut controller, _, active) = controller_with_active(2, 0);
    let install = planned_manual_next(&mut controller);
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(111));
    controller
        .accept_install_request(install_request(111, 211, install))
        .expect("manual request accepted");
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::HeldForExplicitIntent { .. }
    ));
    assert!(matches!(
        controller.report_manual_navigation_target_failure(request_id),
        ManualNavigationFailureOutcome::AwaitingUserAfterFailure { .. }
    ));
    let cancellation = controller.cancel_manual_navigation();
    assert!(matches!(
        cancellation,
        ManualNavigationCancelOutcome::Discarded(invalidation)
            if invalidation.terminal_action == ManualNavigationTerminalAction::StopEndedOrigin
    ));
    assert!(matches!(
        controller.reevaluate_held_ended(AutomaticDeferredAvailability::Unavailable),
        AutomaticLifecycleOutcome::NoAction
    ));
}

#[test]
fn d50_exhaustion_reevaluates_matching_held_ended_exactly_once() {
    let (mut controller, _, active) = controller_with_active(1, 0);
    let scope_id = super::super::transport::SiblingDiscoveryScopeId::from_non_zero(non_zero(401));
    let waiting = controller.manual_navigation(
        playlist_core::ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        std::time::Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).expect("zero is valid"),
        super::super::transport::DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    );
    let ControllerManualNavigationOutcome::Waiting { wait_id, .. } = waiting else {
        panic!("D50 must wait while discovery may produce a candidate");
    };
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::HeldForExplicitIntent { .. }
    ));
    assert!(matches!(
        controller.resume_manual_navigation_wait(wait_id, scope_id, true),
        ControllerManualNavigationOutcome::NoItem(_)
    ));
    assert!(matches!(
        controller.reevaluate_held_ended(AutomaticDeferredAvailability::Unavailable),
        AutomaticLifecycleOutcome::Stop {
            cause: AutomaticStopCause::Domain(_),
            ..
        }
    ));
    assert!(matches!(
        controller.reevaluate_held_ended(AutomaticDeferredAvailability::Unavailable),
        AutomaticLifecycleOutcome::NoAction
    ));
}

#[test]
fn d57_structural_invalidation_consumes_ended_hold_with_one_mutation_revision() {
    let (mut controller, _, active) = controller_with_active(2, 0);
    let install = planned_manual_next(&mut controller);
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(112));
    controller
        .accept_install_request(install_request(112, 212, install))
        .expect("manual request accepted");
    let _held = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    assert!(matches!(
        controller.report_manual_navigation_target_failure(request_id),
        ManualNavigationFailureOutcome::AwaitingUserAfterFailure { .. }
    ));
    let dirty_before = controller.dirty_revision().get();
    let appended = controller
        .append(vec![draft(7)])
        .expect("structural append succeeds");
    let crate::playlist_runtime::controller::ControllerAppendOutcome::Added {
        manual_navigation_invalidation: Some(invalidation),
        ..
    } = appended
    else {
        panic!("D57 must report typed invalidation");
    };
    assert_eq!(
        invalidation.terminal_action,
        ManualNavigationTerminalAction::StopEndedOrigin
    );
    assert_eq!(controller.dirty_revision().get(), dirty_before + 1);
    assert!(matches!(
        controller.reevaluate_held_ended(AutomaticDeferredAvailability::Unavailable),
        AutomaticLifecycleOutcome::NoAction
    ));
}

#[test]
fn dispatch_enqueue_win_commits_b_before_old_snapshot_can_act_again() {
    let (mut controller, ids, active) = controller_with_active(2, 0);
    let install = planned_manual_next(&mut controller);
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(114));
    let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(214));
    controller
        .accept_install_request(install_request(114, 214, install))
        .expect("manual request accepted");
    let _held = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    );
    assert!(matches!(
        controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id)
        .expect("dispatch begins");
    assert!(
        controller
            .resolve_authorization_dispatch(
                request_id,
                AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
            )
            .expect("enqueue resolution is valid")
            .is_none()
    );
    let installed = controller
        .on_installed(
            request_id,
            player_request_id,
            MediaInstanceId::from_non_zero(non_zero(314)),
            PlaylistBindingGeneration(91),
        )
        .expect("exact Installed commits B");
    assert_eq!(
        installed
            .active_media
            .and_then(ActiveMediaIdentity::item_id),
        Some(ids[1])
    );
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::Unavailable,
        ),
        AutomaticLifecycleOutcome::StaleObservation
    ));
}

#[test]
fn skip_chain_snapshot_excludes_late_row_and_keeps_failed_badge() {
    let (mut controller, ids, active) = controller_with_active(2, 0);
    controller.set_error_behavior(PlaylistErrorBehavior::Skip);
    let AutomaticLifecycleOutcome::OpenItem { install } = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::ErrorAssociated {
            safe_summary: Arc::from("runtime failed"),
        },
        AutomaticDeferredAvailability::Unavailable,
    ) else {
        panic!("skip policy must select B");
    };
    let request = install_request(101, 201, install);
    controller
        .accept_install_request(request)
        .expect("automatic request is accepted");
    let late_ids = match controller
        .append(vec![draft(3)])
        .expect("late append succeeds")
    {
        crate::playlist_runtime::controller::ControllerAppendOutcome::Added {
            item_ids, ..
        } => item_ids,
        _ => panic!("late append is non-empty"),
    };
    assert!(matches!(
        controller.report_automatic_target_failure(
            MediaOpenRequestId::from_non_zero(non_zero(101)),
            Arc::from("source unavailable"),
        ),
        AutomaticTargetFailureOutcome::Stopped {
            cause: AutomaticStopCause::AllCandidatesFailed { attempted_count: 2 }
        }
    ));
    assert_ne!(ids[1], late_ids[0]);
    let failed_row = controller
        .view_snapshot()
        .visible_rows(0..3)
        .into_iter()
        .find(|row| row.item_id() == ids[1])
        .expect("failed row remains committed");
    assert!(failed_row.runtime_error().is_some());
    assert_eq!(controller.queue().len(), 3);
}

#[test]
fn late_admission_does_not_invalidate_ready_or_join_automatic_plan() {
    let (mut controller, ids, active) = controller_with_active(2, 0);
    let AutomaticLifecycleOutcome::OpenItem { install } = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    ) else {
        panic!("clean Ended selects B");
    };
    let request_id = MediaOpenRequestId::from_non_zero(non_zero(151));
    let player_request_id = MediaInstallRequestId::from_non_zero(non_zero(251));
    controller
        .accept_install_request(install_request(151, 251, install))
        .expect("automatic request accepted");
    let _late = controller
        .append(vec![draft(9)])
        .expect("late admission commits");
    let dirty_after_late_admission = controller.dirty_revision().get();
    assert!(matches!(
        controller.on_ready_to_commit(request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id)
        .expect("dispatch begins");
    controller
        .resolve_authorization_dispatch(
            request_id,
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue resolution is valid");
    controller
        .on_installed(
            request_id,
            player_request_id,
            MediaInstanceId::from_non_zero(non_zero(351)),
            PlaylistBindingGeneration(91),
        )
        .expect("automatic target commits");
    assert_eq!(
        controller
            .queue()
            .traversal_current()
            .map(|current| current.item_id()),
        Some(ids[1])
    );
    assert_eq!(
        controller.dirty_revision().get(),
        dirty_after_late_admission + 1
    );
    assert_eq!(controller.queue().len(), 3);
}

#[test]
fn deferred_automatic_cancel_is_terminal() {
    let (mut controller, _, active) = controller_with_active(1, 0);
    let scope_id = super::super::transport::SiblingDiscoveryScopeId::from_non_zero(non_zero(301));
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::MayProduceCandidate { scope_id },
        ),
        AutomaticLifecycleOutcome::Deferred { .. }
    ));
    assert!(matches!(
        controller.cancel_deferred_automatic_advance(),
        AutomaticLifecycleOutcome::Stop {
            cause: AutomaticStopCause::DeferredCancelled,
            ..
        }
    ));
}

#[test]
fn manual_navigation_replaces_d26_latch_without_hidden_automatic_replay() {
    let (mut controller, _, active) = controller_with_active(1, 0);
    let scope_id = super::super::transport::SiblingDiscoveryScopeId::from_non_zero(non_zero(501));
    assert!(matches!(
        controller.observe_automatic_snapshot(
            active.player_binding_generation(),
            Some(active.media_instance_id()),
            PlaybackState::Ended,
            EndedSnapshotKind::Clean,
            AutomaticDeferredAvailability::MayProduceCandidate { scope_id },
        ),
        AutomaticLifecycleOutcome::Deferred { .. }
    ));
    assert!(matches!(
        controller.manual_navigation(
            playlist_core::ManualNavigationDirection::Next,
            TransportActionOrigin::Ui,
            std::time::Duration::ZERO,
            PreviousRestartThreshold::from_milliseconds(0).expect("zero is valid"),
            super::super::transport::DiscoveryManualWaitAvailability::MayProduceCandidate {
                scope_id,
            },
        ),
        ControllerManualNavigationOutcome::Waiting { .. }
    ));
    assert!(matches!(
        controller.cancel_deferred_automatic_advance(),
        AutomaticLifecycleOutcome::NoAction
    ));
}
