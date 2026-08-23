//! Functional regressions для AUD-011/AUD-012 queue/seek identity invariants.

use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackState};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistItemId, PlaylistLocator,
    PlaylistMediaKind,
};

use super::transport_execution::playlist_install_request;
use super::{PlaylistRuntime, RelativeBeyondEndNavigationOutcome};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::media_open::{AuthorizationDispatchResolution, MediaOpenRequestId};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, ControllerAppendOutcome,
    ControllerManualNavigationOutcome, ControllerPlayItemOutcome, EndedSnapshotKind,
    InstallReadyOutcome, PlannedPlaylistInstall, PlaylistController, PlaylistErrorBehavior,
    UnstagedPlannedTargetFailureOutcome,
};
use crate::playlist_runtime::identity::TransportActionOrigin;

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
}

/// Создаёт Installed A и exact automatic plan на B для functional failure-тестов.
fn automatic_transition_fixture() -> (PlaylistRuntime, [PlaylistItemId; 3], PlannedPlaylistInstall)
{
    let mut controller = PlaylistController::new();
    let appended_item_ids = match controller
        .append(
            (0..3)
                .map(|index| {
                    let label = format!("unstaged-transition-{index}.webm");
                    PlaylistItemDraft::local(
                        LocalLocator::Native(PathBuf::from(&label)),
                        None,
                        CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
                    )
                })
                .collect(),
        )
        .expect("append automatic transition fixture")
    {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
        ControllerAppendOutcome::NoItemsProvided => {
            panic!("automatic transition fixture is non-empty")
        }
    };
    let item_ids: [PlaylistItemId; 3] = appended_item_ids
        .try_into()
        .expect("fixture contains exactly A, B and C");

    let ControllerPlayItemOutcome::StartInstall { install, .. } =
        controller.play_item(item_ids[0], TransportActionOrigin::Ui)
    else {
        panic!("A starts strong install");
    };
    let first_request_id = MediaOpenRequestId::from_non_zero(non_zero(801));
    let first_player_request_id = MediaInstallRequestId::from_non_zero(non_zero(901));
    controller
        .accept_install_request(playlist_install_request(
            first_request_id,
            first_player_request_id,
            install,
        ))
        .expect("A install admission");
    assert!(matches!(
        controller.on_ready_to_commit(first_request_id),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(first_request_id)
        .expect("A authorization dispatch");
    controller
        .resolve_authorization_dispatch(
            first_request_id,
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("A enqueue barrier");
    controller
        .on_installed(
            first_request_id,
            first_player_request_id,
            MediaInstanceId::from_non_zero(non_zero(1001)),
            PlaylistBindingGeneration(1101),
        )
        .expect("A exact Installed");

    controller.set_error_behavior(PlaylistErrorBehavior::Skip);
    let active = controller.active_media().expect("A is active");
    let AutomaticLifecycleOutcome::OpenItem { install } = controller.observe_automatic_snapshot(
        active.player_binding_generation(),
        Some(active.media_instance_id()),
        PlaybackState::Ended,
        EndedSnapshotKind::Clean,
        AutomaticDeferredAvailability::Unavailable,
    ) else {
        panic!("clean EOF A starts automatic B transition");
    };
    assert_eq!(install.item_id, item_ids[1]);

    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.controller.install(controller);
    (runtime, item_ids, install)
}

#[test]
fn unstaged_automatic_failure_preserves_fixed_continuation_to_c() {
    // A уже Installed, а EOF создал automatic plan на B без request ID.
    let (mut runtime, item_ids, failed_b_install) = automatic_transition_fixture();

    // Синхронная source/pre-staging ошибка B должна потребить тот же opaque traversal plan.
    let UnstagedPlannedTargetFailureOutcome::OpenItem {
        install: continuation,
    } = runtime.report_unstaged_planned_playlist_navigation_failure(failed_b_install)
    else {
        panic!("Skip продолжает fixed traversal после unstaged B");
    };

    // Продолжение адресует C и не создаёт manual failed-anchor.
    assert_eq!(continuation.item_id, item_ids[2]);
    assert!(
        !runtime
            .controller
            .as_ref()
            .expect("controller remains installed")
            .view_snapshot()
            .awaiting_user_after_navigation_failure()
    );

    // Новый exact plan уже привязан к актуальной revision и реально проходит open gate C.
    let open_intent = runtime
        .media_open_intent_for_planned_install(&continuation)
        .expect("C continuation remains stageable");
    assert_eq!(
        open_intent.locator(),
        &PlaylistLocator::Local(LocalLocator::Native(PathBuf::from(
            "unstaged-transition-2.webm"
        )))
    );
}

#[test]
fn removed_automatic_target_still_advances_fixed_continuation_to_c() {
    // План на B уже существует, когда structural mutation удаляет B из живой очереди.
    let (mut runtime, item_ids, removed_b_install) = automatic_transition_fixture();
    let controller = runtime
        .controller
        .as_mut()
        .expect("controller remains installed");
    let _removal = controller.remove_item(item_ids[1]);
    assert!(controller.queue().item(item_ids[1]).is_none());

    // Stale pre-staging plan сохраняет automatic origin и продолжает его snapshot к C.
    let UnstagedPlannedTargetFailureOutcome::OpenItem {
        install: continuation,
    } = runtime.report_unstaged_planned_playlist_navigation_failure(removed_b_install)
    else {
        panic!("removed B does not lose automatic continuation");
    };
    assert_eq!(continuation.item_id, item_ids[2]);
    assert!(
        runtime
            .media_open_intent_for_planned_install(&continuation)
            .is_ok(),
        "C continuation uses the post-removal queue revision"
    );
}

#[test]
fn delayed_beyond_end_from_a_is_stale_after_b_installed_and_c_remains_unopened() {
    // Fixture даёт Installed A и automatic plan B; завершаем B через production controller path.
    let (mut runtime, item_ids, install_b) = automatic_transition_fixture();
    let request_b = MediaOpenRequestId::from_non_zero(non_zero(1201));
    let player_request_b = MediaInstallRequestId::from_non_zero(non_zero(1301));
    let media_b = MediaInstanceId::from_non_zero(non_zero(1401));
    runtime
        .accept_planned_playlist_install(request_b, player_request_b, install_b)
        .expect("B install admission");
    let controller = runtime
        .controller
        .as_mut()
        .expect("controller remains installed");
    assert!(matches!(
        controller.on_ready_to_commit(request_b),
        InstallReadyOutcome::RequestAuthorization { .. }
    ));
    controller
        .begin_authorization_dispatch(request_b)
        .expect("B authorization dispatch");
    controller
        .resolve_authorization_dispatch(
            request_b,
            AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
        )
        .expect("B enqueue barrier");
    controller
        .on_installed(
            request_b,
            player_request_b,
            media_b,
            PlaylistBindingGeneration(1501),
        )
        .expect("B exact Installed");

    // Delayed BeyondEnd(A) проверяется против authoritative active B и становится no-op.
    let media_a = MediaInstanceId::from_non_zero(non_zero(1001));
    assert!(matches!(
        runtime.request_relative_beyond_end_navigation(media_a, Duration::ZERO),
        RelativeBeyondEndNavigationOutcome::StaleInstance {
            outcome_media_instance_id,
            current_media_instance_id: Some(current_media_instance_id),
        } if outcome_media_instance_id == media_a && current_media_instance_id == media_b
    ));
    let active_after_stale = runtime
        .controller
        .as_ref()
        .and_then(PlaylistController::active_media)
        .expect("B stays active after stale A outcome");
    assert_eq!(active_after_stale.item_id(), Some(item_ids[1]));

    // Matching BeyondEnd(B) по-прежнему производит один normal Next plan на C.
    let RelativeBeyondEndNavigationOutcome::Navigation {
        outcome: ControllerManualNavigationOutcome::StartInstall { install },
    } = runtime.request_relative_beyond_end_navigation(media_b, Duration::ZERO)
    else {
        panic!("matching B outcome must route one Next action");
    };
    assert_eq!(install.item_id, item_ids[2]);
}

#[test]
fn repeated_unstaged_failures_keep_bounded_skip_budget_and_stop_after_all_candidates() {
    // A -> B -> C: оба remaining target-а синхронно падают, plan потребляется ровно по одному разу.
    let (mut runtime, _item_ids, failed_b_install) = automatic_transition_fixture();
    let UnstagedPlannedTargetFailureOutcome::OpenItem {
        install: failed_c_install,
    } = runtime.report_unstaged_planned_playlist_navigation_failure(failed_b_install)
    else {
        panic!("B failure advances to C");
    };
    assert!(matches!(
        runtime.report_unstaged_planned_playlist_navigation_failure(failed_c_install),
        UnstagedPlannedTargetFailureOutcome::Stopped {
            cause: crate::playlist_runtime::controller::AutomaticStopCause::AllCandidatesFailed {
                attempted_count: 2,
            }
        }
    ));
}
