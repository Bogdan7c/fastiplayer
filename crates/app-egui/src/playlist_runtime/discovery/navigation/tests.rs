use std::num::NonZeroU64;

use player_core::{MediaInstanceId, PlaybackState, PlayerError, PlayerErrorKind, PlayerSnapshot};
use playlist_core::ManualNavigationDirection;
use playlist_core::{CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind};
use playlist_discovery::AdmissionDirection;

use super::{AutomaticLifecycleOutcome, accept_monotonic_revision, admission_direction_matches};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::PlaylistRuntime;
use crate::playlist_runtime::controller::{ControllerAppendOutcome, PlaylistErrorBehavior};
use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity must be non-zero")
}

fn draft(name: &str) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(format!("/tmp/{name}").into()),
        None,
        CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
    )
}

fn runtime_with_active_first_item() -> (
    PlaylistRuntime,
    crate::playlist_runtime::PlaylistRuntimeBinding,
    Vec<playlist_core::PlaylistItemId>,
    ActiveMediaIdentity,
) {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    runtime.resolve_missing_state_for_test();
    let binding = runtime
        .bind_resumed_app_state()
        .expect("test runtime accepts an AppState binding");
    let item_ids = match runtime
        .controller
        .append(vec![draft("first.mkv"), draft("second.mkv")])
        .expect("fixture append succeeds")
    {
        ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
        ControllerAppendOutcome::NoItemsProvided => panic!("fixture contains two rows"),
    };
    runtime
        .controller
        .queue
        .set_traversal_current(item_ids[0])
        .expect("fixture current remains valid");
    let active = ActiveMediaIdentity::installed(
        Some(item_ids[0]),
        ActiveMediaLineageId::from_non_zero(non_zero(101)),
        MediaInstanceId::from_non_zero(non_zero(201)),
        binding.binding_generation(),
    );
    runtime.controller.active_media = Some(active);
    (runtime, binding, item_ids, active)
}

#[test]
fn latest_frontier_revision_rejects_duplicate_and_out_of_order_completion() {
    let mut revision = 0;
    assert!(accept_monotonic_revision(&mut revision, 3));
    assert!(!accept_monotonic_revision(&mut revision, 2));
    assert!(!accept_monotonic_revision(&mut revision, 3));
    assert!(accept_monotonic_revision(&mut revision, 5));
    assert_eq!(revision, 5);
}

#[test]
fn non_shuffle_direction_never_accepts_opposite_or_nondirectional_frontier() {
    assert!(admission_direction_matches(
        ManualNavigationDirection::Next,
        AdmissionDirection::After,
    ));
    assert!(admission_direction_matches(
        ManualNavigationDirection::Previous,
        AdmissionDirection::Before,
    ));
    assert!(!admission_direction_matches(
        ManualNavigationDirection::Next,
        AdmissionDirection::Before,
    ));
    assert!(!admission_direction_matches(
        ManualNavigationDirection::Previous,
        AdmissionDirection::NonDirectional,
    ));
}

#[test]
fn runtime_snapshot_boundary_routes_clean_eof_to_next_item_exactly_once() {
    let (mut runtime, binding, item_ids, active) = runtime_with_active_first_item();
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.media_instance_id = Some(active.media_instance_id());
    player_snapshot.playback_state = PlaybackState::Ended;
    player_snapshot.last_error = Some(PlayerError::new(
        PlayerErrorKind::RuntimeError,
        "устаревшая ошибка прошлого состояния",
    ));

    let first_outcome = runtime
        .observe_playlist_automatic_snapshot(binding, &player_snapshot)
        .expect("loaded runtime observes the player snapshot");
    assert!(matches!(
        first_outcome,
        AutomaticLifecycleOutcome::OpenItem { install }
            if install.item_id == item_ids[1]
    ));

    let repeated_outcome = runtime
        .observe_playlist_automatic_snapshot(binding, &player_snapshot)
        .expect("loaded runtime keeps observing the same snapshot");
    assert!(matches!(
        repeated_outcome,
        AutomaticLifecycleOutcome::NoAction
    ));
}

#[test]
fn runtime_snapshot_boundary_treats_failed_state_as_error_not_clean_eof() {
    let (mut runtime, binding, item_ids, active) = runtime_with_active_first_item();
    runtime
        .controller
        .set_error_behavior(PlaylistErrorBehavior::Skip);
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.media_instance_id = Some(active.media_instance_id());
    player_snapshot.playback_state = PlaybackState::Failed;
    player_snapshot.last_error = Some(PlayerError::new(
        PlayerErrorKind::DemuxError,
        "https://media.example.test/private.mkv?token=secret",
    ));

    let outcome = runtime
        .observe_playlist_automatic_snapshot(binding, &player_snapshot)
        .expect("loaded runtime observes the player failure");
    assert!(matches!(
        outcome,
        AutomaticLifecycleOutcome::OpenItem { install }
            if install.item_id == item_ids[1]
    ));
    let runtime_error = runtime
        .controller
        .runtime_errors
        .get(&item_ids[0])
        .expect("failed active item receives a runtime badge");
    assert!(runtime_error.safe_summary().contains("DemuxError"));
    assert!(!runtime_error.safe_summary().contains("secret"));
    assert!(!runtime_error.safe_summary().contains("private.mkv"));
}

/// Сессия 16 (решение владельца): пропажа сети посреди просмотра — не дефект элемента.
/// Даже при политике `skip` очередь останавливается, элемент не получает бейдж ошибки и
/// пользователь не видит ложного «пропущен из-за ошибки» (причину показывает центр окна).
#[test]
fn runtime_snapshot_boundary_stops_queue_on_network_loss_instead_of_skipping() {
    let (mut runtime, binding, item_ids, active) = runtime_with_active_first_item();
    runtime
        .controller
        .set_error_behavior(PlaylistErrorBehavior::Skip);
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.media_instance_id = Some(active.media_instance_id());
    player_snapshot.playback_state = PlaybackState::Failed;
    player_snapshot.last_error = Some(PlayerError::new(
        PlayerErrorKind::NetworkError,
        "Ошибка чтения packet: connection refused",
    ));

    let outcome = runtime
        .observe_playlist_automatic_snapshot(binding, &player_snapshot)
        .expect("loaded runtime observes the network failure");

    assert!(matches!(
        outcome,
        AutomaticLifecycleOutcome::Stop {
            cause: crate::playlist_runtime::controller::AutomaticStopCause::NetworkLost,
            ..
        }
    ));
    assert!(
        !runtime.controller.runtime_errors.contains_key(&item_ids[0]),
        "элемент с пропавшей сетью не помечается битым"
    );
    assert!(
        runtime
            .controller
            .drain_automatic_queue_notices()
            .is_empty(),
        "никакого «пропущен»/«остановлено на файле» при пропаже сети"
    );
    assert_eq!(
        runtime
            .controller
            .active_media
            .and_then(|media| media.item_id()),
        Some(item_ids[0]),
        "очередь не перешла к следующему элементу"
    );
}
