//! Legacy live/container proof path, отделённый от VOD target-aware initial handoff.

use demux_api::{DemuxHints, DemuxOpenError, DemuxProbeRejection, DemuxProbedOpen};
use hls_playlist_core::MediaPlaylist;
use source_core::HttpRequestTarget;
use web_media_adaptive::AdaptiveResourcePurpose;

use super::HlsVodOpenError;
use crate::plan::{HlsEpochPlan, HlsPlanError, build_component_plan};
use crate::source::HlsEpochSegmentSource;
use crate::{HlsContainerEvidence, HlsRequiredContainer, HlsVodOpenRequest};

pub(crate) fn required_main_container(
    media: &MediaPlaylist,
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
) -> Result<HlsRequiredContainer, HlsVodOpenError> {
    match request.containers.main {
        HlsContainerEvidence::Exact(container) => Ok(container),
        HlsContainerEvidence::ContentProbe => {
            probe_component_container(media, base, request, ContainerProbeRole::Main)
        }
        HlsContainerEvidence::Missing => Err(HlsVodOpenError::MissingMainContainerEvidence),
        HlsContainerEvidence::Ambiguous => Err(HlsVodOpenError::AmbiguousMainContainerEvidence),
    }
}

pub(crate) fn required_audio_container(
    media: &MediaPlaylist,
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
) -> Result<HlsRequiredContainer, HlsVodOpenError> {
    match request.containers.alternate_audio {
        Some(HlsContainerEvidence::Exact(container)) => Ok(container),
        Some(HlsContainerEvidence::ContentProbe) => {
            probe_component_container(media, base, request, ContainerProbeRole::AlternateAudio)
        }
        None | Some(HlsContainerEvidence::Missing) => {
            Err(HlsVodOpenError::MissingAudioContainerEvidence)
        }
        Some(HlsContainerEvidence::Ambiguous) => {
            Err(HlsVodOpenError::AmbiguousAudioContainerEvidence)
        }
    }
}

#[derive(Clone, Copy)]
enum ContainerProbeRole {
    Main,
    AlternateAudio,
}

fn probe_component_container(
    media: &MediaPlaylist,
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
    role: ContainerProbeRole,
) -> Result<HlsRequiredContainer, HlsVodOpenError> {
    let provisional = build_component_plan(
        media,
        HlsRequiredContainer::TransportStream,
        base,
        &request.overrides,
    )?;
    provisional.validate_resource_bound(
        request
            .http
            .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment),
    )?;
    let first_epoch = provisional
        .epochs
        .first()
        .cloned()
        .ok_or(HlsPlanError::EmptyMediaPlaylist)?;
    let opened = open_epoch_probe(request, first_epoch).map_err(|error| match role {
        ContainerProbeRole::Main => HlsVodOpenError::MainContainerProbeOpen(error),
        ContainerProbeRole::AlternateAudio => HlsVodOpenError::AudioContainerProbeOpen(error),
    })?;
    let transport_stream = HlsRequiredContainer::TransportStream
        .demux_container_id()
        .map_err(|_| unsupported_container(role))?;
    let fragmented_mp4 = HlsRequiredContainer::FragmentedMp4
        .demux_container_id()
        .map_err(|_| unsupported_container(role))?;
    match opened.container() {
        container if container == &transport_stream => Ok(HlsRequiredContainer::TransportStream),
        container if container == &fragmented_mp4 => Ok(HlsRequiredContainer::FragmentedMp4),
        _ => Err(unsupported_container(role)),
    }
}

/// Открывает container probe через pull-stream и сохраняет segmented fallback для fMP4 factory.
///
/// Transport Stream factory умеет определить tracks по небольшому bounded префиксу. Старый
/// `OrderedSegments` boundary сначала скачивал целиком многомегабайтный media segment, хотя
/// catalog/runtime нужен был только PSI/probe. Для factory без streaming capability (сейчас это
/// fMP4) повторяем probe через прежний контракт, не превращая сетевые и parse-ошибки в fallback.
pub(crate) fn open_epoch_probe(
    request: &HlsVodOpenRequest,
    first_epoch: HlsEpochPlan,
) -> Result<DemuxProbedOpen, DemuxOpenError> {
    let streaming_source = HlsEpochSegmentSource::new(
        request.http.clone(),
        request.generation,
        first_epoch.clone(),
        request.policy.maximum_key_resource_bytes,
    );
    let streaming_result = request.demux_registry.open_probed(
        streaming_source.into_demux_input(HlsRequiredContainer::TransportStream),
        DemuxHints::none(),
        request.policy.demux_sniff_budget,
        request.http.cancellation().clone(),
    );
    match streaming_result {
        Ok(opened) => Ok(opened),
        Err(
            DemuxOpenError::NoMatch
            | DemuxOpenError::ProbeRejected(DemuxProbeRejection::UnsupportedInput { .. }),
        ) => {
            let segmented_source = HlsEpochSegmentSource::new(
                request.http.clone(),
                request.generation,
                first_epoch,
                request.policy.maximum_key_resource_bytes,
            );
            request.demux_registry.open_probed(
                segmented_source.into_demux_input(HlsRequiredContainer::FragmentedMp4),
                DemuxHints::none(),
                request.policy.demux_sniff_budget,
                request.http.cancellation().clone(),
            )
        }
        Err(error) => Err(error),
    }
}

fn unsupported_container(role: ContainerProbeRole) -> HlsVodOpenError {
    match role {
        ContainerProbeRole::Main => HlsVodOpenError::UnsupportedMainContainer,
        ContainerProbeRole::AlternateAudio => HlsVodOpenError::UnsupportedAudioContainer,
    }
}
