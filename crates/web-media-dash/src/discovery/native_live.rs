//! Direct dynamic MPD discovery поверх существующего S35 runtime.

use super::*;
use crate::catalog::DashRepresentationLaneProviderDefault;
use crate::live::{
    DashEndpointRefreshPort, DashFetchedLiveManifestInput, DashLiveInitialManifest,
    DashLiveRuntimeOpenRequest, DashWallClock,
};
use media_core::{DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration};
use web_media_core::PreferredHeightPolicy;

/// Direct dynamic discovery request без extractor-owned Representation evidence.
pub struct NativeDashLiveCatalogDiscoveryRequest<'capabilities> {
    /// Один adaptive context обслуживает initial root, clock и Representation resources.
    pub http: Box<AdaptiveHttpContext>,
    /// Exact generation root fetch/open attempt-а.
    pub generation: SourceGeneration,
    /// Уже fetched authoritative dynamic MPD с clock observation.
    pub manifest: DashFetchedLiveManifestInput,
    /// Existing injected fMP4/WebM demux registry.
    pub demux_registry: Arc<DemuxRegistry>,
    /// Existing bounded live open/seek policy.
    pub policy: DashVodOpenPolicy,
    /// Injected app wall clock.
    pub wall_clock: Arc<dyn DashWallClock>,
    /// Fresh neutral timeline generation.
    pub timeline_port_generation: DynamicMediaTimelinePortGeneration,
    /// Initial provider epoch текущей runtime generation.
    pub initial_source_epoch: DynamicMediaTimelineEpoch,
    /// App-owned native stable-root endpoint recovery.
    pub endpoint_refresh: Arc<dyn DashEndpointRefreshPort>,
    /// Fresh stable-lineage parent/catalog identity.
    pub catalog_identity: ComponentVariantCatalogIdentity,
    /// Additive catalog row bound.
    pub catalog_limit: ComponentVariantCatalogLimit,
    /// Proven compatibility-edge bound.
    pub compatibility_edge_limit: ComponentVariantEdgeLimit,
    /// Existing decoder/audio capability intersection.
    pub capability_probe: &'capabilities dyn DashRepresentationCapabilityProbe,
    /// Native default ranking применяется только после full capability proof.
    pub preferred_height: PreferredHeightPolicy,
}

/// Парсит один fetched dynamic MPD и передаёт тот же snapshot exact live open-у.
pub fn discover_native_dash_live_catalog(
    request: NativeDashLiveCatalogDiscoveryRequest<'_>,
) -> Result<DashDiscoveredLiveCatalog, DashLiveCatalogDiscoveryError> {
    if request.manifest.manifest().source_generation() != request.generation {
        return Err(DashLiveOpenError::FetchedManifestGenerationMismatch.into());
    }
    let (manifest_base, document_bytes, xml_budgets, mpd_limits) =
        request.manifest.manifest().parse_parts();
    if document_bytes.len() > request.policy.maximum_manifest_bytes.get() {
        return Err(DashLiveOpenError::FetchedManifestExceedsPolicy.into());
    }
    let mpd = parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes,
        xml_budgets,
        limits: mpd_limits,
    })
    .map_err(DashLiveOpenError::from)?;
    let manifest_base = manifest_base.clone();
    // Clock проверяется до catalog publication; direct sample использует exact root observation.
    resolve_dash_live_clock(
        &mpd.utc_timing,
        &manifest_base,
        &request.http,
        request.generation,
        Arc::clone(&request.wall_clock),
        request.manifest.observation(),
    )
    .map_err(DashLiveRefreshError::Clock)
    .map_err(DashLiveOpenError::from)?;

    let parent_semantic = request.catalog_identity.parent().semantic().clone();
    let mut proof = ProviderLaneProof::new(ProviderLaneProofContext {
        presentation: &mpd.presentation,
        manifest_base: &manifest_base,
        http: &request.http,
        generation: request.generation,
        demux_registry: &request.demux_registry,
        policy: request.policy,
        capability_probe: request.capability_probe,
        timeline_mode: DashRepresentationLaneTimelineMode::Dynamic,
    });
    let lanes = build_dash_representation_lane_catalog(
        DashRepresentationLaneCatalogBuildRequest {
            presentation: &mpd.presentation,
            manifest_base: &manifest_base,
            catalog_identity: request.catalog_identity,
            parent_semantic: &parent_semantic,
            provider_default: DashRepresentationLaneProviderDefault::NativePreferredHeight(
                request.preferred_height,
            ),
            catalog_limit: request.catalog_limit,
            compatibility_edge_limit: request.compatibility_edge_limit,
            maximum_planned_segments: request.policy.maximum_planned_segments,
            timeline_mode: DashRepresentationLaneTimelineMode::Dynamic,
        },
        &mut proof,
    )?;
    let open = DashLiveRuntimeOpenRequest {
        http: request.http,
        generation: request.generation,
        initial_manifest: DashLiveInitialManifest::Fetched(request.manifest),
        demux_registry: request.demux_registry,
        policy: request.policy,
        wall_clock: request.wall_clock,
        timeline_port_generation: request.timeline_port_generation,
        initial_source_epoch: request.initial_source_epoch,
        endpoint_refresh: request.endpoint_refresh,
    };
    Ok(DashDiscoveredLiveCatalog {
        lanes,
        open,
        _mpd: mpd,
        _manifest_base: manifest_base,
    })
}
