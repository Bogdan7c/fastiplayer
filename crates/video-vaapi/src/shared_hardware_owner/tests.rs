use std::num::NonZeroUsize;

use frame_server_core::hover_budget::{
    HoverBudgetAdmissionReport, HoverBudgetAdmissionUnavailableReason, HoverBudgetCapabilityReport,
    HoverBudgetCapabilityUnavailableReason, HoverBudgetResolutionSource, HoverBudgetResourceClass,
    HoverBudgetResourcePressureReason, HoverResolvedBudget, HoverResolvedBudgetResource,
};

use super::*;

fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test values must be non-zero")
}

fn owner_with_context(
    playback_surface_budget: usize,
    hover_surface_minimum: usize,
    hover_surface_provider_capacity: usize,
) -> VaapiSharedHardwareOwner {
    VaapiSharedHardwareOwner::new(VaapiSharedHardwareOwnerContext::new(
        nz(playback_surface_budget),
        nz(hover_surface_minimum),
        hover_surface_provider_capacity,
    ))
}

fn resolved_hardware_budget(surface_frames: usize) -> HoverResolvedBudget {
    HoverResolvedBudget::new(vec![HoverResolvedBudgetResource::new(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        nz(surface_frames),
        HoverBudgetResolutionSource::BackendMinimumAuto,
    )])
}

fn reported_hardware_minimum(report: HoverBudgetCapabilityReport) -> usize {
    match report {
        HoverBudgetCapabilityReport::Supported(capability) => capability
            .minimums()
            .iter()
            .find(|minimum| {
                minimum.resource_class() == HoverBudgetResourceClass::HardwareSurfaceFrames
            })
            .expect("hardware surface minimum must be reported")
            .reported_minimum(),
        HoverBudgetCapabilityReport::Unsupported(reason) => {
            panic!("capability must be supported, got unsupported: {reason:?}")
        }
        HoverBudgetCapabilityReport::Unavailable(reason) => {
            panic!("capability must be supported, got unavailable: {reason:?}")
        }
    }
}

#[test]
fn capability_minimum_uses_context_and_is_not_global_one() {
    let small_context = VaapiSharedHardwareOwnerContext::from_surface_accounting(18, 6);
    let larger_context = VaapiSharedHardwareOwnerContext::from_surface_accounting(30, 10);

    let small_owner = VaapiSharedHardwareOwner::new(small_context);
    let larger_owner = VaapiSharedHardwareOwner::new(larger_context);

    assert_eq!(
        reported_hardware_minimum(small_owner.hover_capability_report()),
        6
    );
    assert_eq!(
        reported_hardware_minimum(larger_owner.hover_capability_report()),
        10
    );
}

#[test]
fn hover_admission_requires_active_playback_reservation() {
    let owner = owner_with_context(12, 4, 8);

    let admission = owner.admit_hover_reservation(&resolved_hardware_budget(4));

    assert!(matches!(
        admission,
        VaapiHoverHardwareAdmission::Rejected(HoverBudgetAdmissionReport::Unavailable(
            HoverBudgetAdmissionUnavailableReason::ReservationOwnerUnavailable
        ))
    ));
}

#[test]
fn hover_admission_allows_only_one_active_hover_reservation() {
    let owner = owner_with_context(12, 4, 8);
    let _playback = owner
        .reserve_playback_branch()
        .expect("playback reservation must be admitted");

    let first_hover = owner.admit_hover_reservation(&resolved_hardware_budget(4));
    assert!(matches!(
        first_hover,
        VaapiHoverHardwareAdmission::Admitted(_)
    ));

    let second_hover = owner.admit_hover_reservation(&resolved_hardware_budget(4));
    assert!(matches!(
        second_hover,
        VaapiHoverHardwareAdmission::Rejected(HoverBudgetAdmissionReport::ResourcePressure(
            HoverBudgetResourcePressureReason::ExistingHoverReservation
        ))
    ));
}

#[test]
fn hover_admission_rejects_budget_at_or_above_playback_budget() {
    let owner = owner_with_context(12, 4, 12);
    let _playback = owner
        .reserve_playback_branch()
        .expect("playback reservation must be admitted");

    let admission = owner.admit_hover_reservation(&resolved_hardware_budget(12));

    assert!(matches!(
        admission,
        VaapiHoverHardwareAdmission::Rejected(HoverBudgetAdmissionReport::ResourcePressure(
            HoverBudgetResourcePressureReason::ProviderCapacityExhausted
        ))
    ));
}

#[test]
fn hover_admission_rejects_provider_pressure_separately_from_capability() {
    let owner = owner_with_context(12, 4, 3);
    let _playback = owner
        .reserve_playback_branch()
        .expect("playback reservation must be admitted");

    assert_eq!(
        reported_hardware_minimum(owner.hover_capability_report()),
        4
    );

    let admission = owner.admit_hover_reservation(&resolved_hardware_budget(4));

    assert!(matches!(
        admission,
        VaapiHoverHardwareAdmission::Rejected(HoverBudgetAdmissionReport::ResourcePressure(
            HoverBudgetResourcePressureReason::ProviderCapacityExhausted
        ))
    ));
}

#[test]
fn hover_release_does_not_release_playback_reservation() {
    let owner = owner_with_context(12, 4, 8);
    let _playback = owner
        .reserve_playback_branch()
        .expect("playback reservation must be admitted");
    let mut hover = match owner.admit_hover_reservation(&resolved_hardware_budget(4)) {
        VaapiHoverHardwareAdmission::Admitted(hover) => hover,
        VaapiHoverHardwareAdmission::Rejected(rejection) => {
            panic!("hover reservation must be admitted, got {rejection:?}")
        }
    };

    let release = hover.release();
    assert!(matches!(
        release,
        VaapiHardwareReservationReleaseOutcome::Released(VaapiHardwareReservationRelease {
            branch_kind: VaapiHardwareBranchKind::Hover,
            ..
        })
    ));

    let snapshot = owner.snapshot_for_tests();
    assert!(snapshot.playback_active);
    assert!(!snapshot.hover_active);

    assert!(matches!(
        hover.release(),
        VaapiHardwareReservationReleaseOutcome::AlreadyReleased(VaapiHardwareReservationRelease {
            branch_kind: VaapiHardwareBranchKind::Hover,
            ..
        })
    ));
}

#[test]
fn dropping_hover_releases_exactly_one_hover_slot() {
    let owner = owner_with_context(12, 4, 8);
    let _playback = owner
        .reserve_playback_branch()
        .expect("playback reservation must be admitted");

    {
        let _hover = match owner.admit_hover_reservation(&resolved_hardware_budget(4)) {
            VaapiHoverHardwareAdmission::Admitted(hover) => hover,
            VaapiHoverHardwareAdmission::Rejected(rejection) => {
                panic!("hover reservation must be admitted, got {rejection:?}")
            }
        };
        assert!(owner.snapshot_for_tests().hover_active);
    }

    let snapshot = owner.snapshot_for_tests();
    assert!(snapshot.playback_active);
    assert!(!snapshot.hover_active);
}

#[test]
fn hover_budget_without_hardware_surfaces_is_unavailable_for_va_owner() {
    let owner = owner_with_context(12, 4, 8);
    let _playback = owner
        .reserve_playback_branch()
        .expect("playback reservation must be admitted");
    let software_only_budget = HoverResolvedBudget::new(vec![HoverResolvedBudgetResource::new(
        HoverBudgetResourceClass::SoftwareThreadCount,
        nz(2),
        HoverBudgetResolutionSource::FixedConfig,
    )]);

    let admission = owner.admit_hover_reservation(&software_only_budget);

    assert!(matches!(
        admission,
        VaapiHoverHardwareAdmission::Rejected(HoverBudgetAdmissionReport::Unavailable(
            HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable
        ))
    ));
}

#[test]
fn poisoned_capability_lock_returns_typed_unavailable() {
    let owner = owner_with_context(12, 4, 8);
    let poisoned_owner = owner.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned_owner.inner.lock().expect("lock must be available");
        panic!("poison owner mutex for test");
    });

    assert!(matches!(
        owner.hover_capability_report(),
        HoverBudgetCapabilityReport::Unavailable(
            HoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable
        )
    ));
}
