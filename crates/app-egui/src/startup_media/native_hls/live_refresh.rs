//! Stable-root refresh port для native HLS live без subprocess/extractor ownership.

use super::*;
use web_media_hls::{
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest,
};

/// Process-lifetime native live owner хранит только stable root и transport policy.
pub(super) struct NativeHlsEndpointRefreshPort {
    parent: ExactSelectionIdentity,
    source: NativeHlsUrl,
    network_config: NetworkConfig,
    cancellation: CancellationToken,
}

impl NativeHlsEndpointRefreshPort {
    pub(super) fn new(
        parent: ExactSelectionIdentity,
        source: NativeHlsUrl,
        network_config: NetworkConfig,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            parent,
            source,
            network_config,
            cancellation,
        }
    }
}

impl HlsEndpointRefreshPort for NativeHlsEndpointRefreshPort {
    fn refresh(
        &self,
        request: HlsEndpointRefreshRequest,
    ) -> std::result::Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        if self.cancellation.is_cancelled() {
            return Err(HlsEndpointRefreshError::Cancelled);
        }
        let generation = request
            .previous_generation
            .value()
            .checked_add(1)
            .map(SourceGeneration::new)
            .ok_or(HlsEndpointRefreshError::AttemptsExhausted)?;
        let adaptive_limits =
            crate::web_media_adaptive_config::adaptive_transport_limits(&self.network_config)
                .map_err(|_| HlsEndpointRefreshError::AttemptsExhausted)?;
        let transport_request = native_transport_request(
            &self.parent,
            &self.source,
            MediaPresentation::Live,
            generation,
            self.cancellation.clone(),
        )
        .map_err(|_| HlsEndpointRefreshError::AttemptsExhausted)?;
        let http =
            native_adaptive_http_context(transport_request, &self.network_config, adaptive_limits)
                .map_err(|_| HlsEndpointRefreshError::AttemptsExhausted)?;
        let top_fetch = NativeTopManifestFetchIntent::new(self.source.target().clone());
        let fetched_top = http
            .fetch_resource_blocking(
                top_fetch.request(generation, adaptive_limits.maximum_manifest_bytes),
            )
            .map_err(|error| {
                if matches!(error, AdaptiveTransportError::Cancelled)
                    || self.cancellation.is_cancelled()
                {
                    HlsEndpointRefreshError::Cancelled
                } else {
                    HlsEndpointRefreshError::AttemptsExhausted
                }
            })?;
        let manifest = top_fetch.into_manifest(fetched_top, &http);
        Ok(HlsEndpointRefreshReply {
            http,
            generation,
            manifest,
            overrides: HlsRequestOverrides::new(None),
        })
    }
}
