use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use player_core::{MediaInstanceId, PlaybackState};
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, ManualNavigationDirection, PlaylistItemDraft,
    PlaylistMediaKind,
};

use super::{AutomaticDiscoveryReadiness, DiscoveryNavigationInterest};
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticLifecycleOutcome, ControllerAppendOutcome,
    ControllerManualNavigationOutcome, DiscoveryManualWaitAvailability, EndedSnapshotKind,
    PlaylistController, PreviousRestartThreshold, SiblingDiscoveryScopeId,
};
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, ActiveMediaLineageId, TransportActionOrigin,
};

fn non_zero(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test identity is non-zero")
}

fn draft(index: usize) -> PlaylistItemDraft {
    let display_name = format!("discovery-navigation-{index}.mkv");
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from(&display_name)),
        None,
        CachedPlaylistMetadata::new(display_name, PlaylistMediaKind::Video),
    )
}

fn controller_with_active() -> (
    PlaylistController,
    playlist_core::PlaylistItemId,
    ActiveMediaIdentity,
) {
    let mut controller = PlaylistController::new();
    let ControllerAppendOutcome::Added { item_ids, .. } = controller
        .append(vec![draft(0)])
        .expect("fixture append succeeds")
    else {
        panic!("fixture is non-empty");
    };
    controller
        .queue
        .set_traversal_current(item_ids[0])
        .expect("fixture current commits");
    let active = ActiveMediaIdentity::installed(
        Some(item_ids[0]),
        ActiveMediaLineageId::from_non_zero(non_zero(11)),
        MediaInstanceId::from_non_zero(non_zero(12)),
        PlaylistBindingGeneration(13),
    );
    controller.active_media = Some(active);
    (controller, item_ids[0], active)
}

#[test]
fn admission_without_domain_target_keeps_same_manual_wait_and_no_mutation() {
    let (mut controller, current_item_id, _active) = controller_with_active();
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(21));
    let dirty_before = controller.dirty_revision();
    let revision_before = controller.queue().revision_snapshot();
    let ControllerManualNavigationOutcome::Waiting { wait_id, .. } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).expect("zero threshold is valid"),
        DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    ) else {
        panic!("missing committed next starts one wait");
    };

    assert!(matches!(
        controller.resume_manual_navigation_wait(wait_id, scope_id, false),
        ControllerManualNavigationOutcome::Waiting {
            wait_id: retained_wait,
            ..
        } if retained_wait == wait_id
    ));
    assert_eq!(
        controller
            .queue()
            .traversal_current()
            .map(|id| id.item_id()),
        Some(current_item_id)
    );
    assert_eq!(controller.queue().revision_snapshot(), revision_before);
    assert_eq!(controller.dirty_revision(), dirty_before);
}

#[test]
fn same_direction_coalesces_wait_and_opposite_supersedes_without_fifo() {
    let (mut controller, _current_item_id, _active) = controller_with_active();
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(22));
    let threshold = PreviousRestartThreshold::from_milliseconds(0).expect("valid threshold");
    let ControllerManualNavigationOutcome::Waiting { wait_id, .. } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        threshold,
        DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    ) else {
        panic!("first next waits");
    };
    assert!(matches!(
        controller.manual_navigation(
            ManualNavigationDirection::Next,
            TransportActionOrigin::Ui,
            Duration::ZERO,
            threshold,
            DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
        ),
        ControllerManualNavigationOutcome::Waiting { wait_id: same, .. } if same == wait_id
    ));
    let opposite = controller.manual_navigation(
        ManualNavigationDirection::Previous,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        threshold,
        DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    );
    assert!(matches!(
        opposite,
        ControllerManualNavigationOutcome::Waiting {
            wait_id: opposite_wait,
            direction: ManualNavigationDirection::Previous,
            ..
        } if opposite_wait != wait_id
    ));
    assert!(matches!(
        controller.discovery_navigation_interest(),
        DiscoveryNavigationInterest::Manual {
            direction: ManualNavigationDirection::Previous,
            ..
        }
    ));
}

#[test]
fn stale_exact_frontier_and_scan_cancel_never_start_or_resurrect_transition() {
    let (mut controller, current_item_id, _active) = controller_with_active();
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(25));
    let ControllerManualNavigationOutcome::Waiting { wait_id, .. } = controller.manual_navigation(
        ManualNavigationDirection::Next,
        TransportActionOrigin::Ui,
        Duration::ZERO,
        PreviousRestartThreshold::from_milliseconds(0).expect("zero threshold is valid"),
        DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id },
    ) else {
        panic!("missing committed next waits");
    };
    let _late_item_id = match controller
        .queue
        .append_one(draft(3))
        .expect("late committed admission succeeds")
    {
        playlist_core::AddItemsOutcome::Added(item_ids) => item_ids.as_slice()[0],
        playlist_core::AddItemsOutcome::NoItemsProvided => panic!("one draft is non-empty"),
    };
    assert!(matches!(
        controller.resume_manual_navigation_exact(wait_id, scope_id, current_item_id),
        ControllerManualNavigationOutcome::StaleWait { .. }
    ));
    assert!(matches!(
        controller.discovery_navigation_interest(),
        DiscoveryNavigationInterest::Manual {
            wait_id: retained_wait,
            ..
        } if retained_wait == wait_id
    ));
    assert!(controller.cancel_manual_navigation_wait(wait_id, scope_id));
    assert_eq!(
        controller.discovery_navigation_interest(),
        DiscoveryNavigationInterest::None
    );
    assert!(!controller.cancel_manual_navigation_wait(wait_id, scope_id));
}

#[test]
fn non_shuffle_deferred_ended_accepts_only_matching_exact_ready_once() {
    let (mut controller, current_item_id, active) = controller_with_active();
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(23));
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
    let appended = match controller
        .queue
        .append_one(draft(1))
        .expect("late committed admission succeeds")
    {
        playlist_core::AddItemsOutcome::Added(item_ids) => item_ids.as_slice()[0],
        playlist_core::AddItemsOutcome::NoItemsProvided => panic!("one draft is non-empty"),
    };
    assert!(matches!(
        controller.resume_deferred_automatic_advance(
            scope_id,
            AutomaticDiscoveryReadiness::ExactNaturalNext {
                item_id: current_item_id,
            },
        ),
        AutomaticLifecycleOutcome::NoAction
    ));
    let AutomaticLifecycleOutcome::OpenItem { install } = controller
        .resume_deferred_automatic_advance(
            scope_id,
            AutomaticDiscoveryReadiness::ExactNaturalNext { item_id: appended },
        )
    else {
        panic!("matching exact frontier releases one target");
    };
    assert_eq!(install.item_id, appended);
    assert!(matches!(
        controller.resume_deferred_automatic_advance(
            scope_id,
            AutomaticDiscoveryReadiness::ExactNaturalNext { item_id: appended },
        ),
        AutomaticLifecycleOutcome::NoAction
    ));
    assert_eq!(
        controller
            .queue()
            .traversal_current()
            .map(|id| id.item_id()),
        Some(current_item_id)
    );
}

#[test]
fn shuffle_admission_requeries_committed_upcoming_and_keeps_latch_without_target() {
    let (mut controller, current_item_id, active) = controller_with_active();
    controller
        .queue
        .enable_shuffle()
        .expect("fixture enables shuffle");
    let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(24));
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
        controller.resume_deferred_automatic_advance(
            scope_id,
            AutomaticDiscoveryReadiness::CommittedAdmissionAdvanced,
        ),
        AutomaticLifecycleOutcome::NoAction
    ));
    assert!(matches!(
        controller.discovery_navigation_interest(),
        DiscoveryNavigationInterest::Automatic { scope_id: active_scope, shuffle: true }
            if active_scope == scope_id
    ));

    let admitted_item_id = match controller
        .queue
        .append_one(draft(2))
        .expect("late shuffle admission commits")
    {
        playlist_core::AddItemsOutcome::Added(item_ids) => item_ids.as_slice()[0],
        playlist_core::AddItemsOutcome::NoItemsProvided => panic!("one draft is non-empty"),
    };
    let AutomaticLifecycleOutcome::OpenItem { install } = controller
        .resume_deferred_automatic_advance(
            scope_id,
            AutomaticDiscoveryReadiness::CommittedAdmissionAdvanced,
        )
    else {
        panic!("domain-owned upcoming releases the late admitted item");
    };
    assert_eq!(install.item_id, admitted_item_id);
    assert_eq!(
        controller
            .queue()
            .traversal_current()
            .map(|id| id.item_id()),
        Some(current_item_id)
    );
}
