//! D53 cursor-шаг во время barrier-а исполняется после terminal относительно правильного active.

use super::*;

fn deferred_context() -> DeferredTransportExecutionContext {
    DeferredTransportExecutionContext {
        current_position: Duration::ZERO,
        previous_restart_threshold: PreviousRestartThreshold::from_milliseconds(0)
            .expect("zero threshold is valid"),
        wait_availability: DiscoveryManualWaitAvailability::Exhausted,
    }
}

#[test]
fn dispatch_cancel_winner_recovers_preview_while_enqueue_winner_uses_new_active() {
    // Cancel-win: preview восстановлен, отложенный Next продолжает A -> B -> C.
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
    cancel_controller
        .resolve_authorization_dispatch(
            request_id(51),
            AuthorizationDispatchResolution::CancelWonBeforePlayerEnqueue {
                cause: MediaInstallCancellationCause::Superseded,
            },
        )
        .expect("cancel winner")
        .expect("terminal drain");
    let intent = cancel_controller
        .take_terminal_transport_intent()
        .expect("latest cursor step survives cancel winner");
    let DeferredTransportExecutionOutcome::Navigation(
        ControllerManualNavigationOutcome::StartInstall { install },
    ) = cancel_controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("recovered preview must continue to C")
    };
    assert_eq!(install.item_id, cancel_items[2]);

    // Enqueue-win: B закоммичен, отложенный Next считается от нового active B.
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
        .expect("dispatch");
    enqueue_controller
        .resolve_authorization_dispatch(
            request_id(71),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue winner");
    assert!(matches!(
        navigation(&mut enqueue_controller, ManualNavigationDirection::Next),
        ControllerManualNavigationOutcome::Guarded(TransportGuardOutcome::AwaitInstalled { .. })
    ));
    enqueue_controller
        .on_installed(
            request_id(71),
            player_request_id(81),
            media_instance_id(82),
            PlaylistBindingGeneration(1),
        )
        .expect("B Installed");
    assert_eq!(
        enqueue_controller
            .queue
            .traversal_current()
            .map(|current| current.item_id()),
        Some(enqueue_items[1])
    );
    let intent = enqueue_controller
        .take_terminal_transport_intent()
        .expect("post-commit cursor intent");
    let DeferredTransportExecutionOutcome::Navigation(
        ControllerManualNavigationOutcome::StartInstall { install },
    ) = enqueue_controller.execute_deferred_transport_intent(intent, deferred_context())
    else {
        panic!("post-commit navigation starts from exact B")
    };
    assert_eq!(install.item_id, enqueue_items[2]);
}
