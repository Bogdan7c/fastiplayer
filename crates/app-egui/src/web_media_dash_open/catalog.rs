use super::*;

/// Выполняет provider-owned representation discovery без изменения active runtime.
#[allow(clippy::too_many_arguments)]
pub(crate) fn discover_dash_candidate_catalog(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    demux_registry: Arc<DemuxRegistry>,
    cancellation: CancellationToken,
    live_intent: YtDlpLiveIntent,
    endpoint_refresh: Option<Arc<dyn DashEndpointRefreshPort>>,
    timeline_port_generation: DynamicMediaTimelinePortGeneration,
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    capability_probe: &crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe,
) -> Result<Option<crate::web_media_open::catalog::DiscoveredProviderCatalog>> {
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let context = YtDlpTransportRequestContext::new(provider_id, generation, cancellation);
    let service_components = candidate.dash_transport_components(&context)?;
    let limits = crate::web_media_adaptive_config::adaptive_transport_limits(network_config)?;
    let projected = service_components
        .into_iter()
        .map(|component| project_component(component, source_config, limits))
        .collect::<Result<Vec<_>>>()?;
    let selection = presentation_selection(candidate.descriptor().layout())?;
    let (http, input) = presentation_input(projected)?;
    let catalog_limit = web_media_core::ComponentVariantCatalogLimit::new(256)?;
    let compatibility_edge_limit = web_media_core::ComponentVariantEdgeLimit::new(4_096)?;
    if live_intent == YtDlpLiveIntent::Live {
        let (DashVodHttpContext::Manifest(http), DashVodInput::Manifest(manifest)) = (http, input)
        else {
            return Ok(None);
        };
        let endpoint_refresh = endpoint_refresh
            .ok_or_else(|| anyhow!("DASH live catalog lost endpoint refresh port"))?;
        let discovered = discover_dash_live_catalog(DashLiveCatalogDiscoveryRequest {
            open: DashLiveOpenRequest {
                http,
                generation,
                manifest,
                selection,
                demux_registry,
                policy: dash_policy(limits)?,
                wall_clock: Arc::new(SystemDashWallClock),
                timeline_port_generation,
                initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
                endpoint_refresh,
            },
            catalog_identity,
            catalog_limit,
            compatibility_edge_limit,
            capability_probe,
        })?;
        return Ok(Some(
            crate::web_media_open::catalog::DiscoveredProviderCatalog {
                catalog: Arc::new(discovered.catalog().clone()),
                provider_selection: discovered.provider_default().clone(),
                rejected_siblings: discovered.rejections().len(),
            },
        ));
    }
    ensure_static_dash_intent(live_intent)?;
    if !matches!(
        (&http, &input),
        (DashVodHttpContext::Manifest(_), DashVodInput::Manifest(_))
    ) {
        return Ok(None);
    }
    let discovered = discover_dash_vod_catalog(DashVodCatalogDiscoveryRequest {
        open: DashVodOpenRequest {
            http,
            generation,
            input,
            selection,
            demux_registry,
            policy: dash_policy(limits)?,
        },
        catalog_identity,
        catalog_limit,
        compatibility_edge_limit,
        capability_probe,
    })?;
    Ok(Some(
        crate::web_media_open::catalog::DiscoveredProviderCatalog {
            catalog: Arc::new(discovered.catalog().clone()),
            provider_selection: discovered.provider_default().clone(),
            rejected_siblings: discovered.rejections().len(),
        },
    ))
}
