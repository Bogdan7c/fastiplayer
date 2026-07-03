use std::num::NonZeroUsize;

use crate::{
    HoverBudgetAdmissionOutcome, HoverBudgetAdmissionRejection, HoverBudgetAdmissionReport,
    HoverBudgetCapabilityMinimum, HoverBudgetCapabilityReport, HoverBudgetRequest,
    HoverBudgetRequirement, HoverBudgetResolutionOutcome, HoverBudgetResolutionSource,
    HoverBudgetResolutionUnavailableReason, HoverBudgetResolutionUnsupportedReason,
    HoverBudgetResourceClass, HoverBudgetResourcePressureReason, HoverBudgetSetting,
    HoverPlaybackResourceBudget, HoverPositiveBudgetError, admit_hover_budget,
    resolve_hover_budget,
};

fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test budget must be positive")
}

fn playback_budget(value: usize) -> HoverPlaybackResourceBudget {
    HoverPlaybackResourceBudget::available(nz(value))
}

fn auto_requirement(
    resource_class: HoverBudgetResourceClass,
    playback_value: usize,
) -> HoverBudgetRequirement {
    HoverBudgetRequirement::new(
        resource_class,
        HoverBudgetSetting::default(),
        playback_budget(playback_value),
    )
}

fn fixed_requirement(
    resource_class: HoverBudgetResourceClass,
    fixed_value: usize,
    playback_value: usize,
) -> HoverBudgetRequirement {
    HoverBudgetRequirement::new(
        resource_class,
        HoverBudgetSetting::fixed(fixed_value).expect("fixed test budget must be positive"),
        playback_budget(playback_value),
    )
}

fn supported_capability(
    resource_class: HoverBudgetResourceClass,
    reported_minimum: usize,
) -> HoverBudgetCapabilityReport {
    HoverBudgetCapabilityReport::supported(vec![HoverBudgetCapabilityMinimum::reported(
        resource_class,
        reported_minimum,
    )])
}

fn resolved_budget_for(
    outcome: HoverBudgetResolutionOutcome,
    resource_class: HoverBudgetResourceClass,
) -> NonZeroUsize {
    match outcome {
        HoverBudgetResolutionOutcome::Resolved(resolved_budget) => resolved_budget
            .budget_for(resource_class)
            .expect("resolved budget must contain requested resource"),
        other => panic!("expected resolved budget, got {other:?}"),
    }
}

#[test]
fn hover_auto_defaults_use_backend_reported_minimums() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        8,
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::HardwareSurfaceFrames, 3);

    let outcome = resolve_hover_budget(&request, &capability);

    match outcome {
        HoverBudgetResolutionOutcome::Resolved(resolved_budget) => {
            let resolved_resource = resolved_budget
                .resources()
                .first()
                .copied()
                .expect("one requested resource should resolve");
            assert_eq!(resolved_resource.resolved_budget(), nz(3));
            assert_eq!(
                resolved_resource.source(),
                HoverBudgetResolutionSource::BackendMinimumAuto
            );
        }
        other => panic!("expected resolved auto budget, got {other:?}"),
    }
}

#[test]
fn hover_auto_selects_smallest_reported_positive_minimum_not_one() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::SoftwareFramePoolFrames,
        12,
    )]);
    let capability = HoverBudgetCapabilityReport::supported(vec![
        HoverBudgetCapabilityMinimum::reported(
            HoverBudgetResourceClass::SoftwareFramePoolFrames,
            0,
        ),
        HoverBudgetCapabilityMinimum::reported(
            HoverBudgetResourceClass::SoftwareFramePoolFrames,
            7,
        ),
        HoverBudgetCapabilityMinimum::reported(
            HoverBudgetResourceClass::SoftwareFramePoolFrames,
            4,
        ),
    ]);

    let resolved_budget = resolved_budget_for(
        resolve_hover_budget(&request, &capability),
        HoverBudgetResourceClass::SoftwareFramePoolFrames,
    );

    assert_eq!(resolved_budget, nz(4));
}

#[test]
fn hover_auto_rejects_when_no_positive_backend_minimum() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::SoftwareThreadCount,
        8,
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::SoftwareThreadCount, 0);

    let outcome = resolve_hover_budget(&request, &capability);

    assert_eq!(
        outcome,
        HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::NoPositiveBackendMinimum {
                resource_class: HoverBudgetResourceClass::SoftwareThreadCount,
            }
        )
    );
}

#[test]
fn hover_auto_rejects_when_backend_minimum_does_not_fit_below_playback() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        8,
    )]);
    let capability = HoverBudgetCapabilityReport::supported(vec![
        HoverBudgetCapabilityMinimum::reported(HoverBudgetResourceClass::HardwareSurfaceFrames, 12),
        HoverBudgetCapabilityMinimum::reported(HoverBudgetResourceClass::HardwareSurfaceFrames, 10),
    ]);

    let outcome = resolve_hover_budget(&request, &capability);

    assert_eq!(
        outcome,
        HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::NoFittingBackendMinimum {
                resource_class: HoverBudgetResourceClass::HardwareSurfaceFrames,
                playback_budget: nz(8),
                smallest_positive_minimum: nz(10),
            }
        )
    );
}

#[test]
fn hover_auto_rejects_software_minimum_equal_to_playback_budget() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::SoftwareFramePoolFrames,
        4,
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::SoftwareFramePoolFrames, 4);

    let outcome = resolve_hover_budget(&request, &capability);

    assert_eq!(
        outcome,
        HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::NoFittingBackendMinimum {
                resource_class: HoverBudgetResourceClass::SoftwareFramePoolFrames,
                playback_budget: nz(4),
                smallest_positive_minimum: nz(4),
            }
        )
    );
}

#[test]
fn hover_fixed_budget_rejects_zero_and_has_no_static_upper_cap() {
    assert_eq!(
        HoverBudgetSetting::fixed(0),
        Err(HoverPositiveBudgetError::ValueMustBePositive { value: 0 })
    );

    let request = HoverBudgetRequest::new(vec![fixed_requirement(
        HoverBudgetResourceClass::SoftwareFramePoolFrames,
        100_000,
        100_001,
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::SoftwareFramePoolFrames, 1);

    let resolved_budget = resolved_budget_for(
        resolve_hover_budget(&request, &capability),
        HoverBudgetResourceClass::SoftwareFramePoolFrames,
    );

    assert_eq!(resolved_budget, nz(100_000));
}

#[test]
fn hover_auto_does_not_use_playback_minus_one_maximize_policy() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        10,
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::HardwareSurfaceFrames, 3);

    let resolved_budget = resolved_budget_for(
        resolve_hover_budget(&request, &capability),
        HoverBudgetResourceClass::HardwareSurfaceFrames,
    );

    assert_eq!(resolved_budget, nz(3));
    assert_ne!(resolved_budget, nz(9));
}

#[test]
fn hover_pairwise_requires_hardware_surface_budget_below_playback() {
    let request = HoverBudgetRequest::new(vec![fixed_requirement(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        8,
        8,
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::HardwareSurfaceFrames, 1);

    let outcome = resolve_hover_budget(&request, &capability);

    assert_eq!(
        outcome,
        HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
                resource_class: HoverBudgetResourceClass::HardwareSurfaceFrames,
                fixed_budget: nz(8),
                playback_budget: nz(8),
            }
        )
    );
}

#[test]
fn hover_pairwise_requires_software_pool_and_thread_budgets_below_playback() {
    let pool_request = HoverBudgetRequest::new(vec![fixed_requirement(
        HoverBudgetResourceClass::SoftwareFramePoolFrames,
        8,
        8,
    )]);
    let pool_capability =
        supported_capability(HoverBudgetResourceClass::SoftwareFramePoolFrames, 1);

    assert_eq!(
        resolve_hover_budget(&pool_request, &pool_capability),
        HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
                resource_class: HoverBudgetResourceClass::SoftwareFramePoolFrames,
                fixed_budget: nz(8),
                playback_budget: nz(8),
            }
        )
    );

    let thread_request = HoverBudgetRequest::new(vec![fixed_requirement(
        HoverBudgetResourceClass::SoftwareThreadCount,
        4,
        4,
    )]);
    let thread_capability = supported_capability(HoverBudgetResourceClass::SoftwareThreadCount, 1);

    assert_eq!(
        resolve_hover_budget(&thread_request, &thread_capability),
        HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
                resource_class: HoverBudgetResourceClass::SoftwareThreadCount,
                fixed_budget: nz(4),
                playback_budget: nz(4),
            }
        )
    );
}

#[test]
fn hover_budget_has_no_cross_resource_compensation() {
    let request = HoverBudgetRequest::new(vec![
        fixed_requirement(HoverBudgetResourceClass::SoftwareFramePoolFrames, 1, 8),
        fixed_requirement(HoverBudgetResourceClass::SoftwareThreadCount, 4, 4),
    ]);
    let capability = HoverBudgetCapabilityReport::supported(vec![
        HoverBudgetCapabilityMinimum::reported(
            HoverBudgetResourceClass::SoftwareFramePoolFrames,
            1,
        ),
        HoverBudgetCapabilityMinimum::reported(HoverBudgetResourceClass::SoftwareThreadCount, 1),
    ]);

    let outcome = resolve_hover_budget(&request, &capability);

    assert_eq!(
        outcome,
        HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
                resource_class: HoverBudgetResourceClass::SoftwareThreadCount,
                fixed_budget: nz(4),
                playback_budget: nz(4),
            }
        )
    );
}

#[test]
fn hover_capability_success_then_admission_pressure_failure_remains_distinct() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        8,
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::HardwareSurfaceFrames, 3);
    let resolved_budget = match resolve_hover_budget(&request, &capability) {
        HoverBudgetResolutionOutcome::Resolved(resolved_budget) => resolved_budget,
        other => panic!("expected capability resolution success, got {other:?}"),
    };

    let admission_outcome = admit_hover_budget(
        resolved_budget,
        HoverBudgetAdmissionReport::ResourcePressure(
            HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
        ),
    );

    match admission_outcome {
        HoverBudgetAdmissionOutcome::Rejected { reason, .. } => assert_eq!(
            reason,
            HoverBudgetAdmissionRejection::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted
            )
        ),
        other => panic!("expected admission pressure rejection, got {other:?}"),
    }
}

#[test]
fn hover_context_change_recomputes_backend_minimum_without_global_cache() {
    let request = HoverBudgetRequest::new(vec![auto_requirement(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        10,
    )]);
    let first_context = supported_capability(HoverBudgetResourceClass::HardwareSurfaceFrames, 4);
    let second_context = supported_capability(HoverBudgetResourceClass::HardwareSurfaceFrames, 6);

    let first_budget = resolved_budget_for(
        resolve_hover_budget(&request, &first_context),
        HoverBudgetResourceClass::HardwareSurfaceFrames,
    );
    let second_budget = resolved_budget_for(
        resolve_hover_budget(&request, &second_context),
        HoverBudgetResourceClass::HardwareSurfaceFrames,
    );

    assert_eq!(first_budget, nz(4));
    assert_eq!(second_budget, nz(6));
}

#[test]
fn hover_missing_playable_output_is_typed_unsupported() {
    let request = HoverBudgetRequest::new(vec![HoverBudgetRequirement::new(
        HoverBudgetResourceClass::HardwareSurfaceFrames,
        HoverBudgetSetting::auto(),
        HoverPlaybackResourceBudget::missing_playable_output(),
    )]);
    let capability = supported_capability(HoverBudgetResourceClass::HardwareSurfaceFrames, 3);

    let outcome = resolve_hover_budget(&request, &capability);

    assert_eq!(
        outcome,
        HoverBudgetResolutionOutcome::Unsupported(
            HoverBudgetResolutionUnsupportedReason::MissingPlayableOutput {
                resource_class: HoverBudgetResourceClass::HardwareSurfaceFrames,
            }
        )
    );
}

#[test]
fn hover_duplicate_resource_requirement_is_typed_unsupported() {
    let request = HoverBudgetRequest::new(vec![
        auto_requirement(HoverBudgetResourceClass::SoftwareThreadCount, 8),
        fixed_requirement(HoverBudgetResourceClass::SoftwareThreadCount, 2, 8),
    ]);
    let capability = supported_capability(HoverBudgetResourceClass::SoftwareThreadCount, 2);

    let outcome = resolve_hover_budget(&request, &capability);

    assert_eq!(
        outcome,
        HoverBudgetResolutionOutcome::Unsupported(
            HoverBudgetResolutionUnsupportedReason::DuplicateResourceRequirement {
                resource_class: HoverBudgetResourceClass::SoftwareThreadCount,
            }
        )
    );
}
