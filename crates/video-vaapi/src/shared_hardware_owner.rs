use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use frame_server_core::hover_budget::{
    HoverBudgetAdmissionFatalReason, HoverBudgetAdmissionReport,
    HoverBudgetAdmissionUnavailableReason, HoverBudgetCapabilityMinimum,
    HoverBudgetCapabilityReport, HoverBudgetCapabilityUnavailableReason, HoverBudgetResourceClass,
    HoverBudgetResourcePressureReason, HoverResolvedBudget,
};
use tracing::warn;
use video_backend_api::{
    BackendHoverBudgetAdmissionFatalReason, BackendHoverBudgetAdmissionReport,
    BackendHoverBudgetAdmissionUnavailableReason, BackendHoverBudgetCapabilityMinimum,
    BackendHoverBudgetCapabilityReport, BackendHoverBudgetCapabilityUnavailableReason,
    BackendHoverBudgetResourceClass, BackendHoverBudgetResourcePressureReason,
    BackendHoverResolvedBudget, HoverBudgetDiagnosticsProvider,
};

/// VAAPI-local контекст, из которого owner строит capability/admission решения.
///
/// Здесь нет `Display`, surface id или cros типов: raw VA ownership остаётся
/// внутри decode thread, а этот boundary хранит только reservation accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VaapiSharedHardwareOwnerContext {
    playback_surface_budget: NonZeroUsize,
    hover_surface_capability_minimum: NonZeroUsize,
    hover_surface_provider_capacity: usize,
}

impl VaapiSharedHardwareOwnerContext {
    #[must_use]
    pub(crate) const fn new(
        playback_surface_budget: NonZeroUsize,
        hover_surface_capability_minimum: NonZeroUsize,
        hover_surface_provider_capacity: usize,
    ) -> Self {
        Self {
            playback_surface_budget,
            hover_surface_capability_minimum,
            hover_surface_provider_capacity,
        }
    }

    /// Строит контекст из текущего runtime accounting-а VA decoder-а.
    ///
    /// Hover minimum намеренно берётся из caller-provided backend window, а не
    /// из глобальной константы. Поэтому смена decoder context пересчитает report.
    #[must_use]
    pub(crate) fn from_surface_accounting(
        playback_surface_frames: usize,
        hover_surface_capability_minimum: usize,
    ) -> Self {
        let playback_surface_budget = non_zero_or_one(playback_surface_frames);
        let hover_surface_capability_minimum = non_zero_or_one(hover_surface_capability_minimum);
        let hover_surface_provider_capacity = playback_surface_budget
            .get()
            .saturating_sub(hover_surface_capability_minimum.get());

        Self::new(
            playback_surface_budget,
            hover_surface_capability_minimum,
            hover_surface_provider_capacity,
        )
    }

    #[must_use]
    pub(crate) const fn playback_surface_budget(self) -> NonZeroUsize {
        self.playback_surface_budget
    }

    #[must_use]
    pub(crate) const fn hover_surface_capability_minimum(self) -> NonZeroUsize {
        self.hover_surface_capability_minimum
    }

    #[must_use]
    pub(crate) const fn hover_surface_provider_capacity(self) -> usize {
        self.hover_surface_provider_capacity
    }
}

/// Shared VA hardware owner для playback/hover branch reservations.
///
/// V1 hosted внутри существующего decode thread: он не создаёт отдельный
/// `VADisplay` и не раскрывает VA internals наружу `video-vaapi`.
#[derive(Debug, Clone)]
pub(crate) struct VaapiSharedHardwareOwner {
    inner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
}

impl VaapiSharedHardwareOwner {
    #[must_use]
    pub(crate) fn new(context: VaapiSharedHardwareOwnerContext) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VaapiSharedHardwareOwnerInner::new(context))),
        }
    }

    #[must_use]
    pub(crate) fn hover_capability_report(&self) -> HoverBudgetCapabilityReport {
        match self.inner.lock() {
            Ok(inner) => inner.hover_capability_report(),
            Err(_) => HoverBudgetCapabilityReport::Unavailable(
                HoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable,
            ),
        }
    }

    pub(crate) fn reserve_playback_branch(
        &self,
    ) -> Result<VaapiPlaybackHardwareReservation, VaapiPlaybackReservationError> {
        let active_reservation = self
            .inner
            .lock()
            .map_err(|_| VaapiPlaybackReservationError::OwnerUnavailable)?
            .reserve_playback_branch()?;

        Ok(VaapiPlaybackHardwareReservation::new(
            self.inner.clone(),
            active_reservation,
        ))
    }

    // S22 вводит owner boundary до подключения реального hover executor-а.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(crate) fn admit_hover_reservation(
        &self,
        resolved_budget: &HoverResolvedBudget,
    ) -> VaapiHoverHardwareAdmission {
        let Some(requested_surface_frames) =
            resolved_budget.budget_for(HoverBudgetResourceClass::HardwareSurfaceFrames)
        else {
            return VaapiHoverHardwareAdmission::Rejected(HoverBudgetAdmissionReport::Unavailable(
                HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable,
            ));
        };

        match self.inner.lock() {
            Ok(mut inner) => match inner.reserve_hover_branch(requested_surface_frames) {
                Ok(active_reservation) => VaapiHoverHardwareAdmission::Admitted(
                    VaapiHoverHardwareReservation::new(self.inner.clone(), active_reservation),
                ),
                Err(rejection) => VaapiHoverHardwareAdmission::Rejected(rejection),
            },
            Err(_) => VaapiHoverHardwareAdmission::Rejected(HoverBudgetAdmissionReport::Fatal(
                HoverBudgetAdmissionFatalReason::ProviderInvariantViolated,
            )),
        }
    }

    #[must_use]
    pub(crate) fn hover_admission_report(
        &self,
        resolved_budget: &HoverResolvedBudget,
    ) -> HoverBudgetAdmissionReport {
        let Some(requested_surface_frames) =
            resolved_budget.budget_for(HoverBudgetResourceClass::HardwareSurfaceFrames)
        else {
            return HoverBudgetAdmissionReport::Unavailable(
                HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable,
            );
        };

        match self.inner.lock() {
            Ok(inner) => inner.hover_admission_report(requested_surface_frames),
            Err(_) => HoverBudgetAdmissionReport::Fatal(
                HoverBudgetAdmissionFatalReason::ProviderInvariantViolated,
            ),
        }
    }

    #[cfg(test)]
    fn snapshot_for_tests(&self) -> VaapiSharedHardwareOwnerSnapshot {
        let inner = self
            .inner
            .lock()
            .expect("test owner mutex must not be poisoned");

        VaapiSharedHardwareOwnerSnapshot {
            playback_active: inner.playback_reservation.is_some(),
            hover_active: inner.hover_reservation.is_some(),
            playback_surface_budget: inner.context.playback_surface_budget(),
            hover_surface_provider_capacity: inner.context.hover_surface_provider_capacity(),
        }
    }
}

impl HoverBudgetDiagnosticsProvider for VaapiSharedHardwareOwner {
    fn hover_capability_report(&self) -> BackendHoverBudgetCapabilityReport {
        backend_capability_report_from_core(VaapiSharedHardwareOwner::hover_capability_report(self))
    }

    fn hover_admission_report(
        &self,
        resolved_budget: &BackendHoverResolvedBudget,
    ) -> BackendHoverBudgetAdmissionReport {
        let core_budget = core_resolved_budget_from_backend(resolved_budget);
        backend_admission_report_from_core(VaapiSharedHardwareOwner::hover_admission_report(
            self,
            &core_budget,
        ))
    }
}

fn backend_capability_report_from_core(
    report: HoverBudgetCapabilityReport,
) -> BackendHoverBudgetCapabilityReport {
    match report {
        HoverBudgetCapabilityReport::Supported(capability) => {
            BackendHoverBudgetCapabilityReport::Supported(
                capability
                    .minimums()
                    .iter()
                    .copied()
                    .map(|minimum| {
                        BackendHoverBudgetCapabilityMinimum::reported(
                            resource_class_to_backend(minimum.resource_class()),
                            minimum.reported_minimum(),
                        )
                    })
                    .collect(),
            )
        }
        HoverBudgetCapabilityReport::Unsupported(reason) => {
            BackendHoverBudgetCapabilityReport::Unsupported(match reason {
                frame_server_core::HoverBudgetUnsupportedReason::MissingPlayableOutput => {
                    video_backend_api::BackendHoverBudgetUnsupportedReason::MissingPlayableOutput
                }
                frame_server_core::HoverBudgetUnsupportedReason::BackendDoesNotSupportHover => {
                    video_backend_api::BackendHoverBudgetUnsupportedReason::BackendDoesNotSupportHover
                }
                frame_server_core::HoverBudgetUnsupportedReason::UnsupportedResourceClass {
                    resource_class,
                } => video_backend_api::BackendHoverBudgetUnsupportedReason::UnsupportedResourceClass {
                    resource_class: resource_class_to_backend(resource_class),
                },
            })
        }
        HoverBudgetCapabilityReport::Unavailable(reason) => {
            BackendHoverBudgetCapabilityReport::Unavailable(match reason {
                HoverBudgetCapabilityUnavailableReason::BackendNotReady => {
                    BackendHoverBudgetCapabilityUnavailableReason::BackendNotReady
                }
                HoverBudgetCapabilityUnavailableReason::MediaContextUnavailable => {
                    BackendHoverBudgetCapabilityUnavailableReason::MediaContextUnavailable
                }
                HoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable => {
                    BackendHoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable
                }
            })
        }
    }
}

fn core_resolved_budget_from_backend(
    resolved_budget: &BackendHoverResolvedBudget,
) -> HoverResolvedBudget {
    HoverResolvedBudget::new(
        resolved_budget
            .resources()
            .iter()
            .copied()
            .filter_map(|resource| {
                NonZeroUsize::new(resource.resolved_budget()).map(|budget| {
                    frame_server_core::HoverResolvedBudgetResource::new(
                        resource_class_from_backend(resource.resource_class()),
                        budget,
                        frame_server_core::HoverBudgetResolutionSource::FixedConfig,
                    )
                })
            })
            .collect(),
    )
}

fn backend_admission_report_from_core(
    report: HoverBudgetAdmissionReport,
) -> BackendHoverBudgetAdmissionReport {
    match report {
        HoverBudgetAdmissionReport::Admitted => BackendHoverBudgetAdmissionReport::Admitted,
        HoverBudgetAdmissionReport::ResourcePressure(reason) => {
            BackendHoverBudgetAdmissionReport::ResourcePressure(match reason {
                HoverBudgetResourcePressureReason::ActivePlaybackReservation => {
                    BackendHoverBudgetResourcePressureReason::ActivePlaybackReservation
                }
                HoverBudgetResourcePressureReason::ExistingHoverReservation => {
                    BackendHoverBudgetResourcePressureReason::ExistingHoverReservation
                }
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted => {
                    BackendHoverBudgetResourcePressureReason::ProviderCapacityExhausted
                }
            })
        }
        HoverBudgetAdmissionReport::Unavailable(reason) => {
            BackendHoverBudgetAdmissionReport::Unavailable(match reason {
                HoverBudgetAdmissionUnavailableReason::ReservationOwnerUnavailable => {
                    BackendHoverBudgetAdmissionUnavailableReason::ReservationOwnerUnavailable
                }
                HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable => {
                    BackendHoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable
                }
            })
        }
        HoverBudgetAdmissionReport::Fatal(reason) => {
            BackendHoverBudgetAdmissionReport::Fatal(match reason {
                HoverBudgetAdmissionFatalReason::ProviderInvariantViolated => {
                    BackendHoverBudgetAdmissionFatalReason::ProviderInvariantViolated
                }
            })
        }
    }
}

fn resource_class_to_backend(
    resource_class: HoverBudgetResourceClass,
) -> BackendHoverBudgetResourceClass {
    match resource_class {
        HoverBudgetResourceClass::HardwareSurfaceFrames => {
            BackendHoverBudgetResourceClass::HardwareSurfaceFrames
        }
        HoverBudgetResourceClass::SoftwareFramePoolFrames => {
            BackendHoverBudgetResourceClass::SoftwareFramePoolFrames
        }
        HoverBudgetResourceClass::SoftwareThreadCount => {
            BackendHoverBudgetResourceClass::SoftwareThreadCount
        }
    }
}

fn resource_class_from_backend(
    resource_class: BackendHoverBudgetResourceClass,
) -> HoverBudgetResourceClass {
    match resource_class {
        BackendHoverBudgetResourceClass::HardwareSurfaceFrames => {
            HoverBudgetResourceClass::HardwareSurfaceFrames
        }
        BackendHoverBudgetResourceClass::SoftwareFramePoolFrames => {
            HoverBudgetResourceClass::SoftwareFramePoolFrames
        }
        BackendHoverBudgetResourceClass::SoftwareThreadCount => {
            HoverBudgetResourceClass::SoftwareThreadCount
        }
    }
}

#[derive(Debug)]
struct VaapiSharedHardwareOwnerInner {
    context: VaapiSharedHardwareOwnerContext,
    next_reservation_id: u64,
    playback_reservation: Option<ActiveHardwareReservation>,
    hover_reservation: Option<ActiveHardwareReservation>,
}

impl VaapiSharedHardwareOwnerInner {
    fn new(context: VaapiSharedHardwareOwnerContext) -> Self {
        Self {
            context,
            next_reservation_id: 1,
            playback_reservation: None,
            hover_reservation: None,
        }
    }

    fn hover_capability_report(&self) -> HoverBudgetCapabilityReport {
        HoverBudgetCapabilityReport::supported(vec![HoverBudgetCapabilityMinimum::reported(
            HoverBudgetResourceClass::HardwareSurfaceFrames,
            self.context.hover_surface_capability_minimum().get(),
        )])
    }

    fn reserve_playback_branch(
        &mut self,
    ) -> Result<ActiveHardwareReservation, VaapiPlaybackReservationError> {
        if self.playback_reservation.is_some() {
            return Err(VaapiPlaybackReservationError::ExistingPlaybackReservation);
        }

        let active_reservation = self.create_active_reservation(
            VaapiHardwareBranchKind::Playback,
            self.context.playback_surface_budget(),
        );
        self.playback_reservation = Some(active_reservation);

        Ok(active_reservation)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn reserve_hover_branch(
        &mut self,
        requested_surface_frames: NonZeroUsize,
    ) -> Result<ActiveHardwareReservation, HoverBudgetAdmissionReport> {
        match self.hover_admission_report(requested_surface_frames) {
            HoverBudgetAdmissionReport::Admitted => {}
            rejection => return Err(rejection),
        }

        let active_reservation = self
            .create_active_reservation(VaapiHardwareBranchKind::Hover, requested_surface_frames);
        self.hover_reservation = Some(active_reservation);

        Ok(active_reservation)
    }

    fn hover_admission_report(
        &self,
        requested_surface_frames: NonZeroUsize,
    ) -> HoverBudgetAdmissionReport {
        if self.playback_reservation.is_none() {
            return HoverBudgetAdmissionReport::Unavailable(
                HoverBudgetAdmissionUnavailableReason::ReservationOwnerUnavailable,
            );
        }

        if self.hover_reservation.is_some() {
            return HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ExistingHoverReservation,
            );
        }

        if requested_surface_frames >= self.context.playback_surface_budget() {
            return HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
            );
        }

        if requested_surface_frames.get() > self.context.hover_surface_provider_capacity() {
            return HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
            );
        }

        HoverBudgetAdmissionReport::Admitted
    }

    fn release_reservation(
        &mut self,
        branch_kind: VaapiHardwareBranchKind,
        reservation_id: VaapiHardwareReservationId,
        surface_frames: NonZeroUsize,
    ) -> VaapiHardwareReservationReleaseOutcome {
        let active_slot = match branch_kind {
            VaapiHardwareBranchKind::Playback => &mut self.playback_reservation,
            VaapiHardwareBranchKind::Hover => &mut self.hover_reservation,
        };

        match active_slot {
            Some(active_reservation)
                if active_reservation.reservation_id == reservation_id
                    && active_reservation.branch_kind == branch_kind =>
            {
                let release = VaapiHardwareReservationRelease {
                    branch_kind,
                    reservation_id,
                    surface_frames: active_reservation.surface_frames,
                };
                *active_slot = None;
                VaapiHardwareReservationReleaseOutcome::Released(release)
            }
            Some(_) | None => VaapiHardwareReservationReleaseOutcome::StaleReservation(
                VaapiHardwareReservationRelease {
                    branch_kind,
                    reservation_id,
                    surface_frames,
                },
            ),
        }
    }

    fn create_active_reservation(
        &mut self,
        branch_kind: VaapiHardwareBranchKind,
        surface_frames: NonZeroUsize,
    ) -> ActiveHardwareReservation {
        let reservation_id = VaapiHardwareReservationId(self.next_reservation_id);
        self.next_reservation_id = self.next_reservation_id.saturating_add(1);

        ActiveHardwareReservation {
            branch_kind,
            reservation_id,
            surface_frames,
        }
    }
}

/// Branch kind намеренно типизирован: release/admission код не передаёт мутный bool.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum VaapiHardwareBranchKind {
    Playback,
    Hover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct VaapiHardwareReservationId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveHardwareReservation {
    branch_kind: VaapiHardwareBranchKind,
    reservation_id: VaapiHardwareReservationId,
    surface_frames: NonZeroUsize,
}

#[derive(Debug)]
pub(crate) struct VaapiPlaybackHardwareReservation {
    token: VaapiHardwareReservationToken,
}

impl VaapiPlaybackHardwareReservation {
    fn new(
        owner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
        active: ActiveHardwareReservation,
    ) -> Self {
        Self {
            token: VaapiHardwareReservationToken::new(owner, active),
        }
    }

    #[must_use]
    pub(crate) fn surface_frames(&self) -> NonZeroUsize {
        self.token.surface_frames()
    }
}

impl Drop for VaapiPlaybackHardwareReservation {
    fn drop(&mut self) {
        self.token.release_for_drop();
    }
}

// S22 owner умеет выдавать hover reservation, но реальный executor появится позже.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct VaapiHoverHardwareReservation {
    token: VaapiHardwareReservationToken,
}

#[cfg_attr(not(test), allow(dead_code))]
impl VaapiHoverHardwareReservation {
    fn new(
        owner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
        active: ActiveHardwareReservation,
    ) -> Self {
        Self {
            token: VaapiHardwareReservationToken::new(owner, active),
        }
    }

    pub(crate) fn release(&mut self) -> VaapiHardwareReservationReleaseOutcome {
        self.token.release()
    }
}

impl Drop for VaapiHoverHardwareReservation {
    fn drop(&mut self) {
        self.token.release_for_drop();
    }
}

#[derive(Debug)]
struct VaapiHardwareReservationToken {
    owner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
    branch_kind: VaapiHardwareBranchKind,
    reservation_id: VaapiHardwareReservationId,
    surface_frames: NonZeroUsize,
    released: bool,
}

impl VaapiHardwareReservationToken {
    fn new(
        owner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
        active: ActiveHardwareReservation,
    ) -> Self {
        Self {
            owner,
            branch_kind: active.branch_kind,
            reservation_id: active.reservation_id,
            surface_frames: active.surface_frames,
            released: false,
        }
    }

    fn surface_frames(&self) -> NonZeroUsize {
        self.surface_frames
    }

    fn release(&mut self) -> VaapiHardwareReservationReleaseOutcome {
        if self.released {
            return VaapiHardwareReservationReleaseOutcome::AlreadyReleased(
                self.release_descriptor(),
            );
        }

        let outcome = match self.owner.lock() {
            Ok(mut owner) => owner.release_reservation(
                self.branch_kind,
                self.reservation_id,
                self.surface_frames,
            ),
            Err(_) => {
                VaapiHardwareReservationReleaseOutcome::OwnerUnavailable(self.release_descriptor())
            }
        };
        self.released = true;

        outcome
    }

    fn release_for_drop(&mut self) {
        match self.release() {
            VaapiHardwareReservationReleaseOutcome::Released(_)
            | VaapiHardwareReservationReleaseOutcome::AlreadyReleased(_) => {}
            VaapiHardwareReservationReleaseOutcome::OwnerUnavailable(release)
            | VaapiHardwareReservationReleaseOutcome::StaleReservation(release) => {
                warn!(
                    branch_kind = ?release.branch_kind,
                    reservation_id = release.reservation_id.0,
                    surface_frames = release.surface_frames.get(),
                    "VAAPI hardware reservation drop could not release active reservation"
                );
            }
        }
    }

    fn release_descriptor(&self) -> VaapiHardwareReservationRelease {
        VaapiHardwareReservationRelease {
            branch_kind: self.branch_kind,
            reservation_id: self.reservation_id,
            surface_frames: self.surface_frames,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VaapiHardwareReservationRelease {
    pub(crate) branch_kind: VaapiHardwareBranchKind,
    pub(crate) reservation_id: VaapiHardwareReservationId,
    pub(crate) surface_frames: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaapiHardwareReservationReleaseOutcome {
    Released(VaapiHardwareReservationRelease),
    AlreadyReleased(VaapiHardwareReservationRelease),
    OwnerUnavailable(VaapiHardwareReservationRelease),
    StaleReservation(VaapiHardwareReservationRelease),
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) enum VaapiHoverHardwareAdmission {
    Admitted(VaapiHoverHardwareReservation),
    Rejected(HoverBudgetAdmissionReport),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaapiPlaybackReservationError {
    OwnerUnavailable,
    ExistingPlaybackReservation,
}

impl std::fmt::Display for VaapiPlaybackReservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnerUnavailable => formatter.write_str("VAAPI hardware owner is unavailable"),
            Self::ExistingPlaybackReservation => {
                formatter.write_str("VAAPI playback reservation already exists")
            }
        }
    }
}

impl std::error::Error for VaapiPlaybackReservationError {}

fn non_zero_or_one(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VaapiSharedHardwareOwnerSnapshot {
    playback_active: bool,
    hover_active: bool,
    playback_surface_budget: NonZeroUsize,
    hover_surface_provider_capacity: usize,
}

#[cfg(test)]
mod tests;
