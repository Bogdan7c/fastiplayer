//! App-owned diagnostics/preflight для Frame Server hover budget settings.
//!
//! Модуль не владеет backend resources: он строит нейтральный
//! `frame-server-core` request из committed/draft config-а и спрашивает
//! backend-owned read-only provider, который пришёл из active video backend-а.

use std::num::NonZeroUsize;

use frame_server_core::{
    HoverBudgetAdmissionOutcome, HoverBudgetAdmissionRejection, HoverBudgetAdmissionReport,
    HoverBudgetCapabilityReport, HoverBudgetRequest, HoverBudgetRequirement,
    HoverBudgetResolutionOutcome, HoverBudgetResolutionUnavailableReason, HoverBudgetResourceClass,
    HoverBudgetSetting, HoverPlaybackResourceBudget, HoverPositiveBudgetError, HoverResolvedBudget,
    admit_hover_budget, resolve_hover_budget,
};
use player_core::PlayerVideoDecoderThreadConfig;
use rustiplayer_config::{FrameServerBudgetConfig, FrameServerConfig};
use video_backend_api::{
    BackendHoverBudgetAdmissionFatalReason, BackendHoverBudgetAdmissionReport,
    BackendHoverBudgetAdmissionUnavailableReason, BackendHoverBudgetCapabilityReport,
    BackendHoverBudgetCapabilityUnavailableReason, BackendHoverBudgetResourceClass,
    BackendHoverBudgetResourcePressureReason, BackendHoverBudgetUnsupportedReason,
    BackendHoverResolvedBudget, BackendHoverResolvedBudgetResource,
    HoverBudgetDiagnosticsProviderHandle,
};
use video_ffmpeg::FfmpegSoftwareHoverContext;

use crate::video_pipeline_selector::VideoBackendKind;

/// Backend class, для которого построена budget diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameServerHoverBudgetBackendKind {
    /// Hardware zero-copy backend сейчас использует VAAPI surfaces.
    HardwareZeroCopy,

    /// FFmpeg software backend использует host-frame pool и decoder threads.
    FfmpegSoftware,
}

impl FrameServerHoverBudgetBackendKind {
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::HardwareZeroCopy => "hardware",
            Self::FfmpegSoftware => "ffmpeg_software",
        }
    }
}

/// Per-resource строка diagnostics без доступа к backend internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameServerHoverBudgetResourceDiagnostics {
    /// Resource class из neutral resolver-а.
    pub(crate) resource_class: HoverBudgetResourceClass,

    /// User setting после разбора `auto | fixed-positive`.
    pub(crate) setting: HoverBudgetSetting,

    /// Playback budget для matching resource, если он есть у текущего backend-а.
    pub(crate) playback_budget: Option<NonZeroUsize>,

    /// Минимум, который сообщил backend provider для этого resource class-а.
    pub(crate) backend_reported_minimum: Option<NonZeroUsize>,

    /// Hover budget после `auto`/fixed resolution, если resolution прошёл.
    pub(crate) resolved_hover_budget: Option<NonZeroUsize>,
}

/// Snapshot для Settings preflight и telemetry diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameServerHoverBudgetDiagnosticsSnapshot {
    /// Текущий backend class, из которого пришли реальные provider facts.
    pub(crate) backend_kind: FrameServerHoverBudgetBackendKind,

    /// Capability report до resolution.
    pub(crate) capability_report: HoverBudgetCapabilityReport,

    /// Итог `auto`/fixed resolution.
    pub(crate) resolution_outcome: HoverBudgetResolutionOutcome,

    /// Текущий admission/pressure report, если resolution дал concrete budget.
    pub(crate) admission_outcome: Option<HoverBudgetAdmissionOutcome>,

    /// Раскрытые numbers для help/diagnostics UI без fake values.
    pub(crate) resources: Vec<FrameServerHoverBudgetResourceDiagnostics>,
}

impl FrameServerHoverBudgetDiagnosticsSnapshot {
    #[must_use]
    pub(crate) fn fixed_too_large_rejection_for_changed_fields(
        &self,
        current: &FrameServerConfig,
        draft: &FrameServerConfig,
    ) -> Option<FrameServerHoverBudgetPreflightRejectionKind> {
        let unavailable_reason = match self.resolution_outcome {
            HoverBudgetResolutionOutcome::Unavailable(reason) => reason,
            HoverBudgetResolutionOutcome::Resolved(_)
            | HoverBudgetResolutionOutcome::Unsupported(_) => return None,
        };

        let rejected_class = fixed_too_large_resource_class(unavailable_reason)?;
        if !fixed_budget_changed_for_resource(rejected_class, current, draft) {
            return None;
        }

        Some(
            FrameServerHoverBudgetPreflightRejectionKind::FixedTooLarge {
                reason: unavailable_reason,
            },
        )
    }
}

/// Ошибка построения request-а до neutral resolver-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameServerHoverBudgetRequestError {
    /// Persisted/draft fixed value должен быть positive; `0` не off-switch.
    InvalidFixedBudget {
        resource_class: HoverBudgetResourceClass,
        error: HoverPositiveBudgetError,
    },
}

/// Почему live settings preflight отклонил изменение до persist/runtime mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrameServerHoverBudgetPreflightRejectionKind {
    /// Fixed budget не positive; это не hidden hover-off switch.
    InvalidFixedBudget {
        resource_class: HoverBudgetResourceClass,
        error: HoverPositiveBudgetError,
    },

    /// Fixed budget для изменённого поля не проходит current backend resolution.
    FixedTooLarge {
        reason: HoverBudgetResolutionUnavailableReason,
    },
}

/// Полный rejection с typed diagnostics snapshot-ом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameServerHoverBudgetPreflightRejection {
    /// Machine-readable reason для tests/diagnostics.
    pub(crate) kind: FrameServerHoverBudgetPreflightRejectionKind,

    /// Snapshot, построенный до отказа; config/runtime ещё не мутировали.
    pub(crate) diagnostics: FrameServerHoverBudgetDiagnosticsSnapshot,
}

impl FrameServerHoverBudgetPreflightRejection {
    #[must_use]
    pub(crate) fn message(&self) -> String {
        match self.kind {
            FrameServerHoverBudgetPreflightRejectionKind::InvalidFixedBudget {
                resource_class,
                error,
            } => format!(
                "Frame Server hover budget отклонён до сохранения: {} {:?}",
                resource_class_label(resource_class),
                error
            ),
            FrameServerHoverBudgetPreflightRejectionKind::FixedTooLarge { reason } => {
                format!(
                    "Frame Server hover budget отклонён до сохранения: {}",
                    resolution_unavailable_label(reason)
                )
            }
        }
    }
}

/// Строит diagnostics для активного backend-а, если provider доступен.
pub(crate) fn frame_server_hover_budget_diagnostics(
    backend_kind: VideoBackendKind,
    frame_server_config: &FrameServerConfig,
    decoder_config: PlayerVideoDecoderThreadConfig,
    provider: &HoverBudgetDiagnosticsProviderHandle,
) -> Result<FrameServerHoverBudgetDiagnosticsSnapshot, FrameServerHoverBudgetRequestError> {
    let backend_kind = match backend_kind {
        VideoBackendKind::HardwareZeroCopy => FrameServerHoverBudgetBackendKind::HardwareZeroCopy,
        VideoBackendKind::FfmpegSoftware => FrameServerHoverBudgetBackendKind::FfmpegSoftware,
    };
    let request = hover_budget_request_for_backend(
        backend_kind,
        frame_server_config,
        decoder_config.normalized(),
    )?;
    let capability_report =
        hover_capability_report_from_backend(provider.hover_capability_report());
    let resolution_outcome = resolve_hover_budget(&request, &capability_report);
    let admission_outcome =
        resolved_budget_from_outcome(&resolution_outcome).map(|resolved_budget| {
            let admission_report = hover_admission_report_from_backend(
                provider
                    .hover_admission_report(&backend_resolved_budget_from_core(resolved_budget)),
            );
            admit_hover_budget(resolved_budget.clone(), admission_report)
        });
    let resources = resource_diagnostics(
        request.requirements(),
        &capability_report,
        &resolution_outcome,
    );

    Ok(FrameServerHoverBudgetDiagnosticsSnapshot {
        backend_kind,
        capability_report,
        resolution_outcome,
        admission_outcome,
        resources,
    })
}

/// Выполняет live preflight только для budget fields, не трогая controller/store.
pub(crate) fn preflight_frame_server_hover_budget_change(
    current: &FrameServerConfig,
    draft: &FrameServerConfig,
    backend_kind: Option<VideoBackendKind>,
    decoder_config: PlayerVideoDecoderThreadConfig,
    provider: Option<&HoverBudgetDiagnosticsProviderHandle>,
) -> Result<
    Option<FrameServerHoverBudgetDiagnosticsSnapshot>,
    Box<FrameServerHoverBudgetPreflightRejection>,
> {
    let (Some(backend_kind), Some(provider)) = (backend_kind, provider) else {
        return Ok(None);
    };

    let diagnostics =
        frame_server_hover_budget_diagnostics(backend_kind, draft, decoder_config, provider)
            .map_err(|error| Box::new(request_error_rejection(error, backend_kind, provider)))?;

    if let Some(kind) = diagnostics.fixed_too_large_rejection_for_changed_fields(current, draft) {
        return Err(Box::new(FrameServerHoverBudgetPreflightRejection {
            kind,
            diagnostics,
        }));
    }

    Ok(Some(diagnostics))
}

fn hover_budget_request_for_backend(
    backend_kind: FrameServerHoverBudgetBackendKind,
    frame_server_config: &FrameServerConfig,
    decoder_config: PlayerVideoDecoderThreadConfig,
) -> Result<HoverBudgetRequest, FrameServerHoverBudgetRequestError> {
    match backend_kind {
        FrameServerHoverBudgetBackendKind::HardwareZeroCopy => {
            Ok(HoverBudgetRequest::new(vec![HoverBudgetRequirement::new(
                HoverBudgetResourceClass::HardwareSurfaceFrames,
                budget_setting_from_config(
                    HoverBudgetResourceClass::HardwareSurfaceFrames,
                    frame_server_config.hover_pool_frames,
                )?,
                HoverPlaybackResourceBudget::available(non_zero_or_one(
                    decoder_config.decoder_surface_pool_frames,
                )),
            )]))
        }
        FrameServerHoverBudgetBackendKind::FfmpegSoftware => {
            let software_context =
                FfmpegSoftwareHoverContext::from_playback_decoder_config(decoder_config);
            Ok(HoverBudgetRequest::new(vec![
                HoverBudgetRequirement::new(
                    HoverBudgetResourceClass::SoftwareFramePoolFrames,
                    budget_setting_from_config(
                        HoverBudgetResourceClass::SoftwareFramePoolFrames,
                        frame_server_config.hover_pool_frames,
                    )?,
                    HoverPlaybackResourceBudget::available(
                        software_context.playback_frame_pool_budget(),
                    ),
                ),
                HoverBudgetRequirement::new(
                    HoverBudgetResourceClass::SoftwareThreadCount,
                    budget_setting_from_config(
                        HoverBudgetResourceClass::SoftwareThreadCount,
                        frame_server_config.hover_thread_count,
                    )?,
                    HoverPlaybackResourceBudget::available(
                        software_context.playback_thread_budget(),
                    ),
                ),
            ]))
        }
    }
}

fn budget_setting_from_config(
    resource_class: HoverBudgetResourceClass,
    config: FrameServerBudgetConfig,
) -> Result<HoverBudgetSetting, FrameServerHoverBudgetRequestError> {
    match config {
        FrameServerBudgetConfig::Auto => Ok(HoverBudgetSetting::auto()),
        FrameServerBudgetConfig::Fixed(value) => {
            HoverBudgetSetting::fixed(value).map_err(|error| {
                FrameServerHoverBudgetRequestError::InvalidFixedBudget {
                    resource_class,
                    error,
                }
            })
        }
    }
}

fn resource_diagnostics(
    requirements: &[HoverBudgetRequirement],
    capability_report: &HoverBudgetCapabilityReport,
    resolution_outcome: &HoverBudgetResolutionOutcome,
) -> Vec<FrameServerHoverBudgetResourceDiagnostics> {
    requirements
        .iter()
        .copied()
        .map(|requirement| {
            let resource_class = requirement.resource_class();
            FrameServerHoverBudgetResourceDiagnostics {
                resource_class,
                setting: requirement.setting(),
                playback_budget: playback_budget_value(requirement.playback_budget()),
                backend_reported_minimum: backend_reported_minimum(
                    capability_report,
                    resource_class,
                ),
                resolved_hover_budget: resolved_hover_budget(resolution_outcome, resource_class),
            }
        })
        .collect()
}

fn playback_budget_value(playback_budget: HoverPlaybackResourceBudget) -> Option<NonZeroUsize> {
    match playback_budget {
        HoverPlaybackResourceBudget::Available(value) => Some(value),
        HoverPlaybackResourceBudget::MissingPlayableOutput => None,
    }
}

fn backend_reported_minimum(
    capability_report: &HoverBudgetCapabilityReport,
    resource_class: HoverBudgetResourceClass,
) -> Option<NonZeroUsize> {
    match capability_report {
        HoverBudgetCapabilityReport::Supported(capability) => capability
            .minimums()
            .iter()
            .copied()
            .filter(|minimum| minimum.resource_class() == resource_class)
            .filter_map(|minimum| NonZeroUsize::new(minimum.reported_minimum()))
            .min(),
        HoverBudgetCapabilityReport::Unsupported(_)
        | HoverBudgetCapabilityReport::Unavailable(_) => None,
    }
}

fn resolved_hover_budget(
    resolution_outcome: &HoverBudgetResolutionOutcome,
    resource_class: HoverBudgetResourceClass,
) -> Option<NonZeroUsize> {
    resolved_budget_from_outcome(resolution_outcome)?.budget_for(resource_class)
}

fn resolved_budget_from_outcome(
    resolution_outcome: &HoverBudgetResolutionOutcome,
) -> Option<&HoverResolvedBudget> {
    match resolution_outcome {
        HoverBudgetResolutionOutcome::Resolved(resolved_budget) => Some(resolved_budget),
        HoverBudgetResolutionOutcome::Unsupported(_)
        | HoverBudgetResolutionOutcome::Unavailable(_) => None,
    }
}

fn fixed_too_large_resource_class(
    reason: HoverBudgetResolutionUnavailableReason,
) -> Option<HoverBudgetResourceClass> {
    match reason {
        HoverBudgetResolutionUnavailableReason::FixedBudgetBelowBackendMinimum {
            resource_class,
            ..
        }
        | HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
            resource_class,
            ..
        } => Some(resource_class),
        HoverBudgetResolutionUnavailableReason::Capability(_)
        | HoverBudgetResolutionUnavailableReason::NoPositiveBackendMinimum { .. }
        | HoverBudgetResolutionUnavailableReason::NoFittingBackendMinimum { .. } => None,
    }
}

fn fixed_budget_changed_for_resource(
    resource_class: HoverBudgetResourceClass,
    current: &FrameServerConfig,
    draft: &FrameServerConfig,
) -> bool {
    match resource_class {
        HoverBudgetResourceClass::HardwareSurfaceFrames
        | HoverBudgetResourceClass::SoftwareFramePoolFrames => {
            current.hover_pool_frames != draft.hover_pool_frames
                && matches!(draft.hover_pool_frames, FrameServerBudgetConfig::Fixed(_))
        }
        HoverBudgetResourceClass::SoftwareThreadCount => {
            current.hover_thread_count != draft.hover_thread_count
                && matches!(draft.hover_thread_count, FrameServerBudgetConfig::Fixed(_))
        }
    }
}

fn request_error_rejection(
    error: FrameServerHoverBudgetRequestError,
    backend_kind: VideoBackendKind,
    provider: &HoverBudgetDiagnosticsProviderHandle,
) -> FrameServerHoverBudgetPreflightRejection {
    let diagnostics = FrameServerHoverBudgetDiagnosticsSnapshot {
        backend_kind: match backend_kind {
            VideoBackendKind::HardwareZeroCopy => {
                FrameServerHoverBudgetBackendKind::HardwareZeroCopy
            }
            VideoBackendKind::FfmpegSoftware => FrameServerHoverBudgetBackendKind::FfmpegSoftware,
        },
        capability_report: hover_capability_report_from_backend(provider.hover_capability_report()),
        resolution_outcome: HoverBudgetResolutionOutcome::Unsupported(
            frame_server_core::HoverBudgetResolutionUnsupportedReason::NoResourceRequirements,
        ),
        admission_outcome: None,
        resources: Vec::new(),
    };
    let kind = match error {
        FrameServerHoverBudgetRequestError::InvalidFixedBudget {
            resource_class,
            error,
        } => FrameServerHoverBudgetPreflightRejectionKind::InvalidFixedBudget {
            resource_class,
            error,
        },
    };

    FrameServerHoverBudgetPreflightRejection { kind, diagnostics }
}

fn hover_capability_report_from_backend(
    report: BackendHoverBudgetCapabilityReport,
) -> HoverBudgetCapabilityReport {
    match report {
        BackendHoverBudgetCapabilityReport::Supported(minimums) => {
            HoverBudgetCapabilityReport::supported(
                minimums
                    .into_iter()
                    .map(|minimum| {
                        frame_server_core::HoverBudgetCapabilityMinimum::reported(
                            resource_class_from_backend(minimum.resource_class()),
                            minimum.reported_minimum(),
                        )
                    })
                    .collect(),
            )
        }
        BackendHoverBudgetCapabilityReport::Unsupported(reason) => {
            HoverBudgetCapabilityReport::Unsupported(unsupported_reason_from_backend(reason))
        }
        BackendHoverBudgetCapabilityReport::Unavailable(reason) => {
            HoverBudgetCapabilityReport::Unavailable(capability_unavailable_from_backend(reason))
        }
    }
}

fn hover_admission_report_from_backend(
    report: BackendHoverBudgetAdmissionReport,
) -> HoverBudgetAdmissionReport {
    match report {
        BackendHoverBudgetAdmissionReport::Admitted => HoverBudgetAdmissionReport::Admitted,
        BackendHoverBudgetAdmissionReport::ResourcePressure(reason) => {
            HoverBudgetAdmissionReport::ResourcePressure(resource_pressure_from_backend(reason))
        }
        BackendHoverBudgetAdmissionReport::Unavailable(reason) => {
            HoverBudgetAdmissionReport::Unavailable(admission_unavailable_from_backend(reason))
        }
        BackendHoverBudgetAdmissionReport::Fatal(reason) => {
            HoverBudgetAdmissionReport::Fatal(admission_fatal_from_backend(reason))
        }
    }
}

fn backend_resolved_budget_from_core(
    resolved_budget: &HoverResolvedBudget,
) -> BackendHoverResolvedBudget {
    BackendHoverResolvedBudget::new(
        resolved_budget
            .resources()
            .iter()
            .copied()
            .map(|resource| {
                BackendHoverResolvedBudgetResource::new(
                    resource_class_to_backend(resource.resource_class()),
                    resource.resolved_budget().get(),
                )
            })
            .collect(),
    )
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

fn unsupported_reason_from_backend(
    reason: BackendHoverBudgetUnsupportedReason,
) -> frame_server_core::HoverBudgetUnsupportedReason {
    match reason {
        BackendHoverBudgetUnsupportedReason::MissingPlayableOutput => {
            frame_server_core::HoverBudgetUnsupportedReason::MissingPlayableOutput
        }
        BackendHoverBudgetUnsupportedReason::BackendDoesNotSupportHover => {
            frame_server_core::HoverBudgetUnsupportedReason::BackendDoesNotSupportHover
        }
        BackendHoverBudgetUnsupportedReason::UnsupportedResourceClass { resource_class } => {
            frame_server_core::HoverBudgetUnsupportedReason::UnsupportedResourceClass {
                resource_class: resource_class_from_backend(resource_class),
            }
        }
    }
}

fn capability_unavailable_from_backend(
    reason: BackendHoverBudgetCapabilityUnavailableReason,
) -> frame_server_core::HoverBudgetCapabilityUnavailableReason {
    match reason {
        BackendHoverBudgetCapabilityUnavailableReason::BackendNotReady => {
            frame_server_core::HoverBudgetCapabilityUnavailableReason::BackendNotReady
        }
        BackendHoverBudgetCapabilityUnavailableReason::MediaContextUnavailable => {
            frame_server_core::HoverBudgetCapabilityUnavailableReason::MediaContextUnavailable
        }
        BackendHoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable => {
            frame_server_core::HoverBudgetCapabilityUnavailableReason::ResourceProviderUnavailable
        }
    }
}

fn resource_pressure_from_backend(
    reason: BackendHoverBudgetResourcePressureReason,
) -> frame_server_core::HoverBudgetResourcePressureReason {
    match reason {
        BackendHoverBudgetResourcePressureReason::ActivePlaybackReservation => {
            frame_server_core::HoverBudgetResourcePressureReason::ActivePlaybackReservation
        }
        BackendHoverBudgetResourcePressureReason::ExistingHoverReservation => {
            frame_server_core::HoverBudgetResourcePressureReason::ExistingHoverReservation
        }
        BackendHoverBudgetResourcePressureReason::ProviderCapacityExhausted => {
            frame_server_core::HoverBudgetResourcePressureReason::ProviderCapacityExhausted
        }
    }
}

fn admission_unavailable_from_backend(
    reason: BackendHoverBudgetAdmissionUnavailableReason,
) -> frame_server_core::HoverBudgetAdmissionUnavailableReason {
    match reason {
        BackendHoverBudgetAdmissionUnavailableReason::ReservationOwnerUnavailable => {
            frame_server_core::HoverBudgetAdmissionUnavailableReason::ReservationOwnerUnavailable
        }
        BackendHoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable => {
            frame_server_core::HoverBudgetAdmissionUnavailableReason::ResourceProviderUnavailable
        }
    }
}

fn admission_fatal_from_backend(
    reason: BackendHoverBudgetAdmissionFatalReason,
) -> frame_server_core::HoverBudgetAdmissionFatalReason {
    match reason {
        BackendHoverBudgetAdmissionFatalReason::ProviderInvariantViolated => {
            frame_server_core::HoverBudgetAdmissionFatalReason::ProviderInvariantViolated
        }
    }
}

fn resolution_unavailable_label(reason: HoverBudgetResolutionUnavailableReason) -> String {
    match reason {
        HoverBudgetResolutionUnavailableReason::FixedBudgetBelowBackendMinimum {
            resource_class,
            fixed_budget,
            backend_minimum,
        } => format!(
            "{} fixed={} меньше backend minimum={}",
            resource_class_label(resource_class),
            fixed_budget,
            backend_minimum
        ),
        HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
            resource_class,
            fixed_budget,
            playback_budget,
        } => format!(
            "{} fixed={} не меньше playback budget={}",
            resource_class_label(resource_class),
            fixed_budget,
            playback_budget
        ),
        other => format!("{other:?}"),
    }
}

pub(crate) fn resource_class_label(resource_class: HoverBudgetResourceClass) -> &'static str {
    match resource_class {
        HoverBudgetResourceClass::HardwareSurfaceFrames => "hardware_surfaces",
        HoverBudgetResourceClass::SoftwareFramePoolFrames => "software_pool_frames",
        HoverBudgetResourceClass::SoftwareThreadCount => "software_threads",
    }
}

pub(crate) fn budget_setting_label(setting: HoverBudgetSetting) -> String {
    match setting {
        HoverBudgetSetting::Auto => "auto".to_string(),
        HoverBudgetSetting::Fixed(value) => format!("fixed:{}", value.get()),
    }
}

pub(crate) fn admission_outcome_label(outcome: &Option<HoverBudgetAdmissionOutcome>) -> String {
    match outcome {
        Some(HoverBudgetAdmissionOutcome::Admitted(_)) => "admitted".to_string(),
        Some(HoverBudgetAdmissionOutcome::Rejected { reason, .. }) => {
            format!("rejected:{}", admission_rejection_label(*reason))
        }
        None => "not_resolved".to_string(),
    }
}

fn admission_rejection_label(reason: HoverBudgetAdmissionRejection) -> &'static str {
    match reason {
        HoverBudgetAdmissionRejection::ResourcePressure(_) => "resource_pressure",
        HoverBudgetAdmissionRejection::Unavailable(_) => "unavailable",
        HoverBudgetAdmissionRejection::Fatal(_) => "fatal",
    }
}

fn non_zero_or_one(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or_else(|| NonZeroUsize::new(1).expect("1 is non-zero"))
}

#[cfg(test)]
mod tests {
    use frame_server_core::{HoverBudgetResolutionSource, HoverResolvedBudgetResource};
    use video_backend_api::{
        BackendHoverBudgetCapabilityMinimum, BackendHoverBudgetResourceClass,
        HoverBudgetDiagnosticsProvider,
    };

    use super::*;

    #[derive(Clone)]
    struct ScriptedBudgetProvider {
        capability_report: BackendHoverBudgetCapabilityReport,
        admission_report: BackendHoverBudgetAdmissionReport,
    }

    impl HoverBudgetDiagnosticsProvider for ScriptedBudgetProvider {
        fn hover_capability_report(&self) -> BackendHoverBudgetCapabilityReport {
            self.capability_report.clone()
        }

        fn hover_admission_report(
            &self,
            _resolved_budget: &BackendHoverResolvedBudget,
        ) -> BackendHoverBudgetAdmissionReport {
            self.admission_report
        }
    }

    fn nz(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test value must be positive")
    }

    fn provider_for_software_minimum(
        pool_minimum: usize,
        thread_minimum: usize,
    ) -> HoverBudgetDiagnosticsProviderHandle {
        HoverBudgetDiagnosticsProviderHandle::new(ScriptedBudgetProvider {
            capability_report: BackendHoverBudgetCapabilityReport::Supported(vec![
                BackendHoverBudgetCapabilityMinimum::reported(
                    BackendHoverBudgetResourceClass::SoftwareFramePoolFrames,
                    pool_minimum,
                ),
                BackendHoverBudgetCapabilityMinimum::reported(
                    BackendHoverBudgetResourceClass::SoftwareThreadCount,
                    thread_minimum,
                ),
            ]),
            admission_report: BackendHoverBudgetAdmissionReport::Admitted,
        })
    }

    #[test]
    fn diagnostics_resolve_auto_from_backend_reported_minimums() {
        let provider = provider_for_software_minimum(3, 2);
        let config = FrameServerConfig {
            hover_pool_frames: FrameServerBudgetConfig::Auto,
            hover_thread_count: FrameServerBudgetConfig::Auto,
            ..FrameServerConfig::default()
        };
        let decoder_config = PlayerVideoDecoderThreadConfig {
            software_frame_pool_frames: 12,
            software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::Fixed(nz(6)),
            ..PlayerVideoDecoderThreadConfig::default()
        };

        let diagnostics = frame_server_hover_budget_diagnostics(
            VideoBackendKind::FfmpegSoftware,
            &config,
            decoder_config,
            &provider,
        )
        .expect("auto budget diagnostics must resolve");

        assert_eq!(diagnostics.backend_kind.label(), "ffmpeg_software");
        assert!(matches!(
            diagnostics.resolution_outcome,
            HoverBudgetResolutionOutcome::Resolved(_)
        ));
        assert_eq!(
            diagnostics
                .resources
                .iter()
                .find(|resource| resource.resource_class
                    == HoverBudgetResourceClass::SoftwareFramePoolFrames)
                .and_then(|resource| resource.backend_reported_minimum)
                .map(NonZeroUsize::get),
            Some(3)
        );
        assert_eq!(
            diagnostics
                .resources
                .iter()
                .find(|resource| resource.resource_class
                    == HoverBudgetResourceClass::SoftwareThreadCount)
                .and_then(|resource| resource.resolved_hover_budget)
                .map(NonZeroUsize::get),
            Some(2)
        );
    }

    #[test]
    fn preflight_rejects_changed_fixed_budget_without_rewriting_config() {
        let provider = provider_for_software_minimum(2, 2);
        let current = FrameServerConfig::default();
        let mut draft = current.clone();
        draft.hover_pool_frames = FrameServerBudgetConfig::Fixed(12);
        let decoder_config = PlayerVideoDecoderThreadConfig {
            software_frame_pool_frames: 8,
            software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::Fixed(nz(6)),
            ..PlayerVideoDecoderThreadConfig::default()
        };

        let rejection = preflight_frame_server_hover_budget_change(
            &current,
            &draft,
            Some(VideoBackendKind::FfmpegSoftware),
            decoder_config,
            Some(&provider),
        )
        .expect_err("changed fixed budget at playback budget must be rejected");

        assert!(matches!(
            rejection.kind,
            FrameServerHoverBudgetPreflightRejectionKind::FixedTooLarge {
                reason: HoverBudgetResolutionUnavailableReason::FixedBudgetNotBelowPlayback {
                    resource_class: HoverBudgetResourceClass::SoftwareFramePoolFrames,
                    ..
                }
            }
        ));
        assert_eq!(draft.hover_pool_frames, FrameServerBudgetConfig::Fixed(12));
    }

    #[test]
    fn already_loaded_fixed_too_large_is_diagnostics_only() {
        let provider = provider_for_software_minimum(2, 2);
        let current = FrameServerConfig {
            hover_pool_frames: FrameServerBudgetConfig::Fixed(12),
            ..FrameServerConfig::default()
        };
        let mut draft = current.clone();
        draft.hover_preview_enabled = false;
        let decoder_config = PlayerVideoDecoderThreadConfig {
            software_frame_pool_frames: 8,
            software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::Fixed(nz(6)),
            ..PlayerVideoDecoderThreadConfig::default()
        };

        let diagnostics = preflight_frame_server_hover_budget_change(
            &current,
            &draft,
            Some(VideoBackendKind::FfmpegSoftware),
            decoder_config,
            Some(&provider),
        )
        .expect("unchanged fixed-too-large budget must not block unrelated settings")
        .expect("active provider should expose diagnostics");

        assert!(matches!(
            diagnostics.resolution_outcome,
            HoverBudgetResolutionOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn admission_pressure_is_reported_without_preflight_reject() {
        let provider = HoverBudgetDiagnosticsProviderHandle::new(ScriptedBudgetProvider {
            capability_report: BackendHoverBudgetCapabilityReport::Supported(vec![
                BackendHoverBudgetCapabilityMinimum::reported(
                    BackendHoverBudgetResourceClass::HardwareSurfaceFrames,
                    3,
                ),
            ]),
            admission_report: BackendHoverBudgetAdmissionReport::ResourcePressure(
                BackendHoverBudgetResourcePressureReason::ExistingHoverReservation,
            ),
        });
        let current = FrameServerConfig {
            hover_pool_frames: FrameServerBudgetConfig::Fixed(3),
            ..FrameServerConfig::default()
        };
        let mut draft = current.clone();
        draft.hover_preview_enabled = false;
        let decoder_config = PlayerVideoDecoderThreadConfig {
            decoder_surface_pool_frames: 12,
            ..PlayerVideoDecoderThreadConfig::default()
        };

        let diagnostics = preflight_frame_server_hover_budget_change(
            &current,
            &draft,
            Some(VideoBackendKind::HardwareZeroCopy),
            decoder_config,
            Some(&provider),
        )
        .expect("current provider pressure should be diagnostics, not config rewrite")
        .expect("provider exists");

        assert!(matches!(
            diagnostics.admission_outcome,
            Some(HoverBudgetAdmissionOutcome::Rejected { .. })
        ));
        assert_eq!(
            diagnostics.admission_outcome,
            Some(HoverBudgetAdmissionOutcome::Rejected {
                resolved_budget: HoverResolvedBudget::new(vec![HoverResolvedBudgetResource::new(
                    HoverBudgetResourceClass::HardwareSurfaceFrames,
                    nz(3),
                    HoverBudgetResolutionSource::FixedConfig,
                )]),
                reason: HoverBudgetAdmissionRejection::ResourcePressure(
                    frame_server_core::HoverBudgetResourcePressureReason::ExistingHoverReservation,
                ),
            })
        );
    }
}
