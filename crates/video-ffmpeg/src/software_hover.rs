//! Software hover budget owner for the FFmpeg backend.
//!
//! Модуль не открывает codec и не содержит raw FFmpeg types. Его задача уже:
//! держать FFmpeg/software-owned capability/admission accounting рядом с
//! concrete backend crate-ом, чтобы `frame-server-core` не угадывал minimums.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use frame_server_core::hover_budget::{
    HoverBudgetAdmissionFatalReason, HoverBudgetAdmissionReport,
    HoverBudgetAdmissionUnavailableReason, HoverBudgetCapabilityMinimum,
    HoverBudgetCapabilityReport, HoverBudgetCapabilityUnavailableReason, HoverBudgetResourceClass,
    HoverBudgetResourcePressureReason, HoverResolvedBudget,
};
use video_core::{SoftwareDecodeThreadBudget, VideoDecoderThreadConfig};

const DEFAULT_HOVER_SOFTWARE_FRAME_POOL_MINIMUM: usize = 2;

/// FFmpeg/software-local context для одного playback/backend/session budget snapshot-а.
///
/// Хранит только neutral accounting: сколько host-frame slots и decoder threads
/// есть у playback, какие minimums текущий backend считает способными для hover,
/// и какую provider capacity сейчас можно пробовать резервировать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegSoftwareHoverContext {
    playback_frame_pool_budget: NonZeroUsize,
    playback_thread_budget: NonZeroUsize,
    hover_frame_pool_capability_minimum: NonZeroUsize,
    hover_thread_capability_minimum: NonZeroUsize,
    hover_frame_pool_provider_capacity: usize,
    hover_thread_provider_capacity: usize,
}

impl FfmpegSoftwareHoverContext {
    #[must_use]
    pub const fn new(
        playback_frame_pool_budget: NonZeroUsize,
        playback_thread_budget: NonZeroUsize,
        hover_frame_pool_capability_minimum: NonZeroUsize,
        hover_thread_capability_minimum: NonZeroUsize,
        hover_frame_pool_provider_capacity: usize,
        hover_thread_provider_capacity: usize,
    ) -> Self {
        Self {
            playback_frame_pool_budget,
            playback_thread_budget,
            hover_frame_pool_capability_minimum,
            hover_thread_capability_minimum,
            hover_frame_pool_provider_capacity,
            hover_thread_provider_capacity,
        }
    }

    /// Строит context из текущего playback decoder config-а без кэша minima.
    ///
    /// `Auto` playback thread budget остаётся `thread_count = 0` на FFmpeg open,
    /// но для hover policy нужно concrete сравнение. Здесь оно считается как
    /// текущая host CPU parallelism snapshot; это не меняет playback config и не
    /// передаёт raw FFmpeg details наружу.
    #[must_use]
    pub fn from_playback_decoder_config(playback_config: VideoDecoderThreadConfig) -> Self {
        let normalized_config = playback_config.normalized();
        let playback_frame_pool_budget =
            non_zero_or_one(normalized_config.software_frame_pool_frames);
        let playback_thread_budget = playback_thread_budget_from_decoder_config(normalized_config);
        let hover_frame_pool_capability_minimum =
            non_zero_or_one(DEFAULT_HOVER_SOFTWARE_FRAME_POOL_MINIMUM);
        let hover_thread_capability_minimum = default_hover_thread_capability_minimum();

        Self::new(
            playback_frame_pool_budget,
            playback_thread_budget,
            hover_frame_pool_capability_minimum,
            hover_thread_capability_minimum,
            playback_frame_pool_budget
                .get()
                .saturating_sub(hover_frame_pool_capability_minimum.get()),
            playback_thread_budget
                .get()
                .saturating_sub(hover_thread_capability_minimum.get()),
        )
    }

    #[must_use]
    pub const fn playback_frame_pool_budget(self) -> NonZeroUsize {
        self.playback_frame_pool_budget
    }

    #[must_use]
    pub const fn playback_thread_budget(self) -> NonZeroUsize {
        self.playback_thread_budget
    }
}

/// Provider-owned accounting boundary для max-one active software hover session.
#[derive(Debug, Clone)]
pub struct FfmpegSoftwareHoverOwner {
    inner: Arc<Mutex<FfmpegSoftwareHoverOwnerInner>>,
}

impl FfmpegSoftwareHoverOwner {
    #[must_use]
    pub fn new(context: FfmpegSoftwareHoverContext) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FfmpegSoftwareHoverOwnerInner::new(context))),
        }
    }

    #[must_use]
    pub fn context(&self) -> Option<FfmpegSoftwareHoverContext> {
        self.inner.lock().ok().map(|inner| inner.context)
    }

    #[must_use]
    pub fn hover_capability_report(&self) -> HoverBudgetCapabilityReport {
        match self.inner.lock() {
            Ok(inner) => inner.hover_capability_report(),
            Err(_) => HoverBudgetCapabilityReport::Unavailable(
                HoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable,
            ),
        }
    }

    #[must_use]
    pub fn admit_hover_reservation(
        &self,
        resolved_budget: &HoverResolvedBudget,
    ) -> FfmpegSoftwareHoverAdmission {
        let Some(requested_frame_pool_frames) =
            resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareFramePoolFrames)
        else {
            return FfmpegSoftwareHoverAdmission::Rejected(
                HoverBudgetAdmissionReport::Unavailable(
                    HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable,
                ),
            );
        };
        let Some(requested_thread_count) =
            resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareThreadCount)
        else {
            return FfmpegSoftwareHoverAdmission::Rejected(
                HoverBudgetAdmissionReport::Unavailable(
                    HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable,
                ),
            );
        };

        match self.inner.lock() {
            Ok(mut inner) => {
                match inner
                    .reserve_hover_branch(requested_frame_pool_frames, requested_thread_count)
                {
                    Ok(active_reservation) => FfmpegSoftwareHoverAdmission::Admitted(
                        FfmpegSoftwareHoverReservation::new(self.inner.clone(), active_reservation),
                    ),
                    Err(rejection) => FfmpegSoftwareHoverAdmission::Rejected(rejection),
                }
            }
            Err(_) => FfmpegSoftwareHoverAdmission::Rejected(HoverBudgetAdmissionReport::Fatal(
                HoverBudgetAdmissionFatalReason::ProviderInvariantViolated,
            )),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Option<FfmpegSoftwareHoverSnapshot> {
        let inner = self.inner.lock().ok()?;

        Some(FfmpegSoftwareHoverSnapshot {
            hover_active: inner.hover_reservation.is_some(),
            playback_frame_pool_budget: inner.context.playback_frame_pool_budget,
            playback_thread_budget: inner.context.playback_thread_budget,
            hover_frame_pool_provider_capacity: inner.context.hover_frame_pool_provider_capacity,
            hover_thread_provider_capacity: inner.context.hover_thread_provider_capacity,
        })
    }
}

#[derive(Debug)]
struct FfmpegSoftwareHoverOwnerInner {
    context: FfmpegSoftwareHoverContext,
    next_reservation_id: u64,
    hover_reservation: Option<ActiveSoftwareHoverReservation>,
}

impl FfmpegSoftwareHoverOwnerInner {
    fn new(context: FfmpegSoftwareHoverContext) -> Self {
        Self {
            context,
            next_reservation_id: 1,
            hover_reservation: None,
        }
    }

    fn hover_capability_report(&self) -> HoverBudgetCapabilityReport {
        HoverBudgetCapabilityReport::supported(vec![
            HoverBudgetCapabilityMinimum::reported(
                HoverBudgetResourceClass::SoftwareFramePoolFrames,
                self.context.hover_frame_pool_capability_minimum.get(),
            ),
            HoverBudgetCapabilityMinimum::reported(
                HoverBudgetResourceClass::SoftwareThreadCount,
                self.context.hover_thread_capability_minimum.get(),
            ),
        ])
    }

    fn reserve_hover_branch(
        &mut self,
        requested_frame_pool_frames: NonZeroUsize,
        requested_thread_count: NonZeroUsize,
    ) -> Result<ActiveSoftwareHoverReservation, HoverBudgetAdmissionReport> {
        if self.hover_reservation.is_some() {
            return Err(HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ExistingHoverReservation,
            ));
        }

        if requested_frame_pool_frames >= self.context.playback_frame_pool_budget
            || requested_thread_count >= self.context.playback_thread_budget
        {
            return Err(HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
            ));
        }

        if requested_frame_pool_frames.get() > self.context.hover_frame_pool_provider_capacity
            || requested_thread_count.get() > self.context.hover_thread_provider_capacity
        {
            return Err(HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
            ));
        }

        let active_reservation = ActiveSoftwareHoverReservation {
            reservation_id: FfmpegSoftwareHoverReservationId(self.next_reservation_id),
            frame_pool_frames: requested_frame_pool_frames,
            thread_count: requested_thread_count,
        };
        self.next_reservation_id = self.next_reservation_id.saturating_add(1);
        self.hover_reservation = Some(active_reservation);

        Ok(active_reservation)
    }

    fn release_hover_reservation(
        &mut self,
        reservation_id: FfmpegSoftwareHoverReservationId,
    ) -> bool {
        match self.hover_reservation {
            Some(active_reservation) if active_reservation.reservation_id == reservation_id => {
                self.hover_reservation = None;
                true
            }
            Some(_) | None => false,
        }
    }
}

#[derive(Debug)]
pub enum FfmpegSoftwareHoverAdmission {
    Admitted(FfmpegSoftwareHoverReservation),
    Rejected(HoverBudgetAdmissionReport),
}

/// RAII reservation token. Drop освобождает provider accounting ровно один раз.
#[derive(Debug)]
pub struct FfmpegSoftwareHoverReservation {
    owner: Arc<Mutex<FfmpegSoftwareHoverOwnerInner>>,
    active_reservation: ActiveSoftwareHoverReservation,
    released: bool,
}

impl FfmpegSoftwareHoverReservation {
    fn new(
        owner: Arc<Mutex<FfmpegSoftwareHoverOwnerInner>>,
        active_reservation: ActiveSoftwareHoverReservation,
    ) -> Self {
        Self {
            owner,
            active_reservation,
            released: false,
        }
    }

    #[must_use]
    pub const fn frame_pool_frames(&self) -> NonZeroUsize {
        self.active_reservation.frame_pool_frames
    }

    #[must_use]
    pub const fn thread_count(&self) -> NonZeroUsize {
        self.active_reservation.thread_count
    }

    pub fn release(mut self) {
        self.release_once();
    }

    fn release_once(&mut self) {
        if self.released {
            return;
        }
        self.released = true;

        if let Ok(mut inner) = self.owner.lock() {
            let _released = inner.release_hover_reservation(self.active_reservation.reservation_id);
        }
    }
}

impl Drop for FfmpegSoftwareHoverReservation {
    fn drop(&mut self) {
        self.release_once();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveSoftwareHoverReservation {
    reservation_id: FfmpegSoftwareHoverReservationId,
    frame_pool_frames: NonZeroUsize,
    thread_count: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FfmpegSoftwareHoverReservationId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegSoftwareHoverSnapshot {
    pub hover_active: bool,
    pub playback_frame_pool_budget: NonZeroUsize,
    pub playback_thread_budget: NonZeroUsize,
    pub hover_frame_pool_provider_capacity: usize,
    pub hover_thread_provider_capacity: usize,
}

#[must_use]
pub fn playback_thread_budget_from_decoder_config(
    config: VideoDecoderThreadConfig,
) -> NonZeroUsize {
    match config.software_decode_thread_budget {
        SoftwareDecodeThreadBudget::Auto => {
            std::thread::available_parallelism().unwrap_or_else(|_| non_zero_or_one(1))
        }
        SoftwareDecodeThreadBudget::Fixed(thread_count) => thread_count,
    }
}

#[must_use]
pub fn hover_decoder_config_from_resolved_budget(
    playback_config: VideoDecoderThreadConfig,
    resolved_budget: &HoverResolvedBudget,
) -> Option<VideoDecoderThreadConfig> {
    let software_frame_pool_frames =
        resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareFramePoolFrames)?;
    let thread_count = resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareThreadCount)?;

    Some(
        VideoDecoderThreadConfig {
            software_frame_pool_frames: software_frame_pool_frames.get(),
            software_decode_thread_budget: SoftwareDecodeThreadBudget::fixed(thread_count),
            ..playback_config
        }
        .normalized(),
    )
}

fn default_hover_thread_capability_minimum() -> NonZeroUsize {
    let host_parallelism = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1);

    non_zero_or_one(host_parallelism.min(2))
}

const fn non_zero_or_one(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

#[cfg(test)]
mod tests {
    use frame_server_core::hover_budget::{
        HoverBudgetResolutionSource, HoverResolvedBudgetResource,
    };

    use super::*;

    fn nz(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test value must be positive")
    }

    fn resolved_software_budget(pool_frames: usize, thread_count: usize) -> HoverResolvedBudget {
        HoverResolvedBudget::new(vec![
            HoverResolvedBudgetResource::new(
                HoverBudgetResourceClass::SoftwareFramePoolFrames,
                nz(pool_frames),
                HoverBudgetResolutionSource::BackendMinimumAuto,
            ),
            HoverResolvedBudgetResource::new(
                HoverBudgetResourceClass::SoftwareThreadCount,
                nz(thread_count),
                HoverBudgetResolutionSource::BackendMinimumAuto,
            ),
        ])
    }

    fn reported_minimum(
        report: HoverBudgetCapabilityReport,
        resource_class: HoverBudgetResourceClass,
    ) -> usize {
        match report {
            HoverBudgetCapabilityReport::Supported(capability) => capability
                .minimums()
                .iter()
                .find(|minimum| minimum.resource_class() == resource_class)
                .expect("requested minimum must be reported")
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
    fn capability_reports_current_pool_and_thread_minimums() {
        let first_owner = FfmpegSoftwareHoverOwner::new(FfmpegSoftwareHoverContext::new(
            nz(8),
            nz(6),
            nz(2),
            nz(3),
            6,
            3,
        ));
        let second_owner = FfmpegSoftwareHoverOwner::new(FfmpegSoftwareHoverContext::new(
            nz(8),
            nz(6),
            nz(4),
            nz(2),
            4,
            4,
        ));

        assert_eq!(
            reported_minimum(
                first_owner.hover_capability_report(),
                HoverBudgetResourceClass::SoftwareFramePoolFrames,
            ),
            2
        );
        assert_eq!(
            reported_minimum(
                first_owner.hover_capability_report(),
                HoverBudgetResourceClass::SoftwareThreadCount,
            ),
            3
        );
        assert_eq!(
            reported_minimum(
                second_owner.hover_capability_report(),
                HoverBudgetResourceClass::SoftwareFramePoolFrames,
            ),
            4
        );
    }

    #[test]
    fn admission_rejects_capacity_pressure_without_rewriting_capability() {
        let owner = FfmpegSoftwareHoverOwner::new(FfmpegSoftwareHoverContext::new(
            nz(8),
            nz(6),
            nz(2),
            nz(2),
            1,
            1,
        ));
        let capability = owner.hover_capability_report();

        let admission = owner.admit_hover_reservation(&resolved_software_budget(2, 2));

        assert_eq!(
            reported_minimum(
                capability,
                HoverBudgetResourceClass::SoftwareFramePoolFrames
            ),
            2
        );
        assert!(matches!(
            admission,
            FfmpegSoftwareHoverAdmission::Rejected(HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted
            ))
        ));
    }

    #[test]
    fn admission_allows_only_one_active_hover_reservation_and_drop_releases() {
        let owner = FfmpegSoftwareHoverOwner::new(FfmpegSoftwareHoverContext::new(
            nz(8),
            nz(6),
            nz(2),
            nz(2),
            6,
            4,
        ));
        let reservation = match owner.admit_hover_reservation(&resolved_software_budget(2, 2)) {
            FfmpegSoftwareHoverAdmission::Admitted(reservation) => reservation,
            FfmpegSoftwareHoverAdmission::Rejected(reason) => {
                panic!("first hover reservation must be admitted, got {reason:?}")
            }
        };

        assert!(
            owner
                .snapshot()
                .expect("test owner mutex must not be poisoned")
                .hover_active
        );
        assert!(matches!(
            owner.admit_hover_reservation(&resolved_software_budget(2, 2)),
            FfmpegSoftwareHoverAdmission::Rejected(HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ExistingHoverReservation
            ))
        ));

        drop(reservation);

        assert!(
            !owner
                .snapshot()
                .expect("test owner mutex must not be poisoned")
                .hover_active
        );
    }

    #[test]
    fn resolved_budget_builds_hover_decoder_config_with_fixed_thread_count() {
        let playback_config = VideoDecoderThreadConfig {
            software_frame_pool_frames: 8,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::auto(),
            ..VideoDecoderThreadConfig::default()
        };

        let hover_config = hover_decoder_config_from_resolved_budget(
            playback_config,
            &resolved_software_budget(2, 3),
        )
        .expect("resolved software budget carries both resources");

        assert_eq!(hover_config.software_frame_pool_frames, 2);
        assert_eq!(
            hover_config
                .software_decode_thread_budget
                .fixed_thread_count(),
            Some(nz(3))
        );
    }
}
