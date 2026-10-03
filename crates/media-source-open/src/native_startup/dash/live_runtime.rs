//! Direct dynamic DASH catalog/rematch composition поверх existing S35 owner-а.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use demux_api::DemuxRegistry;
use web_media_adaptive::AdaptiveHttpContext;
use web_media_core::{
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, WebMediaSelection,
    WebMediaSelectionRematchSource, WebMediaSelectionShape,
};
use web_media_dash::{
    DashDiscoveredLiveOpenError, DashFetchedLiveManifestInput, DashLiveCatalogDiscoveryError,
    DashLiveOpenError, NativeDashLiveCatalogDiscoveryRequest, discover_native_dash_live_catalog,
    prepare_discovered_dash_live,
};
use web_media_transport_api::SourceGeneration;

use super::{
    NativeDashPreparationRequest, NativeDashSnapshotIdentity, NativeDashSourceState,
    PreparedNativeDashLifecycle, PreparedNativeDashMedia, live_refresh,
};

/// Все уже staged inputs direct live branch-а одной physical attempt.
pub(super) struct NativeDashLivePreparation<'request> {
    /// Общий app request владеет policy/capabilities/source intent.
    pub request: &'request NativeDashPreparationRequest<'request>,
    /// Fresh exact parent/catalog identity этой snapshot generation.
    pub snapshot_identity: NativeDashSnapshotIdentity,
    /// Live-scoped HTTP context без VOD expiry observer-а.
    pub http: AdaptiveHttpContext,
    /// Exact generation первого root response.
    pub generation: SourceGeneration,
    /// Тот же fetched MPD с clock observation, без второго GET.
    pub manifest: DashFetchedLiveManifestInput,
    /// Existing fMP4/WebM factories.
    pub demux_registry: Arc<DemuxRegistry>,
}

/// Сохраняет profile exclusion отдельно от malformed/network/cancel/runtime failures.
#[derive(Debug, thiserror::Error)]
pub(super) enum NativeDashLivePreparationError {
    /// Authoritative fetched dynamic catalog не прошёл schema/profile/capability proof.
    #[error("native DASH live catalog discovery failed: {0}")]
    Discovery(#[from] DashLiveCatalogDiscoveryError),
    /// Exact semantic row не открылась существующим S35 runtime-ом.
    #[error("native DASH live selected row open failed: {0}")]
    Open(#[from] DashDiscoveredLiveOpenError),
    /// App composition/policy invariant нарушен вне DASH profile vocabulary.
    #[error("native DASH live app composition failed: {0}")]
    Composition(#[from] anyhow::Error),
}

impl NativeDashLivePreparationError {
    /// Только parser-owned deliberate profile exclusion разрешает initial fallback gate.
    pub(super) fn is_profile_exclusion(&self) -> bool {
        let live_open_error = match self {
            Self::Discovery(DashLiveCatalogDiscoveryError::Open(error)) => Some(error),
            Self::Open(DashDiscoveredLiveOpenError::Open(error)) => Some(error),
            Self::Discovery(DashLiveCatalogDiscoveryError::Catalog(_))
            | Self::Open(
                DashDiscoveredLiveOpenError::Semantic(_)
                | DashDiscoveredLiveOpenError::Selection(_),
            )
            | Self::Composition(_) => None,
        };
        matches!(
            live_open_error,
            Some(DashLiveOpenError::Manifest(
                dash_mpd_core::DashDynamicMpdError::ProfileExcluded(_)
            ))
        )
    }
}

/// Строит capability-filtered live catalog и открывает exact semantic selection.
pub(super) fn prepare_native_dash_live(
    preparation: NativeDashLivePreparation<'_>,
) -> Result<PreparedNativeDashMedia, NativeDashLivePreparationError> {
    let request = preparation.request;
    let endpoint_refresh = Arc::new(live_refresh::NativeDashEndpointRefreshPort::new(
        &preparation.snapshot_identity.parent,
        request.source.clone(),
        request.network_config.clone(),
        request.cancellation.clone(),
    ));
    let capability_probe =
        crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe::new(
            request.system_capabilities.clone(),
            request.audio_capabilities,
        );
    let adaptive_limits =
        crate::web_media_adaptive_config::adaptive_transport_limits(request.network_config)?;
    let discovered = discover_native_dash_live_catalog(NativeDashLiveCatalogDiscoveryRequest {
        http: Box::new(preparation.http),
        generation: preparation.generation,
        manifest: preparation.manifest,
        demux_registry: preparation.demux_registry,
        policy: crate::web_media_dash_open::dash_policy(adaptive_limits)?,
        wall_clock: Arc::new(crate::web_media_dash_open::SystemDashWallClock),
        timeline_port_generation: crate::web_media_open::next_dynamic_timeline_port_generation()?,
        initial_source_epoch: media_core::DynamicMediaTimelineEpoch::new(0),
        endpoint_refresh,
        catalog_identity: preparation.snapshot_identity.catalog,
        catalog_limit: ComponentVariantCatalogLimit::new(256).map_err(anyhow::Error::new)?,
        compatibility_edge_limit: ComponentVariantEdgeLimit::new(4_096)
            .map_err(anyhow::Error::new)?,
        capability_probe: &capability_probe,
        preferred_height: crate::web_media_quality::preferred_height_policy(
            request.web_media_config.preferred_video_height,
        ),
    })?;

    let component_catalog = Arc::new(discovered.catalog().clone());
    let neutral_selection = match request.expected_selection {
        Some(expected) => expected
            .rematch(
                preparation.snapshot_identity.parent.clone(),
                WebMediaSelectionRematchSource::ComponentCatalog(&component_catalog),
            )
            .context("native DASH live semantic selection rematch failed")?,
        None => WebMediaSelection::with_components(
            preparation.snapshot_identity.parent.clone(),
            discovered.provider_default().clone(),
        )
        .context("native DASH live provider default нарушил catalog parent identity")?,
    };
    let WebMediaSelectionShape::Components(component_selection) = neutral_selection.shape() else {
        return Err(anyhow!("native DASH live selection потерял component catalog shape").into());
    };
    let opened = prepare_discovered_dash_live(discovered, component_selection.clone())?;
    let (demuxer, async_seek_handle, timeline_port) = opened.into_parts();
    let seek_port = crate::web_media_dash_open::prepared_dash_seek_port(
        async_seek_handle
            .ok_or_else(|| anyhow!("DASH live runtime потерял seek receipt handle"))?,
    );
    let source_state = NativeDashSourceState::new(
        neutral_selection,
        component_catalog,
        crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
            request.web_media_config,
        ),
    )
    .context("native DASH live neutral catalog projection failed")?;

    Ok(PreparedNativeDashMedia {
        demuxer: Box::new(demuxer),
        seek_port,
        source_state,
        lifecycle: PreparedNativeDashLifecycle::Live { timeline_port },
    })
}
