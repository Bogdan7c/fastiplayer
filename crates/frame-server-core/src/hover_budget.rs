use std::num::NonZeroUsize;

/// Класс ресурса, по которому hover budget сравнивается с playback budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetResourceClass {
    HardwareSurfaceFrames,
    SoftwareFramePoolFrames,
    SoftwareThreadCount,
}

/// Пользовательская/runtime-настройка для одного hover budget ресурса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HoverBudgetSetting {
    #[default]
    Auto,
    Fixed(NonZeroUsize),
}

impl HoverBudgetSetting {
    #[must_use]
    pub const fn auto() -> Self {
        Self::Auto
    }

    pub fn fixed(value: usize) -> Result<Self, HoverPositiveBudgetError> {
        NonZeroUsize::new(value)
            .map(Self::Fixed)
            .ok_or(HoverPositiveBudgetError::ValueMustBePositive { value })
    }
}

/// Ошибка для budget значений, которые обязаны быть положительными.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverPositiveBudgetError {
    ValueMustBePositive { value: usize },
}

/// Playback-side budget для matching resource class.
///
/// `MissingPlayableOutput` оставляет отсутствие playable output типизированным
/// результатом, а не превращает его в общий resource-pressure/no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverPlaybackResourceBudget {
    Available(NonZeroUsize),
    MissingPlayableOutput,
}

impl HoverPlaybackResourceBudget {
    #[must_use]
    pub const fn available(value: NonZeroUsize) -> Self {
        Self::Available(value)
    }

    pub fn from_positive(value: usize) -> Result<Self, HoverPositiveBudgetError> {
        NonZeroUsize::new(value)
            .map(Self::Available)
            .ok_or(HoverPositiveBudgetError::ValueMustBePositive { value })
    }

    #[must_use]
    pub const fn missing_playable_output() -> Self {
        Self::MissingPlayableOutput
    }
}

/// Требование к одному ресурсу внутри active-playback hover budget request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HoverBudgetRequirement {
    resource_class: HoverBudgetResourceClass,
    setting: HoverBudgetSetting,
    playback_budget: HoverPlaybackResourceBudget,
}

impl HoverBudgetRequirement {
    #[must_use]
    pub const fn new(
        resource_class: HoverBudgetResourceClass,
        setting: HoverBudgetSetting,
        playback_budget: HoverPlaybackResourceBudget,
    ) -> Self {
        Self {
            resource_class,
            setting,
            playback_budget,
        }
    }

    #[must_use]
    pub const fn resource_class(self) -> HoverBudgetResourceClass {
        self.resource_class
    }

    #[must_use]
    pub const fn setting(self) -> HoverBudgetSetting {
        self.setting
    }

    #[must_use]
    pub const fn playback_budget(self) -> HoverPlaybackResourceBudget {
        self.playback_budget
    }
}

/// Полный budget request для одной попытки active hover executor/reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverBudgetRequest {
    requirements: Vec<HoverBudgetRequirement>,
}

impl HoverBudgetRequest {
    #[must_use]
    pub fn new(requirements: Vec<HoverBudgetRequirement>) -> Self {
        Self { requirements }
    }

    #[must_use]
    pub fn requirements(&self) -> &[HoverBudgetRequirement] {
        &self.requirements
    }
}

/// Backend-owned minimum report для текущего media/backend/session context.
///
/// `reported_minimum == 0` is intentionally not treated as a capable minimum:
/// it lets the resolver return a typed no-positive-minimum result instead of
/// silently inventing a generic fallback like `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HoverBudgetCapabilityMinimum {
    resource_class: HoverBudgetResourceClass,
    reported_minimum: usize,
}

impl HoverBudgetCapabilityMinimum {
    #[must_use]
    pub const fn reported(
        resource_class: HoverBudgetResourceClass,
        reported_minimum: usize,
    ) -> Self {
        Self {
            resource_class,
            reported_minimum,
        }
    }

    #[must_use]
    pub const fn resource_class(self) -> HoverBudgetResourceClass {
        self.resource_class
    }

    #[must_use]
    pub const fn reported_minimum(self) -> usize {
        self.reported_minimum
    }

    fn positive_minimum(self) -> Option<NonZeroUsize> {
        NonZeroUsize::new(self.reported_minimum)
    }
}

/// Capability snapshot, который backend/app owner передает для одного context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverBudgetCapability {
    minimums: Vec<HoverBudgetCapabilityMinimum>,
}

impl HoverBudgetCapability {
    #[must_use]
    pub fn new(minimums: Vec<HoverBudgetCapabilityMinimum>) -> Self {
        Self { minimums }
    }

    #[must_use]
    pub fn minimums(&self) -> &[HoverBudgetCapabilityMinimum] {
        &self.minimums
    }

    fn smallest_positive_minimum(
        &self,
        resource_class: HoverBudgetResourceClass,
    ) -> Option<NonZeroUsize> {
        self.minimums
            .iter()
            .copied()
            .filter(|minimum| minimum.resource_class() == resource_class)
            .filter_map(HoverBudgetCapabilityMinimum::positive_minimum)
            .min()
    }

    fn smallest_fitting_minimum(
        &self,
        resource_class: HoverBudgetResourceClass,
        playback_budget: NonZeroUsize,
    ) -> Option<NonZeroUsize> {
        self.minimums
            .iter()
            .copied()
            .filter(|minimum| minimum.resource_class() == resource_class)
            .filter_map(HoverBudgetCapabilityMinimum::positive_minimum)
            .filter(|minimum| *minimum < playback_budget)
            .min()
    }
}

/// Capability result до разрешения config-настроек `auto`/fixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverBudgetCapabilityReport {
    Supported(HoverBudgetCapability),
    Unsupported(HoverBudgetUnsupportedReason),
    Unavailable(HoverBudgetCapabilityUnavailableReason),
}

impl HoverBudgetCapabilityReport {
    #[must_use]
    pub fn supported(minimums: Vec<HoverBudgetCapabilityMinimum>) -> Self {
        Self::Supported(HoverBudgetCapability::new(minimums))
    }
}

/// `Unsupported` означает, что этот active-hover executor path невозможен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetUnsupportedReason {
    MissingPlayableOutput,
    BackendDoesNotSupportHover,
    UnsupportedResourceClass {
        resource_class: HoverBudgetResourceClass,
    },
}

/// `Unavailable` означает, что path существует, но текущий context не дает budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetCapabilityUnavailableReason {
    BackendNotReady,
    MediaContextUnavailable,
    ResourceProviderUnavailable,
}

/// Budget после разрешения `auto`/fixed для каждого запрошенного ресурса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverResolvedBudget {
    resources: Vec<HoverResolvedBudgetResource>,
}

impl HoverResolvedBudget {
    #[must_use]
    pub fn new(resources: Vec<HoverResolvedBudgetResource>) -> Self {
        Self { resources }
    }

    #[must_use]
    pub fn resources(&self) -> &[HoverResolvedBudgetResource] {
        &self.resources
    }

    #[must_use]
    pub fn budget_for(&self, resource_class: HoverBudgetResourceClass) -> Option<NonZeroUsize> {
        self.resources
            .iter()
            .find(|resource| resource.resource_class() == resource_class)
            .map(|resource| resource.resolved_budget())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HoverResolvedBudgetResource {
    resource_class: HoverBudgetResourceClass,
    resolved_budget: NonZeroUsize,
    source: HoverBudgetResolutionSource,
}

impl HoverResolvedBudgetResource {
    #[must_use]
    pub const fn new(
        resource_class: HoverBudgetResourceClass,
        resolved_budget: NonZeroUsize,
        source: HoverBudgetResolutionSource,
    ) -> Self {
        Self {
            resource_class,
            resolved_budget,
            source,
        }
    }

    #[must_use]
    pub const fn resource_class(self) -> HoverBudgetResourceClass {
        self.resource_class
    }

    #[must_use]
    pub const fn resolved_budget(self) -> NonZeroUsize {
        self.resolved_budget
    }

    #[must_use]
    pub const fn source(self) -> HoverBudgetResolutionSource {
        self.source
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetResolutionSource {
    BackendMinimumAuto,
    FixedConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverBudgetResolutionOutcome {
    Resolved(HoverResolvedBudget),
    Unsupported(HoverBudgetResolutionUnsupportedReason),
    Unavailable(HoverBudgetResolutionUnavailableReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetResolutionUnsupportedReason {
    NoResourceRequirements,
    DuplicateResourceRequirement {
        resource_class: HoverBudgetResourceClass,
    },
    MissingPlayableOutput {
        resource_class: HoverBudgetResourceClass,
    },
    Capability(HoverBudgetUnsupportedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetResolutionUnavailableReason {
    Capability(HoverBudgetCapabilityUnavailableReason),
    NoPositiveBackendMinimum {
        resource_class: HoverBudgetResourceClass,
    },
    NoFittingBackendMinimum {
        resource_class: HoverBudgetResourceClass,
        playback_budget: NonZeroUsize,
        smallest_positive_minimum: NonZeroUsize,
    },
    FixedBudgetBelowBackendMinimum {
        resource_class: HoverBudgetResourceClass,
        fixed_budget: NonZeroUsize,
        backend_minimum: NonZeroUsize,
    },
    FixedBudgetNotBelowPlayback {
        resource_class: HoverBudgetResourceClass,
        fixed_budget: NonZeroUsize,
        playback_budget: NonZeroUsize,
    },
}

/// Разрешает budget knobs для текущего context без кэша backend minima.
#[must_use]
pub fn resolve_hover_budget(
    request: &HoverBudgetRequest,
    capability_report: &HoverBudgetCapabilityReport,
) -> HoverBudgetResolutionOutcome {
    let capability = match capability_report {
        HoverBudgetCapabilityReport::Supported(capability) => capability,
        HoverBudgetCapabilityReport::Unsupported(reason) => {
            return HoverBudgetResolutionOutcome::Unsupported(
                HoverBudgetResolutionUnsupportedReason::Capability(*reason),
            );
        }
        HoverBudgetCapabilityReport::Unavailable(reason) => {
            return HoverBudgetResolutionOutcome::Unavailable(
                HoverBudgetResolutionUnavailableReason::Capability(*reason),
            );
        }
    };

    if request.requirements().is_empty() {
        return HoverBudgetResolutionOutcome::Unsupported(
            HoverBudgetResolutionUnsupportedReason::NoResourceRequirements,
        );
    }

    let mut resolved_resources = Vec::with_capacity(request.requirements().len());
    let mut seen_resource_classes = Vec::with_capacity(request.requirements().len());
    for requirement in request.requirements().iter().copied() {
        if seen_resource_classes.contains(&requirement.resource_class()) {
            return HoverBudgetResolutionOutcome::Unsupported(
                HoverBudgetResolutionUnsupportedReason::DuplicateResourceRequirement {
                    resource_class: requirement.resource_class(),
                },
            );
        }
        seen_resource_classes.push(requirement.resource_class());

        let resolved_resource = match resolve_requirement(requirement, capability) {
            Ok(resolved_resource) => resolved_resource,
            Err(outcome) => return outcome,
        };
        resolved_resources.push(resolved_resource);
    }

    HoverBudgetResolutionOutcome::Resolved(HoverResolvedBudget::new(resolved_resources))
}

fn resolve_requirement(
    requirement: HoverBudgetRequirement,
    capability: &HoverBudgetCapability,
) -> Result<HoverResolvedBudgetResource, HoverBudgetResolutionOutcome> {
    let resource_class = requirement.resource_class();
    let playback_budget = match requirement.playback_budget() {
        HoverPlaybackResourceBudget::Available(playback_budget) => playback_budget,
        HoverPlaybackResourceBudget::MissingPlayableOutput => {
            return Err(HoverBudgetResolutionOutcome::Unsupported(
                HoverBudgetResolutionUnsupportedReason::MissingPlayableOutput { resource_class },
            ));
        }
    };

    let Some(backend_minimum) = capability.smallest_positive_minimum(resource_class) else {
        return Err(HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::NoPositiveBackendMinimum { resource_class },
        ));
    };

    match requirement.setting() {
        HoverBudgetSetting::Auto => {
            resolve_auto_requirement(resource_class, playback_budget, backend_minimum, capability)
        }
        HoverBudgetSetting::Fixed(fixed_budget) => resolve_fixed_requirement(
            resource_class,
            playback_budget,
            backend_minimum,
            fixed_budget,
        ),
    }
}

fn resolve_auto_requirement(
    resource_class: HoverBudgetResourceClass,
    playback_budget: NonZeroUsize,
    smallest_positive_minimum: NonZeroUsize,
    capability: &HoverBudgetCapability,
) -> Result<HoverResolvedBudgetResource, HoverBudgetResolutionOutcome> {
    if let Some(resolved_budget) =
        capability.smallest_fitting_minimum(resource_class, playback_budget)
    {
        return Ok(HoverResolvedBudgetResource::new(
            resource_class,
            resolved_budget,
            HoverBudgetResolutionSource::BackendMinimumAuto,
        ));
    }

    Err(HoverBudgetResolutionOutcome::Unavailable(
        HoverBudgetResolutionUnavailableReason::NoFittingBackendMinimum {
            resource_class,
            playback_budget,
            smallest_positive_minimum,
        },
    ))
}

fn resolve_fixed_requirement(
    resource_class: HoverBudgetResourceClass,
    playback_budget: NonZeroUsize,
    backend_minimum: NonZeroUsize,
    fixed_budget: NonZeroUsize,
) -> Result<HoverResolvedBudgetResource, HoverBudgetResolutionOutcome> {
    if fixed_budget < backend_minimum {
        return Err(HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::FixedBudgetBelowBackendMinimum {
                resource_class,
                fixed_budget,
                backend_minimum,
            },
        ));
    }

    if fixed_budget >= playback_budget {
        return Err(HoverBudgetResolutionOutcome::Unavailable(
            HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
                resource_class,
                fixed_budget,
                playback_budget,
            },
        ));
    }

    Ok(HoverResolvedBudgetResource::new(
        resource_class,
        fixed_budget,
        HoverBudgetResolutionSource::FixedConfig,
    ))
}

/// Текущий provider pressure/reservation result после budget resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetAdmissionReport {
    Admitted,
    ResourcePressure(HoverBudgetResourcePressureReason),
    Unavailable(HoverBudgetAdmissionUnavailableReason),
    Fatal(HoverBudgetAdmissionFatalReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetResourcePressureReason {
    ActivePlaybackReservation,
    ExistingHoverReservation,
    ProviderCapacityExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetAdmissionUnavailableReason {
    ReservationOwnerUnavailable,
    ResourceProviderUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetAdmissionFatalReason {
    ProviderInvariantViolated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverBudgetAdmissionOutcome {
    Admitted(HoverResolvedBudget),
    Rejected {
        resolved_budget: HoverResolvedBudget,
        reason: HoverBudgetAdmissionRejection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HoverBudgetAdmissionRejection {
    ResourcePressure(HoverBudgetResourcePressureReason),
    Unavailable(HoverBudgetAdmissionUnavailableReason),
    Fatal(HoverBudgetAdmissionFatalReason),
}

/// Применяет admission без повторного budget resolution, чтобы pressure был отдельным результатом.
#[must_use]
pub fn admit_hover_budget(
    resolved_budget: HoverResolvedBudget,
    admission_report: HoverBudgetAdmissionReport,
) -> HoverBudgetAdmissionOutcome {
    match admission_report {
        HoverBudgetAdmissionReport::Admitted => {
            HoverBudgetAdmissionOutcome::Admitted(resolved_budget)
        }
        HoverBudgetAdmissionReport::ResourcePressure(reason) => {
            HoverBudgetAdmissionOutcome::Rejected {
                resolved_budget,
                reason: HoverBudgetAdmissionRejection::ResourcePressure(reason),
            }
        }
        HoverBudgetAdmissionReport::Unavailable(reason) => HoverBudgetAdmissionOutcome::Rejected {
            resolved_budget,
            reason: HoverBudgetAdmissionRejection::Unavailable(reason),
        },
        HoverBudgetAdmissionReport::Fatal(reason) => HoverBudgetAdmissionOutcome::Rejected {
            resolved_budget,
            reason: HoverBudgetAdmissionRejection::Fatal(reason),
        },
    }
}
