use std::time::Duration;

use demux_api::{DemuxHints, DemuxInput};
use hls_playlist_core::{
    HlsPlaylist, MediaContainerIntent, MediaPlaylist, validate_initial_profile,
    validate_live_profile, validate_vod_profile,
};
use media_core::{TrackInfo, TrackKind};
use source_core::HttpRequestTarget;
use web_media_adaptive::{AdaptiveResourcePurpose, AdaptiveTransportError};

use super::*;
use crate::open::{fetch_manifest, load_top_playlist, parse_playlist};
use crate::plan::{
    HlsComponentPlan, build_component_plan, build_segment_scoped_component_plan, parse_hls_duration,
};
use crate::source::HlsEpochSegmentSource;
use crate::{HlsRequiredContainer, HlsVodOpenRequest};

#[derive(Clone, PartialEq, Eq)]
struct TimelineSignature(Box<[(Duration, u64)]>);

struct DiscoveryProofPort<'request, 'capability> {
    request: &'request HlsVodOpenRequest,
    base: HttpRequestTarget,
    presentation: HlsCatalogPresentation,
    capability: &'capability mut dyn HlsCatalogCapabilityProofPort,
    alignments: Vec<TimelineSignature>,
}

/// Загружает authoritative master быстро и отдельно доказывает bounded sibling catalog.
pub fn discover_hls_catalog(
    request: HlsCatalogDiscoveryRequest<'_>,
    capability: &mut dyn HlsCatalogCapabilityProofPort,
) -> Result<HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryError> {
    let (playlist, base, was_inline) = load_top_playlist(request.open)?;
    validate_initial_profile(&playlist).map_err(HlsVodOpenError::Profile)?;
    let HlsPlaylist::Master(master) = playlist else {
        return Ok(HlsCatalogDiscoveryOutcome::Unavailable);
    };
    if was_inline {
        return Err(HlsVodOpenError::InlineManifestWasMaster.into());
    }

    let mut proof_port = DiscoveryProofPort {
        request: request.open,
        base,
        presentation: request.presentation,
        capability,
        alignments: Vec::new(),
    };
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: request.catalog_identity,
            provider_default: &request.open.selection,
            policy: request.policy,
        },
        &mut proof_port,
    )?;
    Ok(HlsCatalogDiscoveryOutcome::Installed(snapshot.into()))
}

impl HlsCatalogChildProofPort for DiscoveryProofPort<'_, '_> {
    fn prove_child(
        &mut self,
        request: HlsCatalogChildProbe,
    ) -> Result<HlsCatalogChildProof, HlsCatalogChildProofError> {
        self.prove_child_resource(&request)
            .map_err(classify_child_error)
    }
}

impl DiscoveryProofPort<'_, '_> {
    fn prove_child_resource(
        &mut self,
        child: &HlsCatalogChildProbe,
    ) -> Result<HlsCatalogChildProof, ChildProofFailure> {
        let target = self
            .base
            .resolve_reference(child.reference.expose_for_resolution())
            .map_err(|_| ChildProofFailure::InvalidManifest)?;
        let resource = fetch_manifest(target, self.request).map_err(ChildProofFailure::Open)?;
        let playlist = parse_playlist(resource.bytes(), resource.final_target(), self.request)
            .map_err(ChildProofFailure::Open)?;
        let HlsPlaylist::Media(media) = playlist else {
            return Err(ChildProofFailure::InvalidManifest);
        };
        self.validate_profile(&media, None)?;

        let provisional = self.build_plan(
            &media,
            HlsRequiredContainer::TransportStream,
            resource.final_target(),
        )?;
        provisional
            .validate_resource_bound(
                self.request
                    .http
                    .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment),
            )
            .map_err(|_| ChildProofFailure::InvalidManifest)?;
        let first_epoch = provisional
            .epochs
            .first()
            .cloned()
            .ok_or(ChildProofFailure::InvalidManifest)?;
        let source = HlsEpochSegmentSource::new(
            self.request.http.clone(),
            self.request.generation,
            first_epoch,
            self.request.policy.maximum_key_resource_bytes,
        );
        let opened = self
            .request
            .demux_registry
            .open_probed(
                DemuxInput::ordered_segments(Box::new(source)),
                DemuxHints::none(),
                self.request.policy.demux_sniff_budget,
                self.request.http.cancellation().clone(),
            )
            .map_err(|_| ChildProofFailure::UnsupportedContainer)?;
        let container = required_container(opened.container())?;
        self.validate_profile(&media, Some(container))?;
        let demuxer = opened.into_demuxer();
        let tracks = prove_track_shape(demuxer.tracks(), self.capability)?;
        let alignment = self.alignment_for(&media)?;
        Ok(HlsCatalogChildProof {
            container,
            tracks,
            alignment,
        })
    }

    fn build_plan(
        &self,
        media: &MediaPlaylist,
        container: HlsRequiredContainer,
        base: &HttpRequestTarget,
    ) -> Result<HlsComponentPlan, ChildProofFailure> {
        match self.presentation {
            HlsCatalogPresentation::Vod => {
                build_component_plan(media, container, base, &self.request.overrides)
            }
            HlsCatalogPresentation::Live => {
                build_segment_scoped_component_plan(media, container, base, &self.request.overrides)
            }
        }
        .map_err(|_| ChildProofFailure::InvalidManifest)
    }

    fn validate_profile(
        &self,
        media: &MediaPlaylist,
        container: Option<HlsRequiredContainer>,
    ) -> Result<(), ChildProofFailure> {
        let intent = container.map(container_intent);
        let playlist = HlsPlaylist::Media(media.clone());
        match self.presentation {
            HlsCatalogPresentation::Vod => validate_vod_profile(&playlist, intent),
            HlsCatalogPresentation::Live => validate_live_profile(&playlist, intent),
        }
        .map_err(|_| ChildProofFailure::InvalidManifest)
    }

    fn alignment_for(
        &mut self,
        media: &MediaPlaylist,
    ) -> Result<HlsCatalogAlignmentProof, ChildProofFailure> {
        let base_discontinuity = media
            .segments
            .first()
            .map_or(media.discontinuity_sequence, |segment| {
                segment.discontinuity_sequence
            });
        let signature = TimelineSignature(
            media
                .segments
                .iter()
                .map(|segment| {
                    Ok((
                        parse_hls_duration(&segment.duration)
                            .map_err(|_| ChildProofFailure::InvalidManifest)?,
                        segment
                            .discontinuity_sequence
                            .checked_sub(base_discontinuity)
                            .ok_or(ChildProofFailure::InvalidManifest)?,
                    ))
                })
                .collect::<Result<Vec<_>, ChildProofFailure>>()?
                .into_boxed_slice(),
        );
        if let Some(index) = self
            .alignments
            .iter()
            .position(|candidate| candidate == &signature)
        {
            return Ok(HlsCatalogAlignmentProof::new(index as u64 + 1));
        }
        self.alignments.push(signature);
        Ok(HlsCatalogAlignmentProof::new(self.alignments.len() as u64))
    }
}

fn prove_track_shape(
    tracks: &[TrackInfo],
    capability: &mut dyn HlsCatalogCapabilityProofPort,
) -> Result<HlsCatalogTrackProof, ChildProofFailure> {
    let videos = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .collect::<Vec<_>>();
    let audios = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .collect::<Vec<_>>();
    match (videos.as_slice(), audios.as_slice()) {
        ([video], []) => capability
            .prove_video(video)
            .map(HlsCatalogTrackProof::VideoOnly),
        ([], [audio]) => capability
            .prove_audio(audio)
            .map(HlsCatalogTrackProof::AudioOnly),
        ([video], [audio]) => capability.prove_video(video).and_then(|video| {
            capability
                .prove_audio(audio)
                .map(|audio| HlsCatalogTrackProof::Muxed { video, audio })
        }),
        _ => return Err(ChildProofFailure::UnsupportedTrackShape),
    }
    .map_err(|_| ChildProofFailure::CapabilityRejected)
}

fn required_container(
    actual: &demux_api::DemuxContainerId,
) -> Result<HlsRequiredContainer, ChildProofFailure> {
    let transport_stream = HlsRequiredContainer::TransportStream
        .demux_container_id()
        .map_err(|_| ChildProofFailure::UnsupportedContainer)?;
    let fragmented_mp4 = HlsRequiredContainer::FragmentedMp4
        .demux_container_id()
        .map_err(|_| ChildProofFailure::UnsupportedContainer)?;
    if actual == &transport_stream {
        Ok(HlsRequiredContainer::TransportStream)
    } else if actual == &fragmented_mp4 {
        Ok(HlsRequiredContainer::FragmentedMp4)
    } else {
        Err(ChildProofFailure::UnsupportedContainer)
    }
}

const fn container_intent(container: HlsRequiredContainer) -> MediaContainerIntent {
    match container {
        HlsRequiredContainer::TransportStream => MediaContainerIntent::TransportStream,
        HlsRequiredContainer::FragmentedMp4 => MediaContainerIntent::FragmentedMp4,
    }
}

enum ChildProofFailure {
    Open(HlsVodOpenError),
    InvalidManifest,
    UnsupportedContainer,
    UnsupportedTrackShape,
    CapabilityRejected,
}

fn classify_child_error(error: ChildProofFailure) -> HlsCatalogChildProofError {
    match error {
        ChildProofFailure::Open(HlsVodOpenError::Transport(AdaptiveTransportError::Cancelled)) => {
            HlsCatalogChildProofError::Cancelled
        }
        ChildProofFailure::Open(HlsVodOpenError::Transport(
            AdaptiveTransportError::StaleGeneration { .. },
        )) => HlsCatalogChildProofError::StaleGeneration,
        ChildProofFailure::Open(HlsVodOpenError::Parse(_))
        | ChildProofFailure::Open(HlsVodOpenError::Profile(_))
        | ChildProofFailure::InvalidManifest => HlsCatalogChildProofError::Rejected(
            HlsCatalogSiblingRejectionReason::InvalidChildManifest,
        ),
        ChildProofFailure::UnsupportedContainer => HlsCatalogChildProofError::Rejected(
            HlsCatalogSiblingRejectionReason::UnsupportedContainer,
        ),
        ChildProofFailure::UnsupportedTrackShape => HlsCatalogChildProofError::Rejected(
            HlsCatalogSiblingRejectionReason::UnsupportedTrackShape,
        ),
        ChildProofFailure::CapabilityRejected => HlsCatalogChildProofError::Rejected(
            HlsCatalogSiblingRejectionReason::CapabilityRejected,
        ),
        ChildProofFailure::Open(_) => HlsCatalogChildProofError::Rejected(
            HlsCatalogSiblingRejectionReason::TransportUnavailable,
        ),
    }
}
