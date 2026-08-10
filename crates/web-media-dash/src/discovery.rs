//! Provider-owned DASH VOD catalog discovery и selected-lane open.

use std::fmt;
use std::sync::Arc;

use dash_mpd_core::{
    DashDynamicMpd, DashMediaKind, DashMpd, DashMpdParseRequest, parse_dynamic_dash_mpd,
};
use demux_api::DemuxRegistry;
use media_core::{Demuxer, TrackInfo, TrackKind};
use source_core::HttpRequestTarget;
use thiserror::Error;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_core::{
    AudioTrackDescriptor, ChannelCount, CodecFamily, CodecKind, ComponentVariantCatalog,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit, ComponentVariantEdgeLimit,
    ComponentVariantError, ComponentVariantSelection, ComponentVariantSemanticSelectionRequest,
    SampleRate, VideoHeight, VideoTrackDescriptor, VideoWidth,
};
use web_media_transport_api::SourceGeneration;

use crate::catalog::{
    DashLogicalRepresentationSelection, DashRepresentationLaneCatalog,
    DashRepresentationLaneCatalogBuildError, DashRepresentationLaneCatalogBuildRequest,
    DashRepresentationLaneProbe, DashRepresentationLaneProbeError, DashRepresentationLaneProof,
    DashRepresentationLaneProofPort, DashRepresentationLaneRejection,
    DashRepresentationLaneSelectionError, DashRepresentationLaneTimelineMode, LaneContract,
    audio_descriptor, build_dash_representation_lane_catalog, dynamic_range, normalized_codec,
    video_descriptor,
};
use crate::component::{DashComponentFactory, DashComponentTrackShapeError};
use crate::live::{
    DashClockFetchObservation, DashLiveOpenError, DashLiveOpenRequest, DashLiveOpenResult,
    DashLiveRefreshError, build_dash_live_snapshot, prepare_dash_live_logical,
    resolve_dash_live_clock,
};
use crate::open::{
    DashVodOpenError, DashVodOpenResult, fetch_dash_manifest, prepare_planned_manifest_vod,
};
use crate::plan::{
    DashComponentPlan, DashPlanError, DashPresentationPlan,
    build_manifest_plan_from_logical_selection,
};
use crate::request::{DashVodHttpContext, DashVodInput, DashVodOpenPolicy, DashVodOpenRequest};

/// Existing-composition capability check over exact demux tracks.
pub trait DashRepresentationCapabilityProbe: Send + Sync {
    /// Проверяет video-only lane.
    fn check_video(&self, video: &TrackInfo) -> Result<(), DashRepresentationCapabilityRejection>;

    /// Проверяет audio-only lane.
    fn check_audio(&self, audio: &TrackInfo) -> Result<(), DashRepresentationCapabilityRejection>;

    /// Проверяет coupled muxed lane целиком.
    fn check_muxed(
        &self,
        video: &TrackInfo,
        audio: &TrackInfo,
    ) -> Result<(), DashRepresentationCapabilityRejection>;
}

/// Safe capability rejection без backend или track payload в diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashRepresentationCapabilityRejection;

/// Полный provider-owned static discovery request.
pub struct DashVodCatalogDiscoveryRequest<'capabilities> {
    /// Existing fast-open request; discovery принимает только manifest-backed VOD.
    pub open: DashVodOpenRequest,
    /// Parent identity и caller-owned catalog generation.
    pub catalog_identity: ComponentVariantCatalogIdentity,
    /// Additive row budget.
    pub catalog_limit: ComponentVariantCatalogLimit,
    /// Sparse A/V compatibility budget.
    pub compatibility_edge_limit: ComponentVariantEdgeLimit,
    /// Immutable capability intersection over probed tracks.
    pub capability_probe: &'capabilities dyn DashRepresentationCapabilityProbe,
}

/// Discovered neutral catalog с private MPD/HTTP/lane mapping для exact open.
pub struct DashDiscoveredVodCatalog {
    lanes: DashRepresentationLaneCatalog,
    mpd: DashMpd,
    manifest_base: HttpRequestTarget,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    demux_registry: Arc<DemuxRegistry>,
    policy: DashVodOpenPolicy,
}

/// Полный provider-owned dynamic discovery request.
pub struct DashLiveCatalogDiscoveryRequest<'capabilities> {
    /// Existing fast live-open request; discovery не меняет default selection path.
    pub open: DashLiveOpenRequest,
    /// Parent identity и caller-owned catalog generation.
    pub catalog_identity: ComponentVariantCatalogIdentity,
    /// Additive row budget.
    pub catalog_limit: ComponentVariantCatalogLimit,
    /// Sparse A/V compatibility budget.
    pub compatibility_edge_limit: ComponentVariantEdgeLimit,
    /// Immutable capability intersection over probed tracks.
    pub capability_probe: &'capabilities dyn DashRepresentationCapabilityProbe,
}

/// Fresh dynamic catalog и private logical-selector runtime request.
pub struct DashDiscoveredLiveCatalog {
    lanes: DashRepresentationLaneCatalog,
    open: DashLiveOpenRequest,
    _mpd: DashDynamicMpd,
    _manifest_base: HttpRequestTarget,
}

impl fmt::Debug for DashDiscoveredLiveCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashDiscoveredLiveCatalog")
            .field("catalog_identity", self.lanes.catalog().identity())
            .field(
                "published_rows",
                &self.lanes.catalog().stored_variant_count(),
            )
            .field("rejected_rows", &self.lanes.rejections().len())
            .finish_non_exhaustive()
    }
}

impl DashDiscoveredLiveCatalog {
    /// Provider-neutral catalog без MPD/Representation/URL state.
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        self.lanes.catalog()
    }

    /// Exact provider default внутри текущего catalog generation.
    pub const fn provider_default(&self) -> &ComponentVariantSelection {
        self.lanes.provider_default()
    }

    /// Safe isolated sibling diagnostics.
    pub const fn rejections(&self) -> &[DashRepresentationLaneRejection] {
        self.lanes.rejections()
    }
}

impl fmt::Debug for DashDiscoveredVodCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashDiscoveredVodCatalog")
            .field("catalog_identity", self.lanes.catalog().identity())
            .field(
                "published_rows",
                &self.lanes.catalog().stored_variant_count(),
            )
            .field("rejected_rows", &self.lanes.rejections().len())
            .finish_non_exhaustive()
    }
}

impl DashDiscoveredVodCatalog {
    /// Provider-neutral catalog без MPD/Representation/URL state.
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        self.lanes.catalog()
    }

    /// Exact provider default внутри текущего catalog generation.
    pub const fn provider_default(&self) -> &ComponentVariantSelection {
        self.lanes.provider_default()
    }

    /// Safe isolated sibling diagnostics.
    pub const fn rejections(&self) -> &[DashRepresentationLaneRejection] {
        self.lanes.rejections()
    }
}

/// Authoritative discovery failure; sibling failures остаются в catalog diagnostics.
#[derive(Debug, Error)]
pub enum DashVodCatalogDiscoveryError {
    /// Serialized fragment input не имеет discoverable MPD sibling topology.
    #[error("DASH catalog discovery requires manifest-backed VOD input")]
    ManifestRequired,
    /// Authoritative manifest transport/parsing failed.
    #[error("DASH catalog authoritative open failed: {0}")]
    Open(#[from] DashVodOpenError),
    /// Atomic lane catalog build failed.
    #[error("DASH representation catalog construction failed: {0}")]
    Catalog(#[from] DashRepresentationLaneCatalogBuildError),
}

/// Authoritative dynamic discovery failure; sibling failures остаются diagnostics.
#[derive(Debug, Error)]
pub enum DashLiveCatalogDiscoveryError {
    /// Initial dynamic fetch/schema/clock/availability failed.
    #[error("DASH live catalog authoritative open failed: {0}")]
    Open(#[from] DashLiveOpenError),
    /// Atomic logical lane catalog build failed.
    #[error("DASH live representation catalog construction failed: {0}")]
    Catalog(#[from] DashRepresentationLaneCatalogBuildError),
}

/// Selected discovered-lane preparation failure без provider-default fallback.
#[derive(Debug, Error)]
pub enum DashDiscoveredVodOpenError {
    /// Semantic request отсутствует или неоднозначен в fresh catalog.
    #[error("DASH semantic selection rematch failed: {0}")]
    Semantic(#[from] ComponentVariantError),
    /// Exact neutral row не имеет private provider mapping.
    #[error("DASH exact discovered selection failed: {0}")]
    Selection(#[from] DashRepresentationLaneSelectionError),
    /// Retained exact lane больше не образует valid static plan.
    #[error("DASH selected representation planning failed: {0}")]
    Plan(#[from] DashPlanError),
    /// Selected runtime preparation failed.
    #[error("DASH selected representation open failed: {0}")]
    Open(#[from] DashVodOpenError),
}

/// Selected discovered live-lane preparation failure без fallback.
#[derive(Debug, Error)]
pub enum DashDiscoveredLiveOpenError {
    /// Semantic request отсутствует или неоднозначен в fresh catalog.
    #[error("DASH live semantic selection rematch failed: {0}")]
    Semantic(#[from] ComponentVariantError),
    /// Exact neutral row не имеет private provider mapping.
    #[error("DASH live exact discovered selection failed: {0}")]
    Selection(#[from] DashRepresentationLaneSelectionError),
    /// Selected dynamic runtime preparation failed.
    #[error("DASH live selected representation open failed: {0}")]
    Open(#[from] DashLiveOpenError),
}

/// Fetch-ит MPD один раз, пробует все logical lanes и сохраняет private open mapping.
pub fn discover_dash_vod_catalog(
    request: DashVodCatalogDiscoveryRequest<'_>,
) -> Result<DashDiscoveredVodCatalog, DashVodCatalogDiscoveryError> {
    let DashVodCatalogDiscoveryRequest {
        open,
        catalog_identity,
        catalog_limit,
        compatibility_edge_limit,
        capability_probe,
    } = request;
    let DashVodOpenRequest {
        http,
        generation,
        input,
        selection,
        demux_registry,
        policy,
    } = open;
    let (DashVodHttpContext::Manifest(http), DashVodInput::Manifest(manifest)) = (http, input)
    else {
        return Err(DashVodCatalogDiscoveryError::ManifestRequired);
    };
    let (mpd, manifest_base) = fetch_dash_manifest(&http, generation, manifest, policy)?;
    let parent_semantic = catalog_identity.parent().semantic().clone();
    let mut proof = ProviderLaneProof {
        presentation: &mpd,
        manifest_base: &manifest_base,
        http: &http,
        generation,
        demux_registry: &demux_registry,
        policy,
        capability_probe,
        timeline_mode: DashRepresentationLaneTimelineMode::Static,
    };
    let lanes = build_dash_representation_lane_catalog(
        DashRepresentationLaneCatalogBuildRequest {
            presentation: &mpd,
            manifest_base: &manifest_base,
            catalog_identity,
            parent_semantic: &parent_semantic,
            provider_default: &selection,
            catalog_limit,
            compatibility_edge_limit,
            maximum_planned_segments: policy.maximum_planned_segments,
            timeline_mode: DashRepresentationLaneTimelineMode::Static,
        },
        &mut proof,
    )?;
    Ok(DashDiscoveredVodCatalog {
        lanes,
        mpd,
        manifest_base,
        http: *http,
        generation,
        demux_registry,
        policy,
    })
}

/// Fetch-ит fresh dynamic snapshot, пробует siblings и сохраняет logical refresh selector.
pub fn discover_dash_live_catalog(
    request: DashLiveCatalogDiscoveryRequest<'_>,
) -> Result<DashDiscoveredLiveCatalog, DashLiveCatalogDiscoveryError> {
    let DashLiveCatalogDiscoveryRequest {
        open,
        catalog_identity,
        catalog_limit,
        compatibility_edge_limit,
        capability_probe,
    } = request;
    let local_before_fetch = open.wall_clock.now_utc();
    let fetched = open
        .http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            open.generation,
            open.manifest.target.clone(),
            open.policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))
        .map_err(DashLiveOpenError::from)?;
    let local_after_fetch = open.wall_clock.now_utc();
    let mpd = parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: fetched.bytes(),
        xml_budgets: open.manifest.xml_budgets,
        limits: open.manifest.mpd_limits,
    })
    .map_err(DashLiveOpenError::from)?;
    let manifest_base = fetched.final_target().clone();
    let clock = resolve_dash_live_clock(
        &mpd.utc_timing,
        &manifest_base,
        &open.http,
        open.generation,
        Arc::clone(&open.wall_clock),
        DashClockFetchObservation {
            local_before_fetch,
            local_after_fetch,
        },
    )
    .map_err(DashLiveRefreshError::Clock)
    .map_err(DashLiveOpenError::from)?;
    build_dash_live_snapshot(
        mpd.clone(),
        &manifest_base,
        &open.selection,
        open.policy.maximum_planned_segments,
        &clock,
    )
    .map_err(DashLiveOpenError::from)?;

    let parent_semantic = catalog_identity.parent().semantic().clone();
    let mut proof = ProviderLaneProof {
        presentation: &mpd.presentation,
        manifest_base: &manifest_base,
        http: &open.http,
        generation: open.generation,
        demux_registry: &open.demux_registry,
        policy: open.policy,
        capability_probe,
        timeline_mode: DashRepresentationLaneTimelineMode::Dynamic,
    };
    let lanes = build_dash_representation_lane_catalog(
        DashRepresentationLaneCatalogBuildRequest {
            presentation: &mpd.presentation,
            manifest_base: &manifest_base,
            catalog_identity,
            parent_semantic: &parent_semantic,
            provider_default: &open.selection,
            catalog_limit,
            compatibility_edge_limit,
            maximum_planned_segments: open.policy.maximum_planned_segments,
            timeline_mode: DashRepresentationLaneTimelineMode::Dynamic,
        },
        &mut proof,
    )?;
    Ok(DashDiscoveredLiveCatalog {
        lanes,
        open,
        _mpd: mpd,
        _manifest_base: manifest_base,
    })
}

/// Открывает exact selection только через retained private mapping текущего catalog-а.
pub fn prepare_discovered_dash_vod(
    discovered: DashDiscoveredVodCatalog,
    selection: ComponentVariantSelection,
) -> Result<DashVodOpenResult, DashDiscoveredVodOpenError> {
    let logical = discovered.lanes.resolve_selection(&selection)?;
    prepare_discovered_logical(discovered, logical)
}

/// Fail-closed rematch-ит semantic selection и открывает найденную exact lane.
pub fn prepare_discovered_dash_vod_semantic(
    discovered: DashDiscoveredVodCatalog,
    request: ComponentVariantSemanticSelectionRequest,
) -> Result<DashVodOpenResult, DashDiscoveredVodOpenError> {
    let selection = discovered.lanes.catalog().rematch_semantic(request)?;
    prepare_discovered_dash_vod(discovered, selection)
}

/// Открывает exact live selection и переносит logical contract во все refresh-и.
pub fn prepare_discovered_dash_live(
    discovered: DashDiscoveredLiveCatalog,
    selection: ComponentVariantSelection,
) -> Result<DashLiveOpenResult, DashDiscoveredLiveOpenError> {
    let logical = discovered.lanes.resolve_selection(&selection)?;
    prepare_dash_live_logical(discovered.open, logical).map_err(Into::into)
}

/// Fail-closed rematch-ит semantic live selection перед logical runtime open.
pub fn prepare_discovered_dash_live_semantic(
    discovered: DashDiscoveredLiveCatalog,
    request: ComponentVariantSemanticSelectionRequest,
) -> Result<DashLiveOpenResult, DashDiscoveredLiveOpenError> {
    let selection = discovered.lanes.catalog().rematch_semantic(request)?;
    prepare_discovered_dash_live(discovered, selection)
}

fn prepare_discovered_logical(
    discovered: DashDiscoveredVodCatalog,
    logical: DashLogicalRepresentationSelection,
) -> Result<DashVodOpenResult, DashDiscoveredVodOpenError> {
    let plan = build_manifest_plan_from_logical_selection(
        &discovered.mpd,
        &discovered.manifest_base,
        &logical,
        discovered.policy.maximum_planned_segments,
        DashRepresentationLaneTimelineMode::Static,
    )?;
    prepare_planned_manifest_vod(
        plan,
        discovered.http,
        discovered.generation,
        discovered.demux_registry,
        discovered.policy,
    )
    .map_err(Into::into)
}

struct ProviderLaneProof<'proof> {
    presentation: &'proof DashMpd,
    manifest_base: &'proof HttpRequestTarget,
    http: &'proof AdaptiveHttpContext,
    generation: SourceGeneration,
    demux_registry: &'proof Arc<DemuxRegistry>,
    policy: DashVodOpenPolicy,
    capability_probe: &'proof dyn DashRepresentationCapabilityProbe,
    timeline_mode: DashRepresentationLaneTimelineMode,
}

impl DashRepresentationLaneProofPort for ProviderLaneProof<'_> {
    fn prove_lane(
        &mut self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
        let logical = DashLogicalRepresentationSelection::Single(request.logical_lane);
        let DashPresentationPlan::Single(component_plan) =
            build_manifest_plan_from_logical_selection(
                self.presentation,
                self.manifest_base,
                &logical,
                self.policy.maximum_planned_segments,
                self.timeline_mode,
            )
            .map_err(|_| DashRepresentationLaneProbeError::UnsupportedContainer)?
        else {
            return Err(DashRepresentationLaneProbeError::UnsupportedTrackShape);
        };
        let mut proof = None;
        for period in component_plan.periods.iter().cloned() {
            let period_plan = DashComponentPlan {
                media_kind: component_plan.media_kind,
                periods: vec![period],
                duration: component_plan.duration,
            };
            let factory = DashComponentFactory::new(
                period_plan,
                self.http.clone(),
                self.generation,
                self.policy,
                Arc::clone(self.demux_registry),
            );
            let component = factory
                .open()
                .map_err(|error| map_component_probe_error(&error))?;
            let period_proof =
                prove_tracks(component.tracks(), &request.contract, self.capability_probe)?;
            if proof.as_ref().is_some_and(|proof| proof != &period_proof) {
                return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
            }
            proof = Some(period_proof);
        }
        proof.ok_or(DashRepresentationLaneProbeError::UnsupportedTrackShape)
    }
}

fn map_component_probe_error(error: &anyhow::Error) -> DashRepresentationLaneProbeError {
    if let Some(transport) = error.downcast_ref::<AdaptiveTransportError>() {
        return match transport {
            AdaptiveTransportError::Cancelled => DashRepresentationLaneProbeError::Cancelled,
            AdaptiveTransportError::StaleGeneration { .. } => {
                DashRepresentationLaneProbeError::StaleGeneration
            }
            _ => DashRepresentationLaneProbeError::TransportUnavailable,
        };
    }
    if error
        .downcast_ref::<DashComponentTrackShapeError>()
        .is_some()
    {
        return DashRepresentationLaneProbeError::UnsupportedTrackShape;
    }
    DashRepresentationLaneProbeError::UnsupportedContainer
}

fn prove_tracks(
    tracks: &[TrackInfo],
    contract: &LaneContract,
    capability_probe: &dyn DashRepresentationCapabilityProbe,
) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError> {
    let video = exact_track(tracks, TrackKind::Video);
    let audio = exact_track(tracks, TrackKind::Audio);
    match (contract.kind, video, audio, tracks.len()) {
        (DashMediaKind::Video, Some(video), None, 1) => {
            validate_video_track(video, contract)?;
            capability_probe
                .check_video(video)
                .map_err(|_| DashRepresentationLaneProbeError::CapabilityRejected)?;
            Ok(DashRepresentationLaneProof::VideoOnly(
                proven_video_descriptor(video, contract)?,
            ))
        }
        (DashMediaKind::Audio, None, Some(audio), 1) => {
            validate_audio_track(audio, contract)?;
            capability_probe
                .check_audio(audio)
                .map_err(|_| DashRepresentationLaneProbeError::CapabilityRejected)?;
            Ok(DashRepresentationLaneProof::AudioOnly(
                proven_audio_descriptor(audio, contract)?,
            ))
        }
        (DashMediaKind::Muxed, Some(video), Some(audio), 2) => {
            validate_video_track(video, contract)?;
            validate_audio_track(audio, contract)?;
            capability_probe
                .check_muxed(video, audio)
                .map_err(|_| DashRepresentationLaneProbeError::CapabilityRejected)?;
            Ok(DashRepresentationLaneProof::Muxed {
                video: proven_video_descriptor(video, contract)?,
                audio: proven_audio_descriptor(audio, contract)?,
            })
        }
        _ => Err(DashRepresentationLaneProbeError::UnsupportedTrackShape),
    }
}

fn exact_track(tracks: &[TrackInfo], kind: TrackKind) -> Option<&TrackInfo> {
    let mut matches = tracks.iter().filter(|track| track.kind == kind);
    let track = matches.next()?;
    matches.next().is_none().then_some(track)
}

fn validate_video_track(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<(), DashRepresentationLaneProbeError> {
    let codec = normalized_codec(
        contract
            .video_codec
            .as_deref()
            .ok_or(DashRepresentationLaneProbeError::ManifestEvidenceConflict)?,
    )
    .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    if !video_codec_matches(codec.kind(), &track.codec_id) {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    let probed_width = track.video.as_ref().and_then(|video| video.coded_width);
    let probed_height = track.video.as_ref().and_then(|video| video.coded_height);
    if contract
        .width
        .zip(probed_width)
        .is_some_and(|(advertised, probed)| advertised != probed)
        || contract
            .height
            .zip(probed_height)
            .is_some_and(|(advertised, probed)| advertised != probed)
    {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    if dynamic_range(contract.color) == web_media_core::DynamicRange::Sdr
        && track
            .video
            .as_ref()
            .and_then(|video| video.color.as_ref())
            .is_some_and(|color| color.requires_hdr_processing())
    {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    Ok(())
}

fn validate_audio_track(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<(), DashRepresentationLaneProbeError> {
    let codec = normalized_codec(
        contract
            .audio_codec
            .as_deref()
            .ok_or(DashRepresentationLaneProbeError::ManifestEvidenceConflict)?,
    )
    .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    if !audio_codec_matches(codec.kind(), &track.codec_id)
        || contract
            .audio_sampling_rate
            .zip(track.sample_rate)
            .is_some_and(|(advertised, probed)| advertised != probed)
        || super::catalog::channel_count(contract.audio_channel_configuration)
            .map(u32::from)
            .zip(track.channels)
            .is_some_and(|(advertised, probed)| advertised != probed)
    {
        return Err(DashRepresentationLaneProbeError::ManifestEvidenceConflict);
    }
    Ok(())
}

fn proven_video_descriptor(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<VideoTrackDescriptor, DashRepresentationLaneProbeError> {
    let expected = video_descriptor(contract)
        .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    let probed = track.video.as_ref();
    let width = probed
        .and_then(|video| video.coded_width)
        .or(expected.width_pixels())
        .map(VideoWidth::new)
        .transpose()
        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)?;
    let height = probed
        .and_then(|video| video.coded_height)
        .map(VideoHeight::new)
        .transpose()
        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)?
        .or(expected.height());
    let dynamic_range = if probed
        .and_then(|video| video.color.as_ref())
        .is_some_and(|color| color.requires_hdr_processing())
    {
        web_media_core::DynamicRange::Hdr
    } else {
        expected.dynamic_range()
    };
    Ok(VideoTrackDescriptor::new(
        expected.codec().clone(),
        width,
        height,
        expected.frame_rate(),
        expected.bitrate(),
        dynamic_range,
    ))
}

fn proven_audio_descriptor(
    track: &TrackInfo,
    contract: &LaneContract,
) -> Result<AudioTrackDescriptor, DashRepresentationLaneProbeError> {
    let expected = audio_descriptor(contract)
        .map_err(|_| DashRepresentationLaneProbeError::ManifestEvidenceConflict)?;
    let sample_rate = track
        .sample_rate
        .map(SampleRate::new)
        .transpose()
        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)?
        .or(expected.sample_rate());
    let channels = track
        .channels
        .map(|channels| {
            u16::try_from(channels)
                .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)
                .and_then(|channels| {
                    ChannelCount::new(channels)
                        .map_err(|_| DashRepresentationLaneProbeError::UnsupportedTrackShape)
                })
        })
        .transpose()?
        .or(expected.channels());
    Ok(AudioTrackDescriptor::new(
        expected.codec().clone(),
        sample_rate,
        channels,
        expected.bitrate(),
        expected.language().cloned(),
    ))
}

fn video_codec_matches(kind: CodecKind, codec_id: &str) -> bool {
    let normalized = codec_id.trim().to_ascii_uppercase();
    matches!(
        (kind, normalized.as_str()),
        (CodecKind::Known(CodecFamily::Vp8), "V_VP8" | "VP8")
            | (CodecKind::Known(CodecFamily::Vp9), "V_VP9" | "VP9")
            | (CodecKind::Known(CodecFamily::Av1), "V_AV1" | "AV1" | "AV01")
            | (
                CodecKind::Known(CodecFamily::H264),
                "V_MPEG4/ISO/AVC" | "AVC1" | "H264" | "H.264"
            )
            | (
                CodecKind::Known(CodecFamily::H265),
                "V_MPEGH/ISO/HEVC" | "HEV1" | "HVC1" | "H265" | "H.265"
            )
    )
}

fn audio_codec_matches(kind: CodecKind, codec_id: &str) -> bool {
    let normalized = codec_id.trim().to_ascii_uppercase();
    matches!(
        (kind, normalized.as_str()),
        (CodecKind::Known(CodecFamily::Opus), "A_OPUS" | "OPUS")
            | (CodecKind::Known(CodecFamily::Vorbis), "A_VORBIS" | "VORBIS")
            | (
                CodecKind::Known(CodecFamily::Aac),
                "A_AAC" | "A_AAC/MPEG2/LC" | "A_AAC/MPEG4/LC" | "AAC"
            )
    )
}
