//! Direct static MPD discovery поверх существующего lane proof/catalog runtime.

use super::*;
use crate::catalog::DashRepresentationLaneProviderDefault;
use crate::open::parse_fetched_dash_manifest;
use crate::request::DashFetchedManifestInput;
use web_media_core::PreferredHeightPolicy;

/// Direct-MPD discovery request без extractor-owned default Representation evidence.
pub struct NativeDashVodCatalogDiscoveryRequest<'capabilities> {
    /// Один adaptive context обслуживает root и выбранные Representation resources.
    pub http: Box<AdaptiveHttpContext>,
    /// Exact generation root fetch/open attempt-а.
    pub generation: SourceGeneration,
    /// Уже fetched authoritative MPD; повторный root GET типом не представим.
    pub manifest: DashFetchedManifestInput,
    /// Existing injected fMP4/WebM demux registry.
    pub demux_registry: Arc<DemuxRegistry>,
    /// Existing bounded VOD open/seek policy.
    pub policy: DashVodOpenPolicy,
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

/// Парсит fetched static MPD, доказывает lanes и выбирает exact native default.
pub fn discover_native_dash_vod_catalog(
    request: NativeDashVodCatalogDiscoveryRequest<'_>,
) -> Result<DashDiscoveredVodCatalog, DashVodCatalogDiscoveryError> {
    let NativeDashVodCatalogDiscoveryRequest {
        http,
        generation,
        manifest,
        demux_registry,
        policy,
        catalog_identity,
        catalog_limit,
        compatibility_edge_limit,
        capability_probe,
        preferred_height,
    } = request;
    let (mpd, manifest_base) = parse_fetched_dash_manifest(&http, generation, &manifest, policy)?;
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
            provider_default: DashRepresentationLaneProviderDefault::NativePreferredHeight(
                preferred_height,
            ),
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
