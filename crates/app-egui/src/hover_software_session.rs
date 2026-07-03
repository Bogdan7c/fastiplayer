//! App-owned startup boundary for the minimal FFmpeg software hover session.
//!
//! Здесь нет timeline UI и нет demux/source ownership: S23 только проверяет, что
//! app может безопасно зарезервировать reduced software budget и поднять
//! отдельный FFmpeg decoder backend через существующую factory.

#![cfg_attr(not(test), allow(dead_code))]

use frame_server_core::{
    HoverBudgetAdmissionOutcome, HoverBudgetAdmissionRejection, HoverBudgetAdmissionReport,
    HoverBudgetRequest, HoverBudgetRequirement, HoverBudgetResolutionOutcome,
    HoverBudgetResolutionUnavailableReason, HoverBudgetResolutionUnsupportedReason,
    HoverBudgetResourceClass, HoverBudgetSetting, HoverPlaybackResourceBudget,
    HoverPositiveBudgetError, HoverResolvedBudget, admit_hover_budget, resolve_hover_budget,
};
use player_core::{PlayerVideoDecoderThreadConfig, StartedVideoBackend};
use rustiplayer_config::FrameServerBudgetConfig;
use video_ffmpeg::software_hover::hover_decoder_config_from_resolved_budget;
use video_ffmpeg::{
    FfmpegSoftwareHoverAdmission, FfmpegSoftwareHoverOwner, FfmpegSoftwareHoverReservation,
    FfmpegSoftwareVideoBackendFactory,
};

pub(crate) struct SoftwareHoverSessionStartupRequest {
    playback_decoder_config: PlayerVideoDecoderThreadConfig,
    hover_pool_frames: FrameServerBudgetConfig,
    hover_thread_count: FrameServerBudgetConfig,
    hover_owner: FfmpegSoftwareHoverOwner,
}

impl SoftwareHoverSessionStartupRequest {
    #[must_use]
    pub(crate) fn new(
        playback_decoder_config: PlayerVideoDecoderThreadConfig,
        hover_pool_frames: FrameServerBudgetConfig,
        hover_thread_count: FrameServerBudgetConfig,
        hover_owner: FfmpegSoftwareHoverOwner,
    ) -> Self {
        Self {
            playback_decoder_config,
            hover_pool_frames,
            hover_thread_count,
            hover_owner,
        }
    }
}

pub(crate) struct SoftwareHoverSession {
    started_backend: StartedVideoBackend,
    reservation: FfmpegSoftwareHoverReservation,
    resolved_budget: HoverResolvedBudget,
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
}

impl SoftwareHoverSession {
    #[must_use]
    pub(crate) fn backend_id(&self) -> &str {
        self.started_backend.backend_id()
    }

    #[must_use]
    pub(crate) fn resolved_budget(&self) -> &HoverResolvedBudget {
        &self.resolved_budget
    }

    #[must_use]
    pub(crate) fn decoder_thread_config(&self) -> PlayerVideoDecoderThreadConfig {
        self.decoder_thread_config
    }

    #[must_use]
    pub(crate) fn reservation(&self) -> &FfmpegSoftwareHoverReservation {
        &self.reservation
    }

    /// Разбирает session на started backend и владение reservation/budget.
    ///
    /// Используется decode wiring-ом: backend оборачивается в WGPU release
    /// boundary, а reservation должна жить, пока живёт hover decoder thread.
    #[must_use]
    pub(crate) fn into_parts(
        self,
    ) -> (
        StartedVideoBackend,
        FfmpegSoftwareHoverReservation,
        HoverResolvedBudget,
    ) {
        (self.started_backend, self.reservation, self.resolved_budget)
    }
}

impl std::fmt::Debug for SoftwareHoverSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SoftwareHoverSession")
            .field("backend_id", &self.backend_id())
            .field("resolved_budget", &self.resolved_budget)
            .field("decoder_thread_config", &self.decoder_thread_config)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum SoftwareHoverSessionStartupOutcome {
    Started(SoftwareHoverSession),
    InvalidFixedBudget {
        resource_class: HoverBudgetResourceClass,
        error: HoverPositiveBudgetError,
    },
    Unsupported {
        reason: HoverBudgetResolutionUnsupportedReason,
    },
    Unavailable {
        reason: HoverBudgetResolutionUnavailableReason,
    },
    AdmissionRejected {
        resolved_budget: HoverResolvedBudget,
        reason: HoverBudgetAdmissionRejection,
    },
    StartupFailed {
        resolved_budget: HoverResolvedBudget,
        error: String,
    },
}

#[must_use]
pub(crate) fn start_software_hover_session(
    request: SoftwareHoverSessionStartupRequest,
) -> SoftwareHoverSessionStartupOutcome {
    let Some(context) = request.hover_owner.context() else {
        return SoftwareHoverSessionStartupOutcome::Unavailable {
            reason: HoverBudgetResolutionUnavailableReason::Capability(
                frame_server_core::HoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable,
            ),
        };
    };

    let hover_request = match hover_budget_request_from_startup_request(&request, context) {
        Ok(hover_request) => hover_request,
        Err(outcome) => return outcome,
    };
    let capability_report = request.hover_owner.hover_capability_report();
    let resolved_budget = match resolve_hover_budget(&hover_request, &capability_report) {
        HoverBudgetResolutionOutcome::Resolved(resolved_budget) => resolved_budget,
        HoverBudgetResolutionOutcome::Unsupported(reason) => {
            return SoftwareHoverSessionStartupOutcome::Unsupported { reason };
        }
        HoverBudgetResolutionOutcome::Unavailable(reason) => {
            return SoftwareHoverSessionStartupOutcome::Unavailable { reason };
        }
    };

    let reservation = match request
        .hover_owner
        .admit_hover_reservation(&resolved_budget)
    {
        FfmpegSoftwareHoverAdmission::Admitted(reservation) => reservation,
        FfmpegSoftwareHoverAdmission::Rejected(report) => {
            return admission_rejection_outcome(resolved_budget, report);
        }
    };

    let Some(hover_decoder_config) = hover_decoder_config_from_resolved_budget(
        request.playback_decoder_config,
        &resolved_budget,
    ) else {
        return SoftwareHoverSessionStartupOutcome::AdmissionRejected {
            resolved_budget,
            reason: HoverBudgetAdmissionRejection::Unavailable(
                frame_server_core::HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable,
            ),
        };
    };

    let backend_factory =
        FfmpegSoftwareVideoBackendFactory::new_with_decoder_config(hover_decoder_config);
    match backend_factory.start_for_composition() {
        Ok(started_backend) => SoftwareHoverSessionStartupOutcome::Started(SoftwareHoverSession {
            started_backend,
            reservation,
            resolved_budget,
            decoder_thread_config: hover_decoder_config,
        }),
        Err(error) => SoftwareHoverSessionStartupOutcome::StartupFailed {
            resolved_budget,
            error: error.to_string(),
        },
    }
}

fn hover_budget_request_from_startup_request(
    request: &SoftwareHoverSessionStartupRequest,
    context: video_ffmpeg::FfmpegSoftwareHoverContext,
) -> Result<HoverBudgetRequest, SoftwareHoverSessionStartupOutcome> {
    Ok(HoverBudgetRequest::new(vec![
        HoverBudgetRequirement::new(
            HoverBudgetResourceClass::SoftwareFramePoolFrames,
            budget_setting_from_config(
                HoverBudgetResourceClass::SoftwareFramePoolFrames,
                request.hover_pool_frames,
            )?,
            HoverPlaybackResourceBudget::available(context.playback_frame_pool_budget()),
        ),
        HoverBudgetRequirement::new(
            HoverBudgetResourceClass::SoftwareThreadCount,
            budget_setting_from_config(
                HoverBudgetResourceClass::SoftwareThreadCount,
                request.hover_thread_count,
            )?,
            HoverPlaybackResourceBudget::available(context.playback_thread_budget()),
        ),
    ]))
}

fn budget_setting_from_config(
    resource_class: HoverBudgetResourceClass,
    config: FrameServerBudgetConfig,
) -> Result<HoverBudgetSetting, SoftwareHoverSessionStartupOutcome> {
    match config {
        FrameServerBudgetConfig::Auto => Ok(HoverBudgetSetting::auto()),
        FrameServerBudgetConfig::Fixed(value) => {
            HoverBudgetSetting::fixed(value).map_err(|error| {
                SoftwareHoverSessionStartupOutcome::InvalidFixedBudget {
                    resource_class,
                    error,
                }
            })
        }
    }
}

fn admission_rejection_outcome(
    resolved_budget: HoverResolvedBudget,
    report: HoverBudgetAdmissionReport,
) -> SoftwareHoverSessionStartupOutcome {
    match admit_hover_budget(resolved_budget, report) {
        HoverBudgetAdmissionOutcome::Rejected {
            resolved_budget,
            reason,
        } => SoftwareHoverSessionStartupOutcome::AdmissionRejected {
            resolved_budget,
            reason,
        },
        HoverBudgetAdmissionOutcome::Admitted(_resolved_budget) => unreachable!(
            "admit_hover_budget cannot admit after provider returned a rejected report"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use frame_server_core::{
        HoverBudgetAdmissionRejection, HoverBudgetResolutionUnavailableReason,
        HoverBudgetResourcePressureReason,
    };
    use video_ffmpeg::{
        FFMPEG_SOFTWARE_BACKEND_ID, FfmpegSoftwareHoverContext, FfmpegSoftwareHoverOwner,
    };

    use super::*;

    fn nz(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test value must be positive")
    }

    fn software_owner(
        playback_pool_frames: usize,
        playback_threads: usize,
        hover_pool_minimum: usize,
        hover_thread_minimum: usize,
        pool_capacity: usize,
        thread_capacity: usize,
    ) -> FfmpegSoftwareHoverOwner {
        FfmpegSoftwareHoverOwner::new(FfmpegSoftwareHoverContext::new(
            nz(playback_pool_frames),
            nz(playback_threads),
            nz(hover_pool_minimum),
            nz(hover_thread_minimum),
            pool_capacity,
            thread_capacity,
        ))
    }

    #[test]
    fn software_hover_startup_uses_backend_reported_minimum_budget() {
        let owner = software_owner(8, 6, 4, 3, 4, 3);
        let request = SoftwareHoverSessionStartupRequest::new(
            PlayerVideoDecoderThreadConfig::default(),
            FrameServerBudgetConfig::Auto,
            FrameServerBudgetConfig::Auto,
            owner.clone(),
        );

        let outcome = start_software_hover_session(request);
        let session = match outcome {
            SoftwareHoverSessionStartupOutcome::Started(session) => session,
            other => panic!("software hover session must start, got {other:?}"),
        };

        assert_eq!(session.backend_id(), FFMPEG_SOFTWARE_BACKEND_ID);
        assert_eq!(
            session
                .resolved_budget()
                .budget_for(HoverBudgetResourceClass::SoftwareFramePoolFrames),
            Some(nz(4))
        );
        assert_eq!(
            session
                .resolved_budget()
                .budget_for(HoverBudgetResourceClass::SoftwareThreadCount),
            Some(nz(3))
        );
        assert_eq!(
            session.decoder_thread_config().software_frame_pool_frames,
            4
        );
        assert_eq!(
            session
                .decoder_thread_config()
                .software_decode_thread_budget
                .fixed_thread_count(),
            Some(nz(3))
        );
        assert!(session.reservation().frame_pool_frames() < nz(8));
        assert!(session.reservation().thread_count() < nz(6));
        assert!(
            owner
                .snapshot()
                .expect("test owner mutex must not be poisoned")
                .hover_active
        );

        let SoftwareHoverSession {
            started_backend,
            reservation,
            ..
        } = session;
        let decoder_thread = started_backend.into_decoder_thread();
        let snapshot = decoder_thread.host_upload_resource_snapshot();
        match snapshot {
            video_core::HostUploadResourceSnapshotStatus::Available(snapshot) => {
                assert_eq!(snapshot.upload_slots_capacity, 4);
                assert_eq!(snapshot.upload_slots_free, 4);
            }
            other => {
                panic!("software hover backend must expose HostUpload snapshot, got {other:?}")
            }
        }

        drop(decoder_thread);
        drop(reservation);
        assert!(
            !owner
                .snapshot()
                .expect("test owner mutex must not be poisoned")
                .hover_active
        );
    }

    #[test]
    fn admission_pressure_is_typed_separately_from_capability_minimum() {
        let owner = software_owner(8, 6, 4, 2, 1, 1);
        let request = SoftwareHoverSessionStartupRequest::new(
            PlayerVideoDecoderThreadConfig::default(),
            FrameServerBudgetConfig::Auto,
            FrameServerBudgetConfig::Auto,
            owner,
        );

        let outcome = start_software_hover_session(request);

        match outcome {
            SoftwareHoverSessionStartupOutcome::AdmissionRejected {
                resolved_budget,
                reason:
                    HoverBudgetAdmissionRejection::ResourcePressure(
                        HoverBudgetResourcePressureReason::ProviderCapacityExhausted,
                    ),
            } => {
                assert_eq!(
                    resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareFramePoolFrames),
                    Some(nz(4))
                );
                assert_eq!(
                    resolved_budget.budget_for(HoverBudgetResourceClass::SoftwareThreadCount),
                    Some(nz(2))
                );
            }
            other => panic!("capacity pressure must be typed admission rejection, got {other:?}"),
        }
    }

    #[test]
    fn context_change_recomputes_backend_minimums_and_surfaces_no_fit() {
        let first_owner = software_owner(8, 6, 4, 3, 4, 3);
        let second_owner = software_owner(8, 3, 4, 3, 4, 0);

        let first_outcome = start_software_hover_session(SoftwareHoverSessionStartupRequest::new(
            PlayerVideoDecoderThreadConfig::default(),
            FrameServerBudgetConfig::Auto,
            FrameServerBudgetConfig::Auto,
            first_owner,
        ));
        let second_outcome = start_software_hover_session(SoftwareHoverSessionStartupRequest::new(
            PlayerVideoDecoderThreadConfig::default(),
            FrameServerBudgetConfig::Auto,
            FrameServerBudgetConfig::Auto,
            second_owner,
        ));

        assert!(matches!(
            first_outcome,
            SoftwareHoverSessionStartupOutcome::Started(_)
        ));
        assert!(matches!(
            second_outcome,
            SoftwareHoverSessionStartupOutcome::Unavailable {
                reason: HoverBudgetResolutionUnavailableReason::NoFittingBackendMinimum {
                    resource_class: HoverBudgetResourceClass::SoftwareThreadCount,
                    ..
                }
            }
        ));
    }

    #[test]
    fn small_playback_pool_degrades_hover_without_reserving_or_rewriting_playback_budget() {
        let owner = software_owner(4, 6, 4, 3, 0, 3);
        let playback_decoder_config = PlayerVideoDecoderThreadConfig {
            software_frame_pool_frames: 4,
            software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::fixed(nz(6)),
            ..PlayerVideoDecoderThreadConfig::default()
        };
        let request = SoftwareHoverSessionStartupRequest::new(
            playback_decoder_config,
            FrameServerBudgetConfig::Auto,
            FrameServerBudgetConfig::Auto,
            owner.clone(),
        );

        let outcome = start_software_hover_session(request);

        assert!(matches!(
            outcome,
            SoftwareHoverSessionStartupOutcome::Unavailable {
                reason: HoverBudgetResolutionUnavailableReason::NoFittingBackendMinimum {
                    resource_class: HoverBudgetResourceClass::SoftwareFramePoolFrames,
                    playback_budget,
                    smallest_positive_minimum,
                }
            } if playback_budget == nz(4) && smallest_positive_minimum == nz(4)
        ));

        let snapshot = owner
            .snapshot()
            .expect("test owner mutex must not be poisoned");
        assert_eq!(snapshot.playback_frame_pool_budget, nz(4));
        assert_eq!(snapshot.playback_thread_budget, nz(6));
        assert!(!snapshot.hover_active);
    }
}
