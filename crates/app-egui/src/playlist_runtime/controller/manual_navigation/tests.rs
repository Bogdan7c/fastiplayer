use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use player_core::{MediaInstallCancellationCause, MediaInstallRequestId, MediaInstanceId};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, ManualNavigationDirection, PlaylistItemDraft,
    PlaylistItemId, PlaylistMediaKind,
};

use super::super::*;
use crate::media_open::{
    AuthorizationDispatchResolution, MediaOpenRequestId, PlayerDispatchRejection,
};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, ActiveMediaLineageId, TransportActionOrigin,
};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity must be non-zero")
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
    let display_name = format!("cursor-{index}.mkv");
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(&display_name)),
        None,
        CachedPlaylistMetadata::new(display_name, PlaylistMediaKind::Video),
    )
}

fn append_items(controller: &mut PlaylistController, count: usize) -> Vec<PlaylistItemId> {
    match controller
        .append((0..count).map(draft).collect())
        .expect("append fixture")
    {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
        ControllerAppendOutcome::NoItemsProvided => panic!("non-empty fixture must append"),
    }
}

fn install_active(
    controller: &mut PlaylistController,
    item_id: PlaylistItemId,
    identity: u64,
) -> ActiveMediaIdentity {
    controller
        .queue
        .commit_manual_play(item_id)
        .expect("fixture current");
    let active = ActiveMediaIdentity::installed(
        Some(item_id),
        ActiveMediaLineageId::from_non_zero(non_zero(identity)),
        media_instance_id(identity),
        PlaylistBindingGeneration(1),
    );
    controller.active_media = Some(active);
    active
}

fn navigation(
    controller: &mut PlaylistController,
    direction: ManualNavigationDirection,
) -> ControllerManualNavigationOutcome {
    controller.manual_navigation(
        direction,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).expect("zero threshold"),
        DiscoveryManualWaitAvailability::Exhausted,
    )
}

fn request_from_plan(
    request_value: u64,
    player_request_value: u64,
    install: PlannedPlaylistInstall,
) -> PlaylistInstallRequest {
    PlaylistInstallRequest {
        request_id: request_id(request_value),
        player_request_id: player_request_id(player_request_value),
        target_item_id: Some(install.item_id),
        origin: install.pending_origin,
        intent_revision: install.intent_revision,
        expected_queue_revision: install.expected_queue_revision,
        mutation: install.mutation,
    }
}

fn accept_plan(
    controller: &mut PlaylistController,
    request_value: u64,
    player_request_value: u64,
    install: PlannedPlaylistInstall,
) {
    controller
        .accept_install_request(request_from_plan(
            request_value,
            player_request_value,
            install,
        ))
        .expect("manual plan admission");
}

fn enqueue_and_install(
    controller: &mut PlaylistController,
    request_value: u64,
    player_request_value: u64,
) -> ControllerTerminalDrain {
    assert!(matches!(
        controller.on_ready_to_commit(request_id(request_value)),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id(request_value))
        .expect("dispatch start");
    controller
        .resolve_authorization_dispatch(
            request_id(request_value),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue resolution");
    controller
        .on_installed(
            request_id(request_value),
            player_request_id(player_request_value),
            media_instance_id(player_request_value + 1),
            PlaylistBindingGeneration(1),
        )
        .expect("exact Installed")
}

#[test]
fn a_b_c_supersede_commits_only_latest_target_once() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 3);
    install_active(&mut controller, items[0], 10);
    let dirty_before = controller.dirty_revision();

    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B must start install")
    };
    assert_eq!(install.item_id, items[1]);
    accept_plan(&mut controller, 11, 21, install);

    let ControllerManualNavigationOutcome::SupersedeInstall {
        expected_request_id,
        cause,
        install,
    } = navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("B -> C must supersede one pending request")
    };
    assert_eq!(expected_request_id, request_id(11));
    assert_eq!(cause, MediaInstallCancellationCause::Superseded);
    assert_eq!(install.item_id, items[2]);
    assert_eq!(controller.dirty_revision(), dirty_before);
    assert_eq!(
        controller.queue.traversal_current().unwrap().item_id(),
        items[0]
    );

    controller
        .supersede_install_request_before_ready(request_id(11), request_from_plan(12, 22, install))
        .expect("exact latest-only replacement");
    let drain = enqueue_and_install(&mut controller, 12, 22);
    assert_eq!(drain.resolution, ControllerTerminalResolution::Installed);
    assert_eq!(
        controller.queue.traversal_current().unwrap().item_id(),
        items[2]
    );
    assert_eq!(controller.dirty_revision().get(), dirty_before.get() + 1);
}

#[test]
fn backtrack_to_active_before_dispatch_discards_preview_without_dirty_commit() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 2);
    install_active(&mut controller, items[0], 30);
    let dirty_before = controller.dirty_revision();
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut controller, 31, 41, install);

    let ControllerManualNavigationOutcome::AbortedBeforeDispatch {
        request_id: aborted_request_id,
        cause,
        next,
        no_item: Some(_),
    } = navigation(&mut controller, ManualNavigationDirection::Previous)
    else {
        panic!("backtrack to A must cancel exact request")
    };
    assert_eq!(aborted_request_id, request_id(31));
    assert_eq!(cause, MediaInstallCancellationCause::Superseded);
    assert!(next.is_none());
    assert_eq!(
        controller.queue.traversal_current().unwrap().item_id(),
        items[0]
    );
    assert_eq!(controller.dirty_revision(), dirty_before);
}

#[test]
fn dispatch_cancel_winner_recovers_preview_while_enqueue_winner_uses_new_active() {
    let mut cancel_controller = PlaylistController::new();
    let cancel_items = append_items(&mut cancel_controller, 3);
    install_active(&mut cancel_controller, cancel_items[0], 50);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut cancel_controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut cancel_controller, 51, 61, install);
    cancel_controller.on_ready_to_commit(request_id(51));
    cancel_controller
        .begin_authorization_dispatch(request_id(51))
        .expect("dispatch");
    assert!(matches!(
        navigation(&mut cancel_controller, ManualNavigationDirection::Next),
        ControllerManualNavigationOutcome::Guarded(
            TransportGuardOutcome::AwaitAuthorizationResolution { .. }
        )
    ));
    let cancel_drain = cancel_controller
        .resolve_authorization_dispatch(
            request_id(51),
            AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                cause: MediaInstallCancellationCause::Superseded,
            },
        )
        .expect("cancel winner")
        .expect("terminal drain");
    let DeferredControllerIntent::Transport(intent) = cancel_drain
        .deferred_intent
        .expect("latest cursor step survives cancel winner")
    else {
        panic!("transport intent")
    };
    let DeferredTransportExecutionOutcome::Navigation(
        ControllerManualNavigationOutcome::StartInstall { install },
    ) = cancel_controller.execute_deferred_transport_intent(
        intent,
        DeferredTransportExecutionContext {
            current_position: Duration::ZERO,
            previous_restart_threshold: PreviousRestartThreshold::from_milliseconds(0).unwrap(),
            wait_availability: DiscoveryManualWaitAvailability::Exhausted,
        },
    )
    else {
        panic!("recovered preview must continue to C")
    };
    assert_eq!(install.item_id, cancel_items[2]);

    let mut enqueue_controller = PlaylistController::new();
    let enqueue_items = append_items(&mut enqueue_controller, 3);
    install_active(&mut enqueue_controller, enqueue_items[0], 70);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut enqueue_controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut enqueue_controller, 71, 81, install);
    enqueue_controller.on_ready_to_commit(request_id(71));
    enqueue_controller
        .begin_authorization_dispatch(request_id(71))
        .unwrap();
    enqueue_controller
        .resolve_authorization_dispatch(
            request_id(71),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .unwrap();
    assert!(matches!(
        navigation(&mut enqueue_controller, ManualNavigationDirection::Next),
        ControllerManualNavigationOutcome::Guarded(TransportGuardOutcome::AwaitInstalled { .. })
    ));
    let drain = enqueue_controller
        .on_installed(
            request_id(71),
            player_request_id(81),
            media_instance_id(82),
            PlaylistBindingGeneration(1),
        )
        .unwrap();
    assert_eq!(
        enqueue_controller
            .queue
            .traversal_current()
            .unwrap()
            .item_id(),
        enqueue_items[1]
    );
    let DeferredControllerIntent::Transport(intent) = drain.deferred_intent.unwrap() else {
        panic!("post-commit cursor intent")
    };
    let DeferredTransportExecutionOutcome::Navigation(
        ControllerManualNavigationOutcome::StartInstall { install },
    ) = enqueue_controller.execute_deferred_transport_intent(
        intent,
        DeferredTransportExecutionContext {
            current_position: Duration::ZERO,
            previous_restart_threshold: PreviousRestartThreshold::from_milliseconds(0).unwrap(),
            wait_availability: DiscoveryManualWaitAvailability::Exhausted,
        },
    )
    else {
        panic!("post-commit navigation starts from exact B")
    };
    assert_eq!(install.item_id, enqueue_items[2]);
}

#[test]
fn concrete_failure_requires_retry_or_explicit_cursor_action() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 3);
    install_active(&mut controller, items[0], 90);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut controller, 91, 101, install);
    assert_eq!(
        controller.report_manual_navigation_target_failure(request_id(91)),
        ManualNavigationFailureOutcome::AwaitingUserAfterFailure { item_id: items[1] }
    );
    assert!(
        controller
            .view_snapshot()
            .awaiting_user_after_navigation_failure()
    );
    let failed_row = controller.view_snapshot().visible_rows(1..2).remove(0);
    assert!(!failed_row.is_pending());
    assert_eq!(
        controller.queue.traversal_current().unwrap().item_id(),
        items[0]
    );

    let ManualNavigationRetryOutcome::StartInstall { install } =
        controller.retry_failed_manual_navigation()
    else {
        panic!("retry must target exact failed B")
    };
    assert_eq!(install.item_id, items[1]);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("Next after D55 must continue to C")
    };
    assert_eq!(install.item_id, items[2]);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Previous)
    else {
        panic!("Previous after D55 continuation must backtrack to B")
    };
    assert_eq!(install.item_id, items[1]);
}

#[test]
fn concrete_preparation_failure_before_player_admission_enters_d55() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 2);
    install_active(&mut controller, items[0], 190);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B creates an unstaged plan")
    };

    assert_eq!(
        controller.report_unstaged_manual_navigation_target_failure(install.item_id),
        ManualNavigationFailureOutcome::AwaitingUserAfterFailure { item_id: items[1] }
    );
    assert!(
        controller
            .view_snapshot()
            .awaiting_user_after_navigation_failure()
    );
    assert!(matches!(
        controller.retry_failed_manual_navigation(),
        ManualNavigationRetryOutcome::StartInstall { install }
            if install.item_id == items[1]
    ));
}

#[test]
fn downstream_rejection_is_concrete_failure_and_play_pause_keeps_confirmation_cursor() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 2);
    install_active(&mut controller, items[0], 105);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut controller, 106, 116, install);
    controller.on_ready_to_commit(request_id(106));
    controller
        .begin_authorization_dispatch(request_id(106))
        .unwrap();
    let drain = controller
        .resolve_authorization_dispatch(
            request_id(106),
            AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                rejection: PlayerDispatchRejection::Backpressure,
            },
        )
        .unwrap()
        .expect("pre-barrier rejection terminal");
    assert_eq!(
        drain.resolution,
        ControllerTerminalResolution::DownstreamRejectedBeforeEnqueue {
            rejection: PlayerDispatchRejection::Backpressure
        }
    );
    assert!(
        controller
            .manual_navigation_cursor
            .is_awaiting_user_after_failure()
    );
    assert_eq!(
        controller.report_manual_navigation_target_failure(request_id(106)),
        ManualNavigationFailureOutcome::StaleRequest {
            request_id: request_id(106)
        }
    );

    let _dispatch = controller
        .record_stable_transport_intent(StablePlaybackIntent::Paused, TransportActionOrigin::Ui)
        .expect("D52 revision");
    assert!(
        controller
            .manual_navigation_cursor
            .is_awaiting_user_after_failure()
    );
    let ManualNavigationRetryOutcome::StartInstall { install } =
        controller.retry_failed_manual_navigation()
    else {
        panic!("confirmation Play/Pause must not replace failed target")
    };
    assert_eq!(install.item_id, items[1]);
}

#[test]
fn retry_without_failed_target_has_exact_typed_outcome() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 2);
    install_active(&mut controller, items[0], 145);
    assert!(matches!(
        controller.retry_failed_manual_navigation(),
        ManualNavigationRetryOutcome::NoFailedTarget
    ));

    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut controller, 146, 156, install);
    assert!(matches!(
        controller.retry_failed_manual_navigation(),
        ManualNavigationRetryOutcome::InstallAlreadyInProgress { request_id: active_request_id }
            if active_request_id == request_id(146)
    ));
}

#[test]
fn ended_cancel_stops_only_matching_ended_origin() {
    let mut ended_controller = PlaylistController::new();
    let ended_items = append_items(&mut ended_controller, 2);
    let ended_active = install_active(&mut ended_controller, ended_items[0], 110);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut ended_controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut ended_controller, 111, 121, install);
    ended_controller.report_manual_navigation_target_failure(request_id(111));
    assert!(ended_controller.mark_manual_navigation_origin_ended(ended_active));
    assert!(matches!(
        ended_controller.cancel_manual_navigation(),
        ManualNavigationCancelOutcome::Discarded(ManualNavigationInvalidation {
            cause: MediaInstallCancellationCause::UserCancelled,
            terminal_action: ManualNavigationTerminalAction::StopEndedOrigin,
            ..
        })
    ));

    let mut active_controller = PlaylistController::new();
    let active_items = append_items(&mut active_controller, 2);
    install_active(&mut active_controller, active_items[0], 130);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut active_controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut active_controller, 131, 141, install);
    active_controller.report_manual_navigation_target_failure(request_id(131));
    assert!(matches!(
        active_controller.cancel_manual_navigation(),
        ManualNavigationCancelOutcome::Discarded(ManualNavigationInvalidation {
            terminal_action: ManualNavigationTerminalAction::KeepActive,
            ..
        })
    ));
}

#[test]
fn structural_invalidation_is_distinct_one_revision_and_rejects_stale_result() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 2);
    let active = install_active(&mut controller, items[0], 150);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("A -> B")
    };
    accept_plan(&mut controller, 151, 161, install);
    controller.report_manual_navigation_target_failure(request_id(151));
    controller.mark_manual_navigation_origin_ended(active);
    let dirty_before = controller.dirty_revision();

    let ControllerAppendOutcome::Added {
        manual_navigation_invalidation: Some(invalidation),
        ..
    } = controller
        .append(vec![draft(9)])
        .expect("one structural edit")
    else {
        panic!("failed cursor must be invalidated by committed structural edit")
    };
    assert_eq!(
        invalidation.cause,
        MediaInstallCancellationCause::StructuralInvalidation
    );
    assert_eq!(
        invalidation.terminal_action,
        ManualNavigationTerminalAction::StopEndedOrigin
    );
    assert_eq!(controller.dirty_revision().get(), dirty_before.get() + 1);
    assert_eq!(
        controller.report_manual_navigation_target_failure(request_id(151)),
        ManualNavigationFailureOutcome::StaleRequest {
            request_id: request_id(151)
        }
    );
    assert_eq!(
        controller.on_ready_to_commit(request_id(151)),
        InstallReadyOutcome::StaleManualNavigationResult {
            request_id: request_id(151)
        }
    );
}

#[test]
fn pre_concrete_probe_rejection_remains_waiting_not_d55() {
    let mut controller = PlaylistController::new();
    let item = append_items(&mut controller, 1)[0];
    install_active(&mut controller, item, 170);
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(171));
    let ControllerManualNavigationOutcome::Waiting { wait_id, .. } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).unwrap(),
        DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    ) else {
        panic!("no concrete row must remain a D50 wait")
    };
    assert_eq!(
        controller.report_pre_concrete_probe_rejection(wait_id, scope_id),
        PreConcreteProbeRejectionOutcome::ContinueWaiting { wait_id, scope_id }
    );
    assert!(
        !controller
            .manual_navigation_cursor
            .is_awaiting_user_after_failure()
    );
}

#[test]
fn shuffle_fast_skip_is_consumed_but_never_becomes_factual_history() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 4);
    install_active(&mut controller, items[0], 180);
    controller.queue.enable_shuffle().expect("shuffle");

    let ControllerManualNavigationOutcome::StartInstall { install: first } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("first shuffle target")
    };
    let skipped_item = first.item_id;
    accept_plan(&mut controller, 181, 191, first);
    let ControllerManualNavigationOutcome::SupersedeInstall {
        install: latest, ..
    } = navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("second shuffle target")
    };
    let latest_item = latest.item_id;
    assert_ne!(skipped_item, latest_item);
    controller
        .supersede_install_request_before_ready(
            request_id(181),
            request_from_plan(182, 192, latest),
        )
        .unwrap();
    enqueue_and_install(&mut controller, 182, 192);

    let snapshot = controller
        .queue
        .shuffle_traversal_snapshot()
        .expect("enabled shuffle snapshot");
    assert!(!snapshot.history().contains(&skipped_item));
    assert!(snapshot.history().contains(&latest_item));
    assert!(!snapshot.upcoming().contains(&skipped_item));
    assert!(!snapshot.upcoming().contains(&latest_item));
}

#[test]
fn fifty_thousand_rows_keep_fast_cursor_view_delta_bounded() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, playlist_core::MAX_PLAYLIST_ITEMS);
    install_active(&mut controller, items[0], 200);
    let shared_rows_before = controller.view_snapshot().shared_rows_identity();
    let dirty_before = controller.dirty_revision();

    let ControllerManualNavigationOutcome::StartInstall { install } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("first bounded cursor step")
    };
    accept_plan(&mut controller, 201, 211, install);
    let ControllerManualNavigationOutcome::SupersedeInstall { install, .. } =
        navigation(&mut controller, ManualNavigationDirection::Next)
    else {
        panic!("second bounded cursor step")
    };
    controller
        .supersede_install_request_before_ready(
            request_id(201),
            request_from_plan(202, 212, install),
        )
        .unwrap();

    assert_eq!(
        controller.view_snapshot().shared_rows_identity(),
        shared_rows_before
    );
    assert_eq!(controller.dirty_revision(), dirty_before);
    assert_eq!(
        controller.queue.traversal_current().unwrap().item_id(),
        items[0]
    );
}
