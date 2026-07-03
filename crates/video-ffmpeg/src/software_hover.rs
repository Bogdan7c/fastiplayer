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
use video_backend_api::{
    BackendHoverBudgetAdmissionFatalReason, BackendHoverBudgetAdmissionReport,
    BackendHoverBudgetAdmissionUnavailableReason, BackendHoverBudgetCapabilityMinimum,
    BackendHoverBudgetCapabilityReport, BackendHoverBudgetCapabilityUnavailableReason,
    BackendHoverBudgetResourceClass, BackendHoverBudgetResourcePressureReason,
    BackendHoverResolvedBudget, HoverBudgetDiagnosticsProvider,
};
use video_core::{SoftwareDecodeThreadBudget, VideoDecoderThreadConfig};

/// Минимальный software hover pool: 1 prepared entry + 1 approximate слот
/// визуального keyframe + 2 свободных кадра, чтобы decoder мог идти конвейером.
const DEFAULT_HOVER_SOFTWARE_FRAME_POOL_MINIMUM: usize = 4;

/// Hover берет половину host parallelism, чтобы не вытеснять playback/render.
const HOVER_THREAD_CAPABILITY_HOST_DIVISOR: usize = 2;
/// Ниже двух потоков 4K software hover слишком легко становится однобуферным bottleneck-ом.
const HOVER_THREAD_CAPABILITY_MINIMUM: usize = 2;
/// Больше шести hover потоков уже конкурируют с playback SW decode на обычных desktop CPU.
const HOVER_THREAD_CAPABILITY_MAXIMUM: usize = 6;

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

        // Provider capacity — максимальный грант, удовлетворяющий политике
        // «hover строго ниже playback» (pairwise hover < playback). Раньше здесь
        // стояло `playback - hover_minimum`, что учитывало minimum дважды:
        // admission требовал `requested + minimum <= playback`, и после подъёма
        // auto-minimums (pool 4, threads clamp(cores/2, 2, 6)) hover session
        // переставала стартовать даже при minimum < playback. Hover пул и
        // потоки — отдельные ресурсы hover-декодера, а не вычет из playback
        // пула, поэтому единственное честное ограничение — строгая пара.
        Self::new(
            playback_frame_pool_budget,
            playback_thread_budget,
            hover_frame_pool_capability_minimum,
            hover_thread_capability_minimum,
            playback_frame_pool_budget.get().saturating_sub(1),
            playback_thread_budget.get().saturating_sub(1),
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
    pub fn hover_admission_report(
        &self,
        resolved_budget: &HoverResolvedBudget,
    ) -> HoverBudgetAdmissionReport {
        let Some(requested_frame_pool_frames) =
            resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareFramePoolFrames)
        else {
            return HoverBudgetAdmissionReport::Unavailable(
                HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable,
            );
        };
        let Some(requested_thread_count) =
            resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareThreadCount)
        else {
            return HoverBudgetAdmissionReport::Unavailable(
                HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable,
            );
        };

        match self.inner.lock() {
            Ok(inner) => {
                inner.hover_admission_report(requested_frame_pool_frames, requested_thread_count)
            }
            Err(_) => HoverBudgetAdmissionReport::Fatal(
                HoverBudgetAdmissionFatalReason::ProviderInvariantViolated,
            ),
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

impl HoverBudgetDiagnosticsProvider for FfmpegSoftwareHoverOwner {
    fn hover_capability_report(&self) -> BackendHoverBudgetCapabilityReport {
        backend_capability_report_from_core(FfmpegSoftwareHoverOwner::hover_capability_report(self))
    }

    fn hover_admission_report(
        &self,
        resolved_budget: &BackendHoverResolvedBudget,
    ) -> BackendHoverBudgetAdmissionReport {
        let core_budget = core_resolved_budget_from_backend(resolved_budget);
        backend_admission_report_from_core(FfmpegSoftwareHoverOwner::hover_admission_report(
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
                frame_server_core::HoverBudgetCapabilityUnavailableReason::BackendNotReady => {
                    BackendHoverBudgetCapabilityUnavailableReason::BackendNotReady
                }
                frame_server_core::HoverBudgetCapabilityUnavailableReason::MediaContextUnavailable => {
                    BackendHoverBudgetCapabilityUnavailableReason::MediaContextUnavailable
                }
                frame_server_core::HoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable => {
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
        match self.hover_admission_report(requested_frame_pool_frames, requested_thread_count) {
            HoverBudgetAdmissionReport::Admitted => {}
            rejection => return Err(rejection),
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

    fn hover_admission_report(
        &self,
        requested_frame_pool_frames: NonZeroUsize,
        requested_thread_count: NonZeroUsize,
    ) -> HoverBudgetAdmissionReport {
        if self.hover_reservation.is_some() {
            return HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ExistingHoverReservation,
            );
        }

        if requested_frame_pool_frames >= self.context.playback_frame_pool_budget
            || requested_thread_count >= self.context.playback_thread_budget
        {
            return HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
            );
        }

        if requested_frame_pool_frames.get() > self.context.hover_frame_pool_provider_capacity
            || requested_thread_count.get() > self.context.hover_thread_provider_capacity
        {
            return HoverBudgetAdmissionReport::ResourcePressure(
                HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
            );
        }

        HoverBudgetAdmissionReport::Admitted
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
    // Единый резолв с ffmpeg_thread_count_from_budget: hover accounting обязан
    // видеть тот же playback thread budget, который реально получит декодер.
    config.software_decode_thread_budget.resolved_thread_count()
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

    hover_thread_capability_minimum_for_host_parallelism(host_parallelism)
}

fn hover_thread_capability_minimum_for_host_parallelism(host_parallelism: usize) -> NonZeroUsize {
    let half_host_parallelism = host_parallelism / HOVER_THREAD_CAPABILITY_HOST_DIVISOR;
    let clamped_thread_count = half_host_parallelism.clamp(
        HOVER_THREAD_CAPABILITY_MINIMUM,
        HOVER_THREAD_CAPABILITY_MAXIMUM,
    );

    non_zero_or_one(clamped_thread_count)
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
    fn default_context_reports_new_auto_minimums() {
        let playback_config = VideoDecoderThreadConfig {
            software_frame_pool_frames: 8,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::fixed(nz(8)),
            ..VideoDecoderThreadConfig::default()
        };
        let owner = FfmpegSoftwareHoverOwner::new(
            FfmpegSoftwareHoverContext::from_playback_decoder_config(playback_config),
        );
        let host_parallelism = std::thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(1);

        assert_eq!(
            reported_minimum(
                owner.hover_capability_report(),
                HoverBudgetResourceClass::SoftwareFramePoolFrames,
            ),
            DEFAULT_HOVER_SOFTWARE_FRAME_POOL_MINIMUM
        );
        assert_eq!(
            reported_minimum(
                owner.hover_capability_report(),
                HoverBudgetResourceClass::SoftwareThreadCount,
            ),
            hover_thread_capability_minimum_for_host_parallelism(host_parallelism).get()
        );
    }

    #[test]
    fn playback_config_capacity_allows_any_budget_strictly_below_playback() {
        // Регрессия: capacity считалась как playback - minimum, из-за чего auto
        // budget (requested == minimum) отклонялся уже при 2*minimum > playback,
        // хотя политика требует только strict pairwise hover < playback.
        let playback_config = VideoDecoderThreadConfig {
            software_frame_pool_frames: 8,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::fixed(nz(5)),
            ..VideoDecoderThreadConfig::default()
        };
        let owner = FfmpegSoftwareHoverOwner::new(
            FfmpegSoftwareHoverContext::from_playback_decoder_config(playback_config),
        );

        let snapshot = owner.snapshot().expect("hover owner snapshot must exist");
        assert_eq!(snapshot.hover_frame_pool_provider_capacity, 7);
        assert_eq!(snapshot.hover_thread_provider_capacity, 4);

        // Сценарий пользователя: pool 4 < 8, threads 4 < 5 — must admit.
        let admission = owner.admit_hover_reservation(&resolved_software_budget(4, 4));
        assert!(matches!(
            admission,
            FfmpegSoftwareHoverAdmission::Admitted(_)
        ));
    }

    #[test]
    fn hover_thread_auto_minimum_uses_half_host_parallelism_clamped_to_two_six() {
        assert_eq!(
            hover_thread_capability_minimum_for_host_parallelism(1),
            nz(2)
        );
        assert_eq!(
            hover_thread_capability_minimum_for_host_parallelism(4),
            nz(2)
        );
        assert_eq!(
            hover_thread_capability_minimum_for_host_parallelism(8),
            nz(4)
        );
        assert_eq!(
            hover_thread_capability_minimum_for_host_parallelism(16),
            nz(6)
        );
    }

    #[test]
    fn capability_reports_current_pool_and_thread_minimums() {
        let first_owner = FfmpegSoftwareHoverOwner::new(FfmpegSoftwareHoverContext::new(
            nz(8),
            nz(6),
            nz(4),
            nz(3),
            4,
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
            4
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
            nz(4),
            nz(2),
            1,
            1,
        ));
        let capability = owner.hover_capability_report();

        let admission = owner.admit_hover_reservation(&resolved_software_budget(4, 2));

        assert_eq!(
            reported_minimum(
                capability,
                HoverBudgetResourceClass::SoftwareFramePoolFrames
            ),
            4
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
            nz(4),
            nz(2),
            4,
            4,
        ));
        let reservation = match owner.admit_hover_reservation(&resolved_software_budget(4, 2)) {
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
            owner.admit_hover_reservation(&resolved_software_budget(4, 2)),
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
            &resolved_software_budget(4, 3),
        )
        .expect("resolved software budget carries both resources");

        assert_eq!(hover_config.software_frame_pool_frames, 4);
        assert_eq!(
            hover_config
                .software_decode_thread_budget
                .fixed_thread_count(),
            Some(nz(3))
        );
    }
}
