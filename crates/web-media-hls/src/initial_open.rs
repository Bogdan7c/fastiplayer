//! Target-aware initial HLS VOD open и one-response content-probe handoff.

use demux_api::{DemuxHints, DemuxOpenError, DemuxProbedOpen};
use hls_playlist_core::{HlsPlaylist, MediaPlaylist, validate_vod_profile};
use source_core::HttpRequestTarget;
use web_media_adaptive::AdaptiveResourcePurpose;

use crate::active_read::{HlsComponentActiveReadControl, HlsEpochActiveReadLifecycle};
use crate::epoch_demux::{HlsInitialComponentOpen, HlsProbedInitialComponent};
use crate::open::{HlsVodOpenError, validate_and_plan_media};
use crate::plan::{HlsComponentPlan, HlsEpochPlan, HlsManifestSeekPoint, build_component_plan};
use crate::source::{HlsEpochSegmentSource, SharedHlsMediaSpanIndex};
use crate::start::{HlsResolvedVodStartIntent, HlsVodStartDisposition};
use crate::{
    HlsContainerEvidence, HlsRequiredContainer, HlsVodOpenRequest, HlsVodSeekLandingPolicy,
    HlsVodStartIntent,
};

/// Роль нужна только для точной typed-классификации container probe errors.
#[derive(Clone, Copy)]
pub(crate) enum HlsInitialComponentRole {
    Main,
    AlternateAudio,
}

/// Manifest plan вместе с exact способом единственного initial open.
pub(crate) struct HlsPreparedInitialComponent {
    pub(crate) plan: HlsComponentPlan,
    pub(crate) initial_open: HlsInitialComponentOpen,
    pub(crate) active_read_control: HlsComponentActiveReadControl,
    pub(crate) start_disposition: HlsVodStartDisposition,
}

#[derive(Clone, Copy)]
enum HlsProbePosition {
    Beginning,
    Restore {
        target: media_core::MediaTime,
        point: HlsManifestSeekPoint,
    },
}

struct HlsOpenedProbeAttempt {
    opened: DemuxProbedOpen,
    media_spans: SharedHlsMediaSpanIndex,
    active_read_lifecycle: HlsEpochActiveReadLifecycle,
}

/// Валидирует VOD, выбирает containing segment до первого GET и сохраняет probed demuxer.
pub(crate) fn prepare_initial_component(
    media: MediaPlaylist,
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
    evidence: HlsContainerEvidence,
    role: HlsInitialComponentRole,
    start: HlsVodStartIntent,
) -> Result<HlsPreparedInitialComponent, HlsVodOpenError> {
    let active_read_control = HlsComponentActiveReadControl::new();
    match evidence {
        HlsContainerEvidence::Exact(container) => {
            let plan = validate_and_plan_media(media, container, base, request)?;
            let resolved_start = start
                .resolve_for_duration(plan.duration)
                .map_err(|_| HlsVodOpenError::InitialRestoreOutsideVod)?;
            Ok(HlsPreparedInitialComponent {
                plan,
                initial_open: HlsInitialComponentOpen::Fresh(resolved_start.intent),
                active_read_control,
                start_disposition: resolved_start.disposition,
            })
        }
        HlsContainerEvidence::ContentProbe => {
            // Profile проверяется до media I/O: malformed/non-VOD playlist не получает probe GET.
            validate_vod_profile(&HlsPlaylist::Media(media.clone()), None)?;
            let provisional = build_component_plan(
                &media,
                HlsRequiredContainer::TransportStream,
                base,
                &request.overrides,
            )?;
            provisional.validate_resource_bound(
                request
                    .http
                    .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment),
            )?;
            let resolved_start = start
                .resolve_for_duration(provisional.duration)
                .map_err(|_| HlsVodOpenError::InitialRestoreOutsideVod)?;
            let position = probe_position(
                &provisional,
                resolved_start.intent,
                request.policy.seek_landing_policy,
            )?;
            let epoch = probe_epoch(&provisional, position)?;
            let probed = probe_streaming_then_finite(request, epoch, role, &active_read_control)?;
            let container = required_probed_container(probed.opened.container(), role)?;
            let plan = validate_and_plan_media(media, container, base, request)?;
            validate_probed_position(&plan, position, request.policy.seek_landing_policy)?;
            let current = probed.opened.into_demuxer();
            let initial_open = match position {
                HlsProbePosition::Beginning => {
                    HlsInitialComponentOpen::Probed(HlsProbedInitialComponent::beginning(
                        current,
                        probed.media_spans,
                        probed.active_read_lifecycle,
                    ))
                }
                HlsProbePosition::Restore { target, point } => {
                    HlsInitialComponentOpen::Probed(HlsProbedInitialComponent::restore(
                        target,
                        point,
                        current,
                        probed.media_spans,
                        probed.active_read_lifecycle,
                    ))
                }
            };
            Ok(HlsPreparedInitialComponent {
                plan,
                initial_open,
                active_read_control,
                start_disposition: resolved_start.disposition,
            })
        }
        HlsContainerEvidence::Missing => Err(match role {
            HlsInitialComponentRole::Main => HlsVodOpenError::MissingMainContainerEvidence,
            HlsInitialComponentRole::AlternateAudio => {
                HlsVodOpenError::MissingAudioContainerEvidence
            }
        }),
        HlsContainerEvidence::Ambiguous => Err(match role {
            HlsInitialComponentRole::Main => HlsVodOpenError::AmbiguousMainContainerEvidence,
            HlsInitialComponentRole::AlternateAudio => {
                HlsVodOpenError::AmbiguousAudioContainerEvidence
            }
        }),
    }
}

fn probe_position(
    plan: &HlsComponentPlan,
    start: HlsResolvedVodStartIntent,
    landing_policy: HlsVodSeekLandingPolicy,
) -> Result<HlsProbePosition, HlsVodOpenError> {
    match start {
        HlsResolvedVodStartIntent::Beginning => Ok(HlsProbePosition::Beginning),
        HlsResolvedVodStartIntent::Restore(target) => {
            initial_manifest_seek_point(plan, target.as_duration(), landing_policy)
                .map(|point| HlsProbePosition::Restore { target, point })
                .ok_or(HlsVodOpenError::InitialRestoreCandidateMissing)
        }
    }
}

/// Выбирает первый HTTP resource initial restore-а до container probe.
fn initial_manifest_seek_point(
    plan: &HlsComponentPlan,
    target: std::time::Duration,
    landing_policy: HlsVodSeekLandingPolicy,
) -> Option<HlsManifestSeekPoint> {
    match landing_policy {
        HlsVodSeekLandingPolicy::DecodeFromOrBeforeTarget => {
            plan.containing_manifest_seek_point(target)
        }
        HlsVodSeekLandingPolicy::PreferPostTargetRap => plan
            .post_target_manifest_seek_point(target)
            .or_else(|| plan.containing_manifest_seek_point(target)),
    }
}

fn probe_epoch(
    plan: &HlsComponentPlan,
    position: HlsProbePosition,
) -> Result<HlsEpochPlan, HlsVodOpenError> {
    match position {
        HlsProbePosition::Beginning => plan
            .epochs
            .first()
            .cloned()
            .ok_or_else(|| HlsVodOpenError::Plan(crate::HlsVodPlanError::EmptyMediaPlaylist)),
        HlsProbePosition::Restore { point, .. } => plan
            .manifest_restart_tail(point)
            .ok_or(HlsVodOpenError::InitialRestoreCandidateMissing),
    }
}

fn validate_probed_position(
    plan: &HlsComponentPlan,
    position: HlsProbePosition,
    landing_policy: HlsVodSeekLandingPolicy,
) -> Result<(), HlsVodOpenError> {
    match position {
        HlsProbePosition::Beginning => {
            if plan.epochs.is_empty() {
                Err(HlsVodOpenError::InitialRestoreCandidateMissing)
            } else {
                Ok(())
            }
        }
        HlsProbePosition::Restore { target, point } => {
            let exact = initial_manifest_seek_point(plan, target.as_duration(), landing_policy);
            if exact == Some(point) {
                Ok(())
            } else {
                Err(HlsVodOpenError::InitialRestoreCandidateMissing)
            }
        }
    }
}

fn probe_streaming_then_finite(
    request: &HlsVodOpenRequest,
    epoch: HlsEpochPlan,
    role: HlsInitialComponentRole,
    active_read_control: &HlsComponentActiveReadControl,
) -> Result<HlsOpenedProbeAttempt, HlsVodOpenError> {
    match open_probe_attempt(
        request,
        epoch.clone(),
        HlsRequiredContainer::TransportStream,
        active_read_control,
    ) {
        Ok(opened) => Ok(opened),
        Err(DemuxOpenError::NoMatch) => open_probe_attempt(
            request,
            epoch,
            HlsRequiredContainer::FragmentedMp4,
            active_read_control,
        )
        .map_err(|error| probe_open_error(role, error)),
        Err(error) => Err(probe_open_error(role, error)),
    }
}

fn open_probe_attempt(
    request: &HlsVodOpenRequest,
    epoch: HlsEpochPlan,
    input_boundary: HlsRequiredContainer,
    active_read_control: &HlsComponentActiveReadControl,
) -> Result<HlsOpenedProbeAttempt, DemuxOpenError> {
    let media_spans = SharedHlsMediaSpanIndex::default();
    let active_read_lifecycle = active_read_control.new_epoch_lifecycle(&epoch);
    let source = HlsEpochSegmentSource::new_with_media_span_index(
        request.http.clone(),
        request.generation,
        epoch,
        request.policy.maximum_key_resource_bytes,
        media_spans.clone(),
    )
    .with_active_read_lifecycle(active_read_lifecycle.clone());
    let opened = request.demux_registry.open_probed(
        source.into_demux_input(input_boundary),
        DemuxHints::none(),
        request.policy.demux_sniff_budget,
        request.http.cancellation().clone(),
    )?;
    Ok(HlsOpenedProbeAttempt {
        opened,
        media_spans,
        active_read_lifecycle,
    })
}

fn required_probed_container(
    container: &demux_api::DemuxContainerId,
    role: HlsInitialComponentRole,
) -> Result<HlsRequiredContainer, HlsVodOpenError> {
    let transport_stream = HlsRequiredContainer::TransportStream
        .demux_container_id()
        .map_err(|_| unsupported_container(role))?;
    let fragmented_mp4 = HlsRequiredContainer::FragmentedMp4
        .demux_container_id()
        .map_err(|_| unsupported_container(role))?;
    if container == &transport_stream {
        Ok(HlsRequiredContainer::TransportStream)
    } else if container == &fragmented_mp4 {
        Ok(HlsRequiredContainer::FragmentedMp4)
    } else {
        Err(unsupported_container(role))
    }
}

fn probe_open_error(role: HlsInitialComponentRole, error: DemuxOpenError) -> HlsVodOpenError {
    match role {
        HlsInitialComponentRole::Main => HlsVodOpenError::MainContainerProbeOpen(error),
        HlsInitialComponentRole::AlternateAudio => HlsVodOpenError::AudioContainerProbeOpen(error),
    }
}

fn unsupported_container(role: HlsInitialComponentRole) -> HlsVodOpenError {
    match role {
        HlsInitialComponentRole::Main => HlsVodOpenError::UnsupportedMainContainer,
        HlsInitialComponentRole::AlternateAudio => HlsVodOpenError::UnsupportedAudioContainer,
    }
}
