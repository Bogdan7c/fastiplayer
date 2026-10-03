//! Исполнение transport-команд, остановленных D08/D39 guard-ом, и terminal slot latest-only.

use super::*;
use crate::playlist_runtime::controller::install::{
    ControllerTerminalResolution, DeferredControllerIntent, DeferredTransportIntent,
};
use player_core::MediaInstallCancellationCause;

fn deferred_context() -> DeferredTransportExecutionContext {
    DeferredTransportExecutionContext {
        current_position: Duration::ZERO,
        previous_restart_threshold: threshold(0),
        wait_availability: DiscoveryManualWaitAvailability::Exhausted,
    }
}

/// Active A, install B доведён до `AuthorizationInFlight` (enqueue winner).
fn in_flight_fixture(controller: &mut PlaylistController) -> Vec<PlaylistItemId> {
    let item_ids = append_items(controller, 3);
    install_active_fixture(controller, item_ids[0], 200);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[1], TransportActionOrigin::Ui)
    else {
        panic!("start B")
    };
    accept_planned_install(controller, 201, 211, install);
    controller.on_ready_to_commit(media_open_request_id(201));
    controller
        .begin_authorization_dispatch(media_open_request_id(201))
        .expect("dispatch");
    controller
        .resolve_authorization_dispatch(
            media_open_request_id(201),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue winner");
    item_ids
}

fn install_b(controller: &mut PlaylistController) {
    controller
        .on_installed(
            media_open_request_id(201),
            media_install_request_id(211),
            media_instance_id(212),
            PlaylistBindingGeneration(1),
        )
        .expect("B Installed");
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

    // Reserved-before-dispatch: guard сам делает exact abort и отдаёт команду на исполнение.
    let ControllerPlayItemOutcome::Guarded {
        guard:
            TransportGuardOutcome::ExecuteNow {
                aborted_request_id,
                intent,
                ..
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
    let DeferredTransportExecutionOutcome::PlayItem(ControllerPlayItemOutcome::StartInstall {
        install,
        ..
    }) = controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("executed Play row C starts its own install")
    };
    assert_eq!(install.item_id, item_ids[2]);

    // После barrier команды не копятся FIFO: выживает только последняя (Stop).
    accept_planned_install(&mut controller, 92, 102, install);
    controller.on_ready_to_commit(media_open_request_id(92));
    controller
        .begin_authorization_dispatch(media_open_request_id(92))
        .expect("dispatch pending");
    assert!(matches!(
        controller.play_item(item_ids[1], TransportActionOrigin::Ui),
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
    assert_eq!(drain.resolution, ControllerTerminalResolution::Installed);
    assert!(matches!(
        drain.deferred_intent,
        Some(DeferredControllerIntent::Transport(
            DeferredTransportIntent::Stop { .. }
        ))
    ));

    // Terminal slot отдаёт ту же команду ровно один раз.
    let intent = controller
        .take_terminal_transport_intent()
        .expect("latest Stop survives until terminal drain");
    assert_eq!(controller.take_terminal_transport_intent(), None);
    let DeferredTransportExecutionOutcome::NeutralStop(Some(Ok(request))) =
        controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("post-commit Stop must address the installed instance")
    };
    assert_eq!(request.media_instance_id, media_instance_id(103));
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
    let intent = controller
        .take_terminal_transport_intent()
        .expect("cancel winner must retain Stop for the old lineage");
    let DeferredTransportExecutionOutcome::NeutralStop(Some(Ok(request))) =
        controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("cancel-winner Stop must address the old instance")
    };
    assert_eq!(request.media_instance_id, media_instance_id(140));
}

#[test]
fn awaiting_ready_guard_releases_install_and_execution_targets_new_row() {
    let mut controller = PlaylistController::new();
    let item_ids = append_items(&mut controller, 3);
    install_active_fixture(&mut controller, item_ids[0], 300);
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[1], TransportActionOrigin::Ui)
    else {
        panic!("start B")
    };
    accept_planned_install(&mut controller, 301, 311, install);
    let current_before = controller.queue().traversal_current();

    let ControllerPlayItemOutcome::Guarded {
        guard:
            TransportGuardOutcome::CancelPendingThenExecute {
                request_id,
                cause,
                intent,
            },
        ..
    } = controller.play_item(item_ids[2], TransportActionOrigin::Ui)
    else {
        panic!("Play during AwaitingReady releases the pending request")
    };
    assert_eq!(request_id, media_open_request_id(301));
    assert_eq!(cause, MediaInstallCancellationCause::Superseded);
    // Controller снял свой install; committed current не тронут до нового Installed.
    assert_eq!(controller.install_phase(), None);
    assert_eq!(controller.queue().traversal_current(), current_before);

    let DeferredTransportExecutionOutcome::PlayItem(ControllerPlayItemOutcome::StartInstall {
        install,
        ..
    }) = controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("released Play row starts the requested target")
    };
    assert_eq!(install.item_id, item_ids[2]);
    // Немедленное исполнение не оставляет копию в terminal slot.
    assert_eq!(controller.take_terminal_transport_intent(), None);
}

#[test]
fn newer_transport_command_supersedes_unexecuted_terminal_intent() {
    let mut controller = PlaylistController::new();
    let item_ids = in_flight_fixture(&mut controller);
    assert!(matches!(
        controller.manual_navigation(
            ManualNavigationDirection::Next,
            TransportActionOrigin::Ui,
            Duration::ZERO,
            threshold(0),
            DiscoveryManualWaitAvailability::Exhausted,
        ),
        ControllerManualNavigationOutcome::Guarded(TransportGuardOutcome::AwaitInstalled { .. })
    ));
    install_b(&mut controller);

    // Пользователь нажал Play row раньше, чем app исполнил отложенный Next.
    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[0], TransportActionOrigin::Ui)
    else {
        panic!("fresh Play row executes immediately")
    };
    assert_eq!(install.item_id, item_ids[0]);
    assert_eq!(
        controller.take_terminal_transport_intent(),
        None,
        "latest-only: старый Next не должен исполниться после нового Play"
    );
}

#[test]
fn lifecycle_suspend_never_enters_terminal_transport_slot() {
    let mut controller = PlaylistController::new();
    in_flight_fixture(&mut controller);
    controller
        .request_lifecycle_intent(DeferredControllerIntent::Suspend)
        .expect("suspend is parked");
    install_b(&mut controller);
    assert_eq!(controller.take_terminal_transport_intent(), None);
}

#[test]
fn terminal_slot_is_empty_without_guarded_command() {
    let mut controller = PlaylistController::new();
    assert_eq!(controller.take_terminal_transport_intent(), None);
    in_flight_fixture(&mut controller);
    install_b(&mut controller);
    assert_eq!(controller.take_terminal_transport_intent(), None);
}
