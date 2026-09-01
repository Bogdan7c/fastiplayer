//! Строгий static VOD и dynamic live/DVR DASH runtime поверх neutral boundaries.
//!
//! Crate владеет MPD/serialized planning, exact Representation selection,
//! multi-period lifecycle, transactional seek и S35 timing/refresh policy.
//! HTTP wire mechanics остаются в `source-core`, request policy — в
//! `web-media-adaptive`, container parsing — в injected existing demux factories.

#![forbid(unsafe_code)]

mod catalog;
mod component;
mod discovery;
// Dynamic timing/refresh policy не раздувает finite S34 modules.
mod live;
mod open;
mod plan;
mod request;
mod selection;
mod source;
#[cfg(test)]
mod tests;
mod transactional_av;

pub use catalog::{
    DashLogicalRepresentationLane, DashLogicalRepresentationSelection,
    DashRepresentationLaneCatalog, DashRepresentationLaneCatalogBuildError,
    DashRepresentationLaneCatalogBuildRequest, DashRepresentationLaneProbe,
    DashRepresentationLaneProbeError, DashRepresentationLaneProbeId, DashRepresentationLaneProof,
    DashRepresentationLaneProofPort, DashRepresentationLaneProviderDefault,
    DashRepresentationLaneRejection, DashRepresentationLaneRejectionReason,
    DashRepresentationLaneSelectionError, DashRepresentationLaneTimelineMode,
    build_dash_representation_lane_catalog,
};
pub use discovery::{
    DashDiscoveredLiveCatalog, DashDiscoveredLiveOpenError, DashDiscoveredVodCatalog,
    DashDiscoveredVodOpenError, DashLiveCatalogDiscoveryError, DashLiveCatalogDiscoveryRequest,
    DashRepresentationCapabilityProbe, DashRepresentationCapabilityRejection,
    DashVodCatalogDiscoveryError, DashVodCatalogDiscoveryRequest,
    NativeDashLiveCatalogDiscoveryRequest, NativeDashVodCatalogDiscoveryRequest,
    discover_dash_live_catalog, discover_dash_vod_catalog, discover_native_dash_live_catalog,
    discover_native_dash_vod_catalog, prepare_discovered_dash_live,
    prepare_discovered_dash_live_semantic, prepare_discovered_dash_vod,
    prepare_discovered_dash_vod_semantic,
};
pub use live::{
    DashClockFetchObservation, DashEndpointRefreshError, DashEndpointRefreshPort,
    DashEndpointRefreshReply, DashEndpointRefreshRequest, DashFetchedLiveManifestInput,
    DashLiveAvailability, DashLiveClockError, DashLiveOpenError, DashLiveOpenRequest,
    DashLiveOpenResult, DashLiveProfileExclusion, DashLiveRefreshError, DashLiveRefreshOutcome,
    DashLiveSnapshot, DashSynchronizedClock, DashWallClock, build_dash_live_snapshot,
    prepare_dash_live, refresh_dash_live_snapshot,
};
pub use open::{
    DashFetchedPresentationKind, DashVodOpenError, DashVodOpenResult,
    classify_fetched_dash_presentation, prepare_dash_vod,
};
pub use plan::DashPlanError;
pub use request::{
    DashFetchedManifestInput, DashManifestInput, DashResourceReference, DashSerializedComponent,
    DashSerializedFragment, DashSerializedFragmentKind, DashSerializedPresentation,
    DashVodHttpContext, DashVodInput, DashVodOpenPolicy, DashVodOpenRequest,
};
pub use selection::{
    DashPresentationSelection, DashRepresentationEvidence, DashRepresentationSelectionError,
    DashVideoDimensions,
};
