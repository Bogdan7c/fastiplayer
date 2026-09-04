//! Provider-owned DASH VOD catalog discovery и selected-lane open.

mod lane_proof;
mod native_live;
mod native_vod;

pub use native_live::{NativeDashLiveCatalogDiscoveryRequest, discover_native_dash_live_catalog};
pub use native_vod::{NativeDashVodCatalogDiscoveryRequest, discover_native_dash_vod_catalog};

use std::fmt;
use std::sync::Arc;

use dash_mpd_core::{DashDynamicMpd, DashMpd, DashMpdParseRequest, parse_dynamic_dash_mpd};
use demux_api::DemuxRegistry;
use media_core::TrackInfo;
use source_core::HttpRequestTarget;
use thiserror::Error;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication,
};
use web_media_core::{
    ComponentVariantCatalog, ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit,
    ComponentVariantEdgeLimit, ComponentVariantError, ComponentVariantSelection,
    ComponentVariantSemanticSelectionRequest,
};
use web_media_transport_api::SourceGeneration;

use crate::catalog::{
    DashLogicalRepresentationSelection, DashRepresentationLaneCatalog,
    DashRepresentationLaneCatalogBuildError, DashRepresentationLaneCatalogBuildRequest,
    DashRepresentationLaneProviderDefault, DashRepresentationLaneRejection,
    DashRepresentationLaneSelectionError, DashRepresentationLaneTimelineMode,
    build_dash_representation_lane_catalog,
};
use crate::live::{
    DashClockFetchObservation, DashLiveOpenError, DashLiveOpenRequest, DashLiveOpenResult,
    DashLiveRefreshError, DashLiveRuntimeOpenRequest, build_dash_live_snapshot,
    prepare_dash_live_logical, resolve_dash_live_clock,
};
use crate::open::{
    DashVodOpenError, DashVodOpenResult, fetch_dash_manifest, parse_fetched_dash_manifest,
    prepare_planned_manifest_vod,
};
use crate::plan::{DashPlanError, build_manifest_plan_from_logical_selection};
use crate::request::{DashVodHttpContext, DashVodInput, DashVodOpenPolicy, DashVodOpenRequest};

use lane_proof::{ProviderLaneProof, ProviderLaneProofContext};

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
    open: DashLiveRuntimeOpenRequest,
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
    let DashVodHttpContext::Manifest(http) = http else {
        return Err(DashVodCatalogDiscoveryError::ManifestRequired);
    };
    let (mpd, manifest_base) = match input {
        DashVodInput::Manifest(manifest) => {
            fetch_dash_manifest(&http, generation, manifest, policy)?
        }
        DashVodInput::FetchedManifest(manifest) => {
            parse_fetched_dash_manifest(&http, generation, &manifest, policy)?
        }
        DashVodInput::Serialized(_) => {
            return Err(DashVodCatalogDiscoveryError::ManifestRequired);
        }
    };
    let parent_semantic = catalog_identity.parent().semantic().clone();
    let mut proof = ProviderLaneProof::new(ProviderLaneProofContext {
        presentation: &mpd,
        manifest_base: &manifest_base,
        http: &http,
        generation,
        demux_registry: &demux_registry,
        policy,
        capability_probe,
        timeline_mode: DashRepresentationLaneTimelineMode::Static,
    });
    let lanes = build_dash_representation_lane_catalog(
        DashRepresentationLaneCatalogBuildRequest {
            presentation: &mpd,
            manifest_base: &manifest_base,
            catalog_identity,
            parent_semantic: &parent_semantic,
            provider_default: DashRepresentationLaneProviderDefault::ExactEvidence(&selection),
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
    let mut proof = ProviderLaneProof::new(ProviderLaneProofContext {
        presentation: &mpd.presentation,
        manifest_base: &manifest_base,
        http: &open.http,
        generation: open.generation,
        demux_registry: &open.demux_registry,
        policy: open.policy,
        capability_probe,
        timeline_mode: DashRepresentationLaneTimelineMode::Dynamic,
    });
    let lanes = build_dash_representation_lane_catalog(
        DashRepresentationLaneCatalogBuildRequest {
            presentation: &mpd.presentation,
            manifest_base: &manifest_base,
            catalog_identity,
            parent_semantic: &parent_semantic,
            provider_default: DashRepresentationLaneProviderDefault::ExactEvidence(&open.selection),
            catalog_limit,
            compatibility_edge_limit,
            maximum_planned_segments: open.policy.maximum_planned_segments,
            timeline_mode: DashRepresentationLaneTimelineMode::Dynamic,
        },
        &mut proof,
    )?;
    Ok(DashDiscoveredLiveCatalog {
        lanes,
        open: open.into(),
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
