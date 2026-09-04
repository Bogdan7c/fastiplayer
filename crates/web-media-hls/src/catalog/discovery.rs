use std::time::Duration;

use hls_playlist_core::{
    HlsParseRequest, HlsParserLimits, HlsPlaylist, MediaContainerIntent, MediaPlaylist,
    parse_hls_playlist, validate_initial_profile, validate_live_profile, validate_vod_profile,
};
use media_core::{TrackInfo, TrackKind};
use source_core::HttpRequestTarget;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_transport_api::SourceGeneration;

use super::*;
use crate::open::{
    fetch_manifest, load_top_playlist, open_epoch_probe, parse_playlist, select_master,
    select_master_at_index,
};
use crate::plan::{
    HlsComponentPlan, build_component_plan, build_segment_scoped_component_plan, parse_hls_duration,
};
use crate::{HlsRequiredContainer, HlsVodOpenRequest};

#[derive(Clone, PartialEq, Eq)]
struct TimelineSignature {
    /// Суммарный presentation interval остаётся обязательным cross-rendition доказательством.
    total_duration: Duration,
    /// Разрывы должны находиться на одинаковых границах сегментов у всех rendition.
    discontinuity_offsets: Box<[u64]>,
}

struct DiscoveryProofPort<'request, 'capability> {
    request: &'request HlsVodOpenRequest,
    base: HttpRequestTarget,
    presentation: HlsCatalogPresentation,
    capability: &'capability mut dyn HlsCatalogCapabilityProofPort,
    alignments: Vec<TimelineSignature>,
}

/// Определяет VOD/live presentation по selected media child, когда top manifest является master-ом.
///
/// Root повторно не загружается: `FetchedTop` остаётся authoritative handoff-ом. Child probe
/// намеренно не становится durable identity и повторяется внутри полного capability catalog-а.
pub fn detect_hls_catalog_presentation(
    fetched_top: &web_media_adaptive::AdaptiveFetchedResource,
    http: &AdaptiveHttpContext,
    generation: SourceGeneration,
    selection: &crate::HlsVariantSelectionIntent,
    provider_default_variant_index: Option<usize>,
    parser_limits: HlsParserLimits,
) -> Result<HlsCatalogPresentation, HlsCatalogDiscoveryError> {
    let playlist = parse_hls_playlist(HlsParseRequest {
        document_bytes: fetched_top.bytes(),
        reference_base: Some(fetched_top.final_target().expose_secret_for_request()),
        limits: parser_limits,
    })
    .map_err(HlsVodOpenError::Parse)?;
    validate_initial_profile(&playlist).map_err(HlsVodOpenError::Profile)?;
    let HlsPlaylist::Master(master) = playlist else {
        return Err(HlsVodOpenError::MissingVariant.into());
    };
    let selected = match provider_default_variant_index {
        Some(variant_index) => select_master_at_index(&master, variant_index, selection),
        None => select_master(&master, selection),
    }?;
    let child_target = fetched_top
        .final_target()
        .resolve_reference(selected.variant.uri.expose_for_resolution())
        .map_err(HlsVodOpenError::from)?;
    let child_resource = http
        .fetch_resource_blocking(
            AdaptiveResourceFetchRequest::full(
                generation,
                child_target.clone(),
                http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
                AdaptiveResourcePurpose::Manifest,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            )
            .with_secret_forwarding(http.resource_secret_forwarding_for(&child_target)),
        )
        .map_err(HlsVodOpenError::from)?;
    let child_playlist = parse_hls_playlist(HlsParseRequest {
        document_bytes: child_resource.bytes(),
        reference_base: Some(child_resource.final_target().expose_secret_for_request()),
        limits: parser_limits,
    })
    .map_err(HlsVodOpenError::Parse)?;
    let HlsPlaylist::Media(media) = child_playlist else {
        return Err(HlsVodOpenError::NestedMasterPlaylist.into());
    };
    let presentation = if media.end_list {
        HlsCatalogPresentation::Vod
    } else {
        HlsCatalogPresentation::Live
    };
    let media_playlist = HlsPlaylist::Media(media);
    match presentation {
        HlsCatalogPresentation::Vod => {
            validate_vod_profile(&media_playlist, None).map_err(HlsVodOpenError::Profile)?;
        }
        HlsCatalogPresentation::Live => {
            validate_live_profile(&media_playlist, None).map_err(HlsVodOpenError::Profile)?;
        }
    }
    Ok(presentation)
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
            provider_default_variant_index: request.provider_default_variant_index,
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
        let opened = open_epoch_probe(self.request, first_epoch)
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
        let mut total_duration = Duration::ZERO;
        let mut discontinuity_offsets = Vec::with_capacity(media.segments.len());
        for segment in &media.segments {
            let segment_duration = parse_hls_duration(&segment.duration)
                .map_err(|_| ChildProofFailure::InvalidManifest)?;
            total_duration = total_duration
                .checked_add(segment_duration)
                .ok_or(ChildProofFailure::InvalidManifest)?;
            discontinuity_offsets.push(
                segment
                    .discontinuity_sequence
                    .checked_sub(base_discontinuity)
                    .ok_or(ChildProofFailure::InvalidManifest)?,
            );
        }
        let signature = TimelineSignature {
            total_duration,
            discontinuity_offsets: discontinuity_offsets.into_boxed_slice(),
        };
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
