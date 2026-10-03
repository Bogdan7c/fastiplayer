//! Functional regressions: transport-команды (Play row / Next / MPRIS Stop), нажатые во время
//! playlist install, исполняются ровно один раз и не отвязывают старый install от плейлиста.
//!
//! Тесты идут через те же `PlaylistRuntime` entry points, что и `AppState`
//! (`play_playlist_row`, `request_playlist_navigation`, `request_desktop_stop`,
//! `resolve_transport_guard`, `register_successful_strong_install`,
//! `take_terminal_transport_execution`). Coordinator/player эмулируются ровно в той форме,
//! в которой их результат видит runtime.

use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use player_core::{ExactMediaTransportAction, MediaInstallCancellationCause};
use player_core::{MediaInstallRequestId, MediaInstanceId};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, ManualNavigationDirection, PlaylistItemDraft,
    PlaylistItemId, PlaylistMediaKind,
};

use super::{
    GuardedTransportFollowUp, PlaylistRuntime, PlaylistRuntimeBinding, ReleasedPendingRequest,
};
use crate::app_wake::{AppWakeEvent, AppWakeOwner, AppWakePort, WakeEmitter};
use crate::media_open::{ActiveMediaSource, AuthorizationDispatchResolution, MediaOpenRequestId};
use crate::playlist_runtime::controller::{
    ControllerAppendOutcome, ControllerInstallPhase, ControllerManualNavigationOutcome,
    ControllerPlayItemOutcome, DeferredTransportExecutionOutcome, DeferredTransportIntent,
    InstallReadyOutcome, PlannedPlaylistInstall, PlaylistControllerInvariantViolation,
    TransportGuardOutcome,
};
use crate::playlist_runtime::identity::TransportActionOrigin;
use crate::playlist_runtime::row_interactions::RuntimeRowPlayOutcome;

struct NoopEmitter;

impl WakeEmitter for NoopEmitter {
    fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
        Ok(())
    }
}

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
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

fn wake_port() -> AppWakePort {
    AppWakePort::new(AppWakeOwner::PlaylistRuntime, Arc::new(NoopEmitter))
}

/// Runtime с открытым allocator gate, строками A/B/C и живым player binding.
fn runtime_with_items() -> (PlaylistRuntime, PlaylistRuntimeBinding, [PlaylistItemId; 3]) {
    let mut runtime = PlaylistRuntime::new(wake_port());
    runtime.resolve_missing_state_for_test();
    let binding = runtime.bind_resumed_app_state().expect("player binding");
    let drafts = ["a.webm", "b.webm", "c.webm"]
        .into_iter()
        .map(|label| {
            PlaylistItemDraft::local(
                LocalLocator::Native(PathBuf::from(label)),
                None,
                CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
            )
        })
        .collect();
    let ControllerAppendOutcome::Added { item_ids, .. } = runtime
        .controller
        .as_mut()
        .expect("controller")
        .append(drafts)
        .expect("append A/B/C")
    else {
        panic!("fixture is non-empty");
    };
    let item_ids: [PlaylistItemId; 3] = item_ids.try_into().expect("exactly A/B/C");
    (runtime, binding, item_ids)
}

/// App admission planned install-а (как `begin_planned_playlist_install` после staging).
fn admit(runtime: &mut PlaylistRuntime, install: PlannedPlaylistInstall, request: u64) {
    runtime
        .accept_planned_playlist_install(request_id(request), player_request_id(request), install)
        .expect("install admission");
}

/// Play row через runtime + admission его strong install.
fn start_row_install(runtime: &mut PlaylistRuntime, item_id: PlaylistItemId, request: u64) {
    let RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::StartInstall {
        install, ..
    }) = runtime.play_playlist_row(item_id)
    else {
        panic!("idle row Play starts strong install");
    };
    admit(runtime, install, request);
}

/// Ready -> authorization -> enqueue barrier (фаза `AuthorizationInFlight`).
fn advance_to_in_flight(runtime: &mut PlaylistRuntime, request: u64) {
    let controller = runtime.controller.as_mut().expect("controller");
    assert!(matches!(
        controller.on_ready_to_commit(request_id(request)),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_id(request))
        .expect("dispatch");
    controller
        .resolve_authorization_dispatch(
            request_id(request),
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("enqueue barrier");
}

/// Player сообщил Installed для `request` — тот же runtime вход, что у `AppState`.
fn report_installed(runtime: &mut PlaylistRuntime, binding: PlaylistRuntimeBinding, request: u64) {
    runtime
        .register_successful_strong_install(
            request_id(request),
            player_request_id(request),
            media_instance_id(request),
            binding,
            ActiveMediaSource::LocalFile("fixture.webm".into()),
            player_core::PlaybackIntent::StartPlaying,
        )
        .expect("Installed registers playlist lineage");
}

/// Полный путь install-а от admission до Installed.
fn install_fully(
    runtime: &mut PlaylistRuntime,
    binding: PlaylistRuntimeBinding,
    install: PlannedPlaylistInstall,
    request: u64,
) {
    admit(runtime, install, request);
    advance_to_in_flight(runtime, request);
    report_installed(runtime, binding, request);
}

fn phase(runtime: &PlaylistRuntime) -> Option<ControllerInstallPhase> {
    runtime.controller.as_ref().and_then(|c| c.install_phase())
}

fn active_item(runtime: &PlaylistRuntime) -> Option<PlaylistItemId> {
    runtime
        .controller
        .as_ref()
        .and_then(|c| c.active_media())
        .and_then(|active| active.item_id())
}

fn row_guard(outcome: RuntimeRowPlayOutcome) -> TransportGuardOutcome {
    let RuntimeRowPlayOutcome::Controller(ControllerPlayItemOutcome::Guarded { guard, .. }) =
        outcome
    else {
        panic!("Play row during install must return the guard decision");
    };
    guard
}

fn navigation_guard(outcome: Option<ControllerManualNavigationOutcome>) -> TransportGuardOutcome {
    let Some(ControllerManualNavigationOutcome::Guarded(guard)) = outcome else {
        panic!("Next during foreign install must return the guard decision");
    };
    guard
}

#[test]
fn row_play_during_awaiting_ready_releases_old_request_and_installs_target() {
    let (mut runtime, binding, [a, b, _c]) = runtime_with_items();
    start_row_install(&mut runtime, a, 11);
    assert_eq!(phase(&runtime), Some(ControllerInstallPhase::AwaitingReady));

    let guard = row_guard(runtime.play_playlist_row(b));
    let GuardedTransportFollowUp::ExecuteAfterRelease {
        released,
        executed:
            DeferredTransportExecutionOutcome::PlayItem(ControllerPlayItemOutcome::StartInstall {
                install,
                ..
            }),
    } = runtime.resolve_transport_guard(guard, Duration::ZERO)
    else {
        panic!("Play B must release A and start B");
    };
    // App обязан отменить именно A с причиной Superseded — иначе A проиграется без binding.
    assert_eq!(
        released,
        ReleasedPendingRequest {
            request_id: request_id(11),
            cause: MediaInstallCancellationCause::Superseded,
        }
    );
    assert_eq!(install.item_id, b);

    install_fully(&mut runtime, binding, install, 12);
    assert_eq!(active_item(&runtime), Some(b), "B доходит до Installed");
    assert!(
        runtime
            .take_terminal_transport_execution(Duration::ZERO)
            .is_none(),
        "команда исполнена ровно один раз"
    );
}

#[test]
fn next_during_awaiting_ready_of_row_install_releases_request_and_moves_from_committed() {
    let (mut runtime, binding, [a, b, c]) = runtime_with_items();
    // A уже играет (committed current), затем Play row C ещё грузится.
    start_row_install(&mut runtime, a, 21);
    advance_to_in_flight(&mut runtime, 21);
    report_installed(&mut runtime, binding, 21);
    start_row_install(&mut runtime, c, 22);

    let guard = navigation_guard(runtime.request_playlist_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
    ));
    let GuardedTransportFollowUp::ExecuteAfterRelease {
        released,
        executed:
            DeferredTransportExecutionOutcome::Navigation(
                ControllerManualNavigationOutcome::StartInstall { install },
            ),
    } = runtime.resolve_transport_guard(guard, Duration::ZERO)
    else {
        panic!("Next must release C and navigate");
    };
    assert_eq!(released.request_id, request_id(22));
    // D08: незакоммиченный C не становится origin-ом; Next считается от committed A.
    assert_eq!(install.item_id, b);

    install_fully(&mut runtime, binding, install, 23);
    assert_eq!(active_item(&runtime), Some(b));
}

#[test]
fn row_play_during_in_flight_runs_once_after_installed() {
    let (mut runtime, binding, [a, b, _c]) = runtime_with_items();
    start_row_install(&mut runtime, a, 31);
    advance_to_in_flight(&mut runtime, 31);

    let guard = row_guard(runtime.play_playlist_row(b));
    assert!(matches!(
        runtime.resolve_transport_guard(guard, Duration::ZERO),
        GuardedTransportFollowUp::AwaitTerminal { request_id: parked }
            if parked == request_id(31)
    ));
    // Ожидание не трогает install, которым runtime не владеет.
    assert_eq!(
        phase(&runtime),
        Some(ControllerInstallPhase::AuthorizationInFlight)
    );
    assert!(
        runtime
            .take_terminal_transport_execution(Duration::ZERO)
            .is_none(),
        "до terminal исполнять нечего"
    );

    report_installed(&mut runtime, binding, 31);
    assert_eq!(
        active_item(&runtime),
        Some(a),
        "A остаётся playlist-строкой"
    );
    let Some(DeferredTransportExecutionOutcome::PlayItem(
        ControllerPlayItemOutcome::StartInstall { install, .. },
    )) = runtime.take_terminal_transport_execution(Duration::ZERO)
    else {
        panic!("отложенный Play row B исполняется после Installed A");
    };
    assert_eq!(install.item_id, b);
    assert!(
        runtime
            .take_terminal_transport_execution(Duration::ZERO)
            .is_none()
    );

    install_fully(&mut runtime, binding, install, 32);
    assert_eq!(active_item(&runtime), Some(b));
}

#[test]
fn next_during_in_flight_runs_relative_to_new_active() {
    let (mut runtime, binding, [a, b, _c]) = runtime_with_items();
    start_row_install(&mut runtime, a, 41);
    advance_to_in_flight(&mut runtime, 41);

    let guard = navigation_guard(runtime.request_playlist_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
    ));
    assert!(matches!(
        runtime.resolve_transport_guard(guard, Duration::ZERO),
        GuardedTransportFollowUp::AwaitTerminal { .. }
    ));

    report_installed(&mut runtime, binding, 41);
    let Some(DeferredTransportExecutionOutcome::Navigation(
        ControllerManualNavigationOutcome::StartInstall { install },
    )) = runtime.take_terminal_transport_execution(Duration::ZERO)
    else {
        panic!("отложенный Next исполняется после Installed A");
    };
    assert_eq!(install.item_id, b, "Next считается от нового active A");
}

#[test]
fn mpris_stop_during_in_flight_stops_installed_instance() {
    let (mut runtime, binding, [a, _b, _c]) = runtime_with_items();
    start_row_install(&mut runtime, a, 51);
    advance_to_in_flight(&mut runtime, 51);

    let Some(Err(guard)) = runtime.request_desktop_stop() else {
        panic!("Stop during install must return the guard decision");
    };
    assert!(matches!(
        runtime.resolve_transport_guard(guard, Duration::ZERO),
        GuardedTransportFollowUp::AwaitTerminal { .. }
    ));

    report_installed(&mut runtime, binding, 51);
    let Some(DeferredTransportExecutionOutcome::NeutralStop(Some(Ok(request)))) =
        runtime.take_terminal_transport_execution(Duration::ZERO)
    else {
        panic!("отложенный Stop адресует только что установленный instance");
    };
    assert_eq!(request.media_instance_id, media_instance_id(51));
    assert_eq!(request.action, ExactMediaTransportAction::NeutralStop);
}

#[test]
fn guard_without_pending_install_executes_immediately_without_release() {
    let (mut runtime, _binding, [a, _b, _c]) = runtime_with_items();
    let guard = TransportGuardOutcome::ExecuteNow {
        intent: DeferredTransportIntent::PlayItem {
            item_id: a,
            origin: TransportActionOrigin::Ui,
        },
        aborted_request_id: None,
        cancellation_cause: None,
        mode_dirty: None,
    };
    assert!(matches!(
        runtime.resolve_transport_guard(guard, Duration::ZERO),
        GuardedTransportFollowUp::Execute(DeferredTransportExecutionOutcome::PlayItem(
            ControllerPlayItemOutcome::StartInstall { .. }
        ))
    ));
}

#[test]
fn guard_resolution_without_controller_reports_unavailable_resource() {
    let mut runtime = PlaylistRuntime::new(wake_port());
    let guard = TransportGuardOutcome::CancelPendingThenExecute {
        request_id: request_id(61),
        cause: MediaInstallCancellationCause::Superseded,
        intent: DeferredTransportIntent::Stop {
            origin: TransportActionOrigin::Mpris,
        },
    };
    assert!(matches!(
        runtime.resolve_transport_guard(guard, Duration::ZERO),
        GuardedTransportFollowUp::ControllerUnavailable
    ));
    assert!(
        runtime
            .take_terminal_transport_execution(Duration::ZERO)
            .is_none()
    );
}

#[test]
fn fatal_guard_is_reported_without_execution() {
    let (mut runtime, _binding, _items) = runtime_with_items();
    let violation = PlaylistControllerInvariantViolation::UnexpectedInstallPhase;
    assert!(matches!(
        runtime.resolve_transport_guard(TransportGuardOutcome::Fatal(violation), Duration::ZERO),
        GuardedTransportFollowUp::Fatal(reported) if reported == violation
    ));
    assert_eq!(phase(&runtime), None, "fatal guard ничего не запускает");
}
