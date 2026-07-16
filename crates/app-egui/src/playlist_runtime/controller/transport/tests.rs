use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use player_core::{
    ExactMediaTransportAction, ExactMediaTransportOutcome, MediaInstallCancellationCause,
    MediaInstallRequestId, MediaInstanceId, PlaybackIntent, PlaybackState,
};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, ManualNavigationDirection, PlaylistItemDraft,
    PlaylistItemId, PlaylistMediaKind, RepeatMode,
};

use super::*;
use crate::media_open::{AuthorizationDispatchResolution, MediaOpenRequestId};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::{
    ControllerAppendOutcome, ControllerTerminalResolution, InstallReadyOutcome,
    PlaylistInstallRequest,
};
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity must be non-zero")
}

fn media_open_request_id(value: u64) -> MediaOpenRequestId {
    MediaOpenRequestId::from_non_zero(non_zero(value))
}

fn media_install_request_id(value: u64) -> MediaInstallRequestId {
    MediaInstallRequestId::from_non_zero(non_zero(value))
}

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(non_zero(value))
}

fn draft(index: usize) -> PlaylistItemDraft {
    let display_name = format!("manual-{index}.mkv");
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

fn install_active_fixture(
    controller: &mut PlaylistController,
    item_id: PlaylistItemId,
    instance_value: u64,
) -> ActiveMediaIdentity {
    controller
        .queue
        .commit_manual_play(item_id)
        .expect("fixture current must commit");
    let active = ActiveMediaIdentity::installed(
        Some(item_id),
        ActiveMediaLineageId::from_non_zero(non_zero(instance_value)),
        media_instance_id(instance_value),
        PlaylistBindingGeneration(1),
    );
    controller.active_media = Some(active);
    active
}

fn accept_planned_install(
    controller: &mut PlaylistController,
    request_value: u64,
    player_request_value: u64,
    install: PlannedPlaylistInstall,
) {
    controller
        .accept_install_request(PlaylistInstallRequest {
            request_id: media_open_request_id(request_value),
            player_request_id: media_install_request_id(player_request_value),
            target_item_id: Some(install.item_id),
            origin: install.pending_origin,
            intent_revision: install.intent_revision,
            expected_queue_revision: install.expected_queue_revision,
            mutation: install.mutation,
        })
        .expect("planned install admission");
}

fn threshold(milliseconds: u64) -> PreviousRestartThreshold {
    PreviousRestartThreshold::from_milliseconds(milliseconds).expect("valid threshold")
}

fn deferred_context() -> DeferredTransportExecutionContext {
    DeferredTransportExecutionContext {
        current_position: Duration::ZERO,
        previous_restart_threshold: threshold(0),
        wait_availability: DiscoveryManualWaitAvailability::Exhausted,
    }
}

#[test]
fn stable_intent_ignores_transient_snapshots_and_builds_exact_plus_d52_dispatch() {
    let mut controller = PlaylistController::new();
    let item_id = append_items(&mut controller, 1)[0];
    let active = install_active_fixture(&mut controller, item_id, 10);

    assert!(!controller.observe_player_snapshot_state(PlaybackState::Buffering));
    assert!(!controller.observe_player_snapshot_state(PlaybackState::Seeking));
    assert_eq!(
        controller.stable_playback_intent(),
        StablePlaybackIntent::Paused
    );

    let dispatch = controller
        .record_stable_transport_intent(StablePlaybackIntent::Playing, TransportActionOrigin::Ui)
        .expect("revision");
    assert_eq!(dispatch.intent, PlaybackIntent::StartPlaying);
    assert_eq!(
        dispatch.exact_current,
        Some(ExactMediaTransportRequest {
            media_instance_id: active.media_instance_id(),
            action: ExactMediaTransportAction::SetPlaybackIntent {
                intent: PlaybackIntent::StartPlaying,
            },
        })
    );
    assert_eq!(dispatch.pending_update, None);
}

#[test]
fn play_active_restarts_clean_instance_but_runtime_failure_reinstalls() {
    let mut controller = PlaylistController::new();
    let item_id = append_items(&mut controller, 1)[0];
    let active = install_active_fixture(&mut controller, item_id, 20);

    let ControllerPlayItemOutcome::RestartActive { request, .. } =
        controller.play_item(item_id, TransportActionOrigin::Ui)
    else {
        panic!("clean active item must restart")
    };
    assert_eq!(request.media_instance_id, active.media_instance_id());
    assert_eq!(
        request.action,
        ExactMediaTransportAction::RestartFromBeginning {
            intent: PlaybackIntent::StartPlaying,
        }
    );

    assert_eq!(
        controller.record_playback_error(
            item_id,
            active.media_instance_id(),
            Arc::from("runtime failed"),
        ),
        crate::playlist_runtime::controller::RuntimeErrorCorrelationOutcome::Recorded
    );
    assert!(matches!(
        controller.play_item(item_id, TransportActionOrigin::Ui),
        ControllerPlayItemOutcome::StartInstall { .. }
    ));
}

#[test]
fn matching_pending_play_coalesces_and_raises_exact_d52_intent() {
    let mut controller = PlaylistController::new();
    let item_id = append_items(&mut controller, 1)[0];
    let first_play = controller.play_item(item_id, TransportActionOrigin::Ui);
    let ControllerPlayItemOutcome::StartInstall { install, .. } = first_play else {
        panic!("first play must start install")
    };
    accept_planned_install(&mut controller, 31, 41, install);

    let ControllerPlayItemOutcome::CoalescePending {
        request_id,
        intent_dispatch,
    } = controller.play_item(item_id, TransportActionOrigin::Ui)
    else {
        panic!("matching pending item must coalesce")
    };
    assert_eq!(request_id, media_open_request_id(31));
    assert_eq!(
        intent_dispatch
            .pending_update
            .expect("D52 pending update")
            .request_id,
        media_install_request_id(41)
    );
    assert_eq!(intent_dispatch.exact_current, None);
}

#[test]
fn previous_restart_threshold_is_strict_and_zero_disables_restart() {
    for (position_ms, threshold_ms, expects_restart) in [
        (6_000, 5_000, true),
        (5_000, 5_000, false),
        (4_999, 5_000, false),
        (60_000, 0, false),
    ] {
        let mut controller = PlaylistController::new();
        let item_id = append_items(&mut controller, 1)[0];
        install_active_fixture(&mut controller, item_id, position_ms + 100);
        let outcome = controller.manual_navigation(
            ManualNavigationDirection::Previous,
            TransportActionOrigin::Ui,
            Duration::from_millis(position_ms),
            threshold(threshold_ms),
            DiscoveryManualWaitAvailability::Exhausted,
        );
        assert_eq!(
            matches!(
                outcome,
                ControllerManualNavigationOutcome::RestartCurrent { .. }
            ),
            expects_restart,
            "position={position_ms}, threshold={threshold_ms}"
        );
    }
}

#[test]
fn d50_wait_reads_latest_stable_intent_only_when_candidate_becomes_committed() {
    let mut controller = PlaylistController::new();
    let first_item = append_items(&mut controller, 1)[0];
    install_active_fixture(&mut controller, first_item, 50);
    controller
        .record_stable_transport_intent(StablePlaybackIntent::Playing, TransportActionOrigin::Ui)
        .expect("playing revision");
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(5));
    let ControllerManualNavigationOutcome::Waiting { wait_id, .. } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        threshold(5_000),
        DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    ) else {
        panic!("boundary Next must wait")
    };

    controller
        .record_stable_transport_intent(StablePlaybackIntent::Paused, TransportActionOrigin::Ui)
        .expect("paused revision");
    append_items(&mut controller, 1);
    let ControllerManualNavigationOutcome::StartInstall { install } =
        controller.resume_manual_navigation_wait(wait_id, scope_id, false)
    else {
        panic!("admitted neighbor must resolve wait")
    };
    assert_eq!(install.playback_intent, PlaybackIntent::StartPaused);
}

#[test]
fn shuffle_previous_never_waits_without_factual_history_but_shuffle_next_may_wait() {
    let mut previous_controller = PlaylistController::new();
    let first_item = append_items(&mut previous_controller, 1)[0];
    install_active_fixture(&mut previous_controller, first_item, 60);
    previous_controller.queue.enable_shuffle().expect("shuffle");
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(6));
    assert!(matches!(
        previous_controller.manual_navigation(
            ManualNavigationDirection::Previous,
            TransportActionOrigin::Ui,
            Duration::ZERO,
            threshold(0),
            DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
        ),
        ControllerManualNavigationOutcome::NoItem(_)
    ));
    assert!(matches!(
        previous_controller.manual_navigation(
            ManualNavigationDirection::Next,
            TransportActionOrigin::Ui,
            Duration::ZERO,
            threshold(0),
            DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
        ),
        ControllerManualNavigationOutcome::Waiting { .. }
    ));
}

#[test]
fn manual_shuffle_token_commits_factual_previous_only_after_installed() {
    let mut controller = PlaylistController::new();
    let item_ids = append_items(&mut controller, 2);
    controller.queue.enable_shuffle().expect("shuffle");
    controller
        .queue
        .commit_manual_play(item_ids[0])
        .expect("play first");
    install_active_fixture(&mut controller, item_ids[1], 70);
    let before = controller.queue.shuffle_traversal_snapshot();
    let ControllerManualNavigationOutcome::StartInstall { install } = controller.manual_navigation(
        ManualNavigationDirection::Previous,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        threshold(0),
        DiscoveryManualWaitAvailability::Exhausted,
    ) else {
        panic!("history Previous must choose first item")
    };
    assert_eq!(install.item_id, item_ids[0]);
    accept_planned_install(&mut controller, 71, 81, install);
    assert!(matches!(
        controller.on_ready_to_commit(media_open_request_id(71)),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    assert_eq!(controller.queue.shuffle_traversal_snapshot(), before);
    controller
        .begin_authorization_dispatch(media_open_request_id(71))
        .expect("dispatch");
    controller
        .resolve_authorization_dispatch(
            media_open_request_id(71),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("resolution");
    controller
        .on_installed(
            media_open_request_id(71),
            media_install_request_id(81),
            media_instance_id(82),
            PlaylistBindingGeneration(1),
        )
        .expect("installed");
    assert_eq!(
        controller
            .queue()
            .traversal_current()
            .map(|current| current.item_id()),
        Some(item_ids[0])
    );
}

#[test]
fn guard_uses_exact_abort_then_latest_barrier_transport_without_fifo() {
    let mut controller = PlaylistController::new();
    let item_ids = append_items(&mut controller, 3);
    install_active_fixture(&mut controller, item_ids[0], 90);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[1], TransportActionOrigin::Ui)
    else {
        panic!("start B")
    };
    accept_planned_install(&mut controller, 91, 101, install);
    controller.on_ready_to_commit(media_open_request_id(91));

    let ControllerPlayItemOutcome::Guarded {
        guard: TransportGuardOutcome::ExecuteNow {
            aborted_request_id, ..
        },
        intent_dispatch,
    } = controller.play_item(item_ids[2], TransportActionOrigin::Ui)
    else {
        panic!("pre-dispatch Play must exact-abort reservation")
    };
    assert_eq!(aborted_request_id, Some(media_open_request_id(91)));
    assert_eq!(
        intent_dispatch
            .pending_update
            .expect("D52 update")
            .request_id,
        media_install_request_id(101)
    );

    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[1], TransportActionOrigin::Ui)
    else {
        panic!("restart B request")
    };
    accept_planned_install(&mut controller, 92, 102, install);
    controller.on_ready_to_commit(media_open_request_id(92));
    controller
        .begin_authorization_dispatch(media_open_request_id(92))
        .expect("dispatch pending");
    assert!(matches!(
        controller.play_item(item_ids[2], TransportActionOrigin::Ui),
        ControllerPlayItemOutcome::Guarded {
            guard: TransportGuardOutcome::AwaitAuthorizationResolution { .. },
            intent_dispatch: ControllerStableIntentDispatch {
                pending_update: Some(_),
                exact_current: None,
                ..
            },
        }
    ));
    assert!(matches!(
        controller.neutral_stop(TransportActionOrigin::Mpris),
        Some(Err(
            TransportGuardOutcome::AwaitAuthorizationResolution { .. }
        ))
    ));
    controller
        .resolve_authorization_dispatch(
            media_open_request_id(92),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue winner");
    let drain = controller
        .on_installed(
            media_open_request_id(92),
            media_install_request_id(102),
            media_instance_id(103),
            PlaylistBindingGeneration(1),
        )
        .expect("installed");
    assert!(matches!(
        drain.deferred_intent,
        Some(DeferredControllerIntent::Transport(
            DeferredTransportIntent::Stop { .. }
        ))
    ));
    assert_eq!(drain.resolution, ControllerTerminalResolution::Installed);
    let Some(DeferredControllerIntent::Transport(intent)) = drain.deferred_intent else {
        panic!("latest Stop must survive until terminal drain")
    };
    let DeferredTransportExecutionOutcome::NeutralStop(Some(Ok(request))) =
        controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("post-commit Stop must address the installed instance")
    };
    assert_eq!(request.media_instance_id, media_instance_id(103));
}

#[test]
fn stop_after_current_clears_wait_and_pre_ready_cause_is_distinct() {
    let mut controller = PlaylistController::new();
    let item_ids = append_items(&mut controller, 2);
    install_active_fixture(&mut controller, item_ids[0], 110);
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(11));
    let ControllerManualNavigationOutcome::StartInstall { install } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        threshold(0),
        DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    ) else {
        panic!("ready B")
    };
    accept_planned_install(&mut controller, 111, 121, install);
    let StopAfterCurrentOutcome::Guarded(TransportGuardOutcome::CancelPendingThenExecute {
        cause,
        ..
    }) = controller.toggle_stop_after_current(true, TransportActionOrigin::Ui)
    else {
        panic!("pre-ready latch must cancel exact request")
    };
    assert_eq!(cause, MediaInstallCancellationCause::StopAfterCurrent);
    assert!(matches!(
        controller.toggle_stop_after_current(false, TransportActionOrigin::Ui),
        StopAfterCurrentOutcome::AppliedToCurrent { enabled: false }
    ));
}

#[test]
fn neutral_stop_sets_stopped_only_after_matching_success_and_mpris_navigation_starts_paused() {
    let mut controller = PlaylistController::new();
    let item_ids = append_items(&mut controller, 2);
    let active = install_active_fixture(&mut controller, item_ids[0], 130);
    let request = controller
        .neutral_stop(TransportActionOrigin::Mpris)
        .expect("active")
        .expect("no guard");
    assert_eq!(request.action, ExactMediaTransportAction::NeutralStop);
    assert!(
        !controller.apply_neutral_stop_outcome(&ExactMediaTransportOutcome::StaleInstance {
            requested_media_instance_id: active.media_instance_id(),
            current_media_instance_id: None,
        })
    );
    assert!(
        controller.apply_neutral_stop_outcome(&ExactMediaTransportOutcome::Applied {
            media_instance_id: active.media_instance_id(),
        })
    );
    assert_eq!(
        controller.transport_disposition(),
        AppTransportDisposition::Stopped
    );

    let ControllerManualNavigationOutcome::StartInstall { install } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Mpris,
        Duration::ZERO,
        threshold(0),
        DiscoveryManualWaitAvailability::Exhausted,
    ) else {
        panic!("MPRIS Next")
    };
    assert_eq!(install.playback_intent, PlaybackIntent::StartPaused);
}

#[test]
fn cancel_winner_preserves_exact_terminal_cause() {
    let mut controller = PlaylistController::new();
    let item_ids = append_items(&mut controller, 2);
    install_active_fixture(&mut controller, item_ids[0], 140);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[1], TransportActionOrigin::Ui)
    else {
        panic!("start install")
    };
    accept_planned_install(&mut controller, 141, 151, install);
    controller.on_ready_to_commit(media_open_request_id(141));
    controller
        .begin_authorization_dispatch(media_open_request_id(141))
        .expect("dispatch");
    controller.neutral_stop(TransportActionOrigin::Mpris);
    let drain = controller
        .resolve_authorization_dispatch(
            media_open_request_id(141),
            AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                cause: MediaInstallCancellationCause::TransportStop,
            },
        )
        .expect("resolution")
        .expect("terminal drain");
    assert_eq!(
        drain.resolution,
        ControllerTerminalResolution::CancelWonBeforePlayerEnqueue {
            cause: MediaInstallCancellationCause::TransportStop,
        }
    );
    let Some(DeferredControllerIntent::Transport(intent)) = drain.deferred_intent else {
        panic!("cancel winner must retain Stop for the old lineage")
    };
    let DeferredTransportExecutionOutcome::NeutralStop(Some(Ok(request))) =
        controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("cancel-winner Stop must address the old instance")
    };
    assert_eq!(request.media_instance_id, media_instance_id(140));
}

#[test]
fn stop_after_current_terminal_executor_targets_winning_lineage_and_latest_toggle() {
    let mut cancel_controller = PlaylistController::new();
    let cancel_items = append_items(&mut cancel_controller, 2);
    install_active_fixture(&mut cancel_controller, cancel_items[0], 170);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        cancel_controller.play_item(cancel_items[1], TransportActionOrigin::Ui)
    else {
        panic!("start cancel-winner fixture")
    };
    accept_planned_install(&mut cancel_controller, 171, 181, install);
    cancel_controller.on_ready_to_commit(media_open_request_id(171));
    cancel_controller
        .begin_authorization_dispatch(media_open_request_id(171))
        .expect("dispatch pending");
    assert!(matches!(
        cancel_controller.toggle_stop_after_current(true, TransportActionOrigin::Ui),
        StopAfterCurrentOutcome::Guarded(
            TransportGuardOutcome::AwaitAuthorizationResolution { .. }
        )
    ));
    let cancel_drain = cancel_controller
        .resolve_authorization_dispatch(
            media_open_request_id(171),
            AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                cause: MediaInstallCancellationCause::StopAfterCurrent,
            },
        )
        .expect("cancel resolution")
        .expect("cancel terminal drain");
    let Some(DeferredControllerIntent::Transport(cancel_intent)) = cancel_drain.deferred_intent
    else {
        panic!("cancel winner must retain latch intent")
    };
    assert!(matches!(
        cancel_controller.execute_deferred_transport_intent(cancel_intent, deferred_context()),
        DeferredTransportExecutionOutcome::StopAfterCurrent(
            StopAfterCurrentOutcome::AppliedToCurrent { enabled: true }
        )
    ));
    assert_eq!(
        cancel_controller
            .stop_after_current()
            .expect("old lineage latch")
            .item_id(),
        Some(cancel_items[0])
    );

    let mut enqueue_controller = PlaylistController::new();
    let enqueue_items = append_items(&mut enqueue_controller, 2);
    install_active_fixture(&mut enqueue_controller, enqueue_items[0], 190);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        enqueue_controller.play_item(enqueue_items[1], TransportActionOrigin::Ui)
    else {
        panic!("start enqueue-winner fixture")
    };
    accept_planned_install(&mut enqueue_controller, 191, 201, install);
    enqueue_controller.on_ready_to_commit(media_open_request_id(191));
    enqueue_controller
        .begin_authorization_dispatch(media_open_request_id(191))
        .expect("dispatch pending");
    enqueue_controller.toggle_stop_after_current(true, TransportActionOrigin::Ui);
    enqueue_controller.toggle_stop_after_current(false, TransportActionOrigin::Ui);
    enqueue_controller
        .resolve_authorization_dispatch(
            media_open_request_id(191),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue winner");
    let enqueue_drain = enqueue_controller
        .on_installed(
            media_open_request_id(191),
            media_install_request_id(201),
            media_instance_id(202),
            PlaylistBindingGeneration(1),
        )
        .expect("installed");
    let Some(DeferredControllerIntent::Transport(enqueue_intent)) = enqueue_drain.deferred_intent
    else {
        panic!("enqueue winner must retain latest toggle")
    };
    assert!(matches!(
        enqueue_controller.execute_deferred_transport_intent(enqueue_intent, deferred_context()),
        DeferredTransportExecutionOutcome::StopAfterCurrent(
            StopAfterCurrentOutcome::AppliedToCurrent { enabled: false }
        )
    ));
    assert_eq!(
        enqueue_controller.active_media.unwrap().item_id(),
        Some(enqueue_items[1])
    );
    assert_eq!(enqueue_controller.stop_after_current(), None);
}

#[test]
fn repeat_one_manual_navigation_does_not_wrap_but_repeat_queue_does() {
    let mut controller = PlaylistController::new();
    let item_ids = append_items(&mut controller, 2);
    install_active_fixture(&mut controller, item_ids[1], 160);
    controller.repeat_mode = RepeatMode::RepeatOne;
    assert!(matches!(
        controller.manual_navigation(
            ManualNavigationDirection::Next,
            TransportActionOrigin::Ui,
            Duration::ZERO,
            threshold(0),
            DiscoveryManualWaitAvailability::Exhausted,
        ),
        ControllerManualNavigationOutcome::NoItem(_)
    ));
    controller.repeat_mode = RepeatMode::RepeatQueue;
    let ControllerManualNavigationOutcome::StartInstall { install } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        threshold(0),
        DiscoveryManualWaitAvailability::Exhausted,
    ) else {
        panic!("RepeatQueue must wrap")
    };
    assert_eq!(install.item_id, item_ids[0]);
}

#[test]
fn owner_availability_distinguishes_ready_wait_disabled_and_pending() {
    let mut controller = PlaylistController::new();
    let items = append_items(&mut controller, 2);
    install_active_fixture(&mut controller, items[0], 170);
    assert_eq!(
        controller.manual_navigation_availability(
            ManualNavigationDirection::Next,
            Duration::ZERO,
            threshold(5_000),
            DiscoveryManualWaitAvailability::Exhausted,
        ),
        ControllerManualNavigationAvailability::Ready
    );

    install_active_fixture(&mut controller, items[1], 171);
    assert_eq!(
        controller.manual_navigation_availability(
            ManualNavigationDirection::Next,
            Duration::ZERO,
            threshold(5_000),
            DiscoveryManualWaitAvailability::MayProduceCandidate {
                scope_id: SiblingDiscoveryScopeId::from_non_zero(non_zero(172)),
            },
        ),
        ControllerManualNavigationAvailability::PotentialWait
    );
    assert_eq!(
        controller.manual_navigation_availability(
            ManualNavigationDirection::Next,
            Duration::ZERO,
            threshold(5_000),
            DiscoveryManualWaitAvailability::Exhausted,
        ),
        ControllerManualNavigationAvailability::Disabled
    );

    let ControllerManualNavigationOutcome::StartInstall { .. } = controller.manual_navigation(
        ManualNavigationDirection::Previous,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        threshold(5_000),
        DiscoveryManualWaitAvailability::Exhausted,
    ) else {
        panic!("previous target must start a plan")
    };
    assert_eq!(
        controller.manual_navigation_availability(
            ManualNavigationDirection::Next,
            Duration::ZERO,
            threshold(5_000),
            DiscoveryManualWaitAvailability::Exhausted,
        ),
        ControllerManualNavigationAvailability::Pending
    );
}

#[test]
fn stable_toggle_uses_owner_intent_instead_of_transient_player_state() {
    let mut controller = PlaylistController::new();
    assert_eq!(
        controller.stable_playback_intent(),
        StablePlaybackIntent::Paused
    );

    controller
        .toggle_stable_transport_intent(TransportActionOrigin::Ui)
        .expect("first toggle advances the stable revision");
    assert_eq!(
        controller.stable_playback_intent(),
        StablePlaybackIntent::Playing
    );

    controller
        .toggle_stable_transport_intent(TransportActionOrigin::Ui)
        .expect("second toggle advances the stable revision");
    assert_eq!(
        controller.stable_playback_intent(),
        StablePlaybackIntent::Paused
    );
}
