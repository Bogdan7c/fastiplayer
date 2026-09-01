//! Native stable-root endpoint refresh для direct dynamic DASH.

use rustiplayer_config::NetworkConfig;
use source_core::CancellationToken;
use web_media_core::ExactSelectionIdentity;
use web_media_dash::{
    DashEndpointRefreshError, DashEndpointRefreshPort, DashEndpointRefreshReply,
    DashEndpointRefreshRequest, DashManifestInput,
};
use web_media_transport_api::{MediaPresentation, SourceGeneration};

use super::{NativeDashUrl, native_adaptive_http_context, native_transport_request};

/// Создаёт fresh HTTP generation из stable MPD root без extractor subprocess-а.
pub(super) struct NativeDashEndpointRefreshPort {
    /// Stable source parent одной installed lineage.
    parent: ExactSelectionIdentity,
    /// Root intent не содержит Representation/fragment endpoint-ов.
    source: NativeDashUrl,
    /// Existing bounded HTTP/retry policy.
    network_config: NetworkConfig,
    /// Общая cooperative cancellation installed attempt-а.
    cancellation: CancellationToken,
}

impl NativeDashEndpointRefreshPort {
    /// Связывает endpoint recovery только с stable native source lineage.
    pub(super) fn new(
        parent: &ExactSelectionIdentity,
        source: NativeDashUrl,
        network_config: NetworkConfig,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            parent: parent.clone(),
            source,
            network_config,
            cancellation,
        }
    }
}

impl DashEndpointRefreshPort for NativeDashEndpointRefreshPort {
    /// Stages fresh context; S35 runtime сам fetch-ит, валидирует и атомарно commit-ит MPD.
    fn refresh(
        &self,
        request: DashEndpointRefreshRequest,
    ) -> Result<DashEndpointRefreshReply, DashEndpointRefreshError> {
        if self.cancellation.is_cancelled() {
            return Err(DashEndpointRefreshError::Cancelled);
        }
        let generation = request
            .previous_generation
            .value()
            .checked_add(1)
            .map(SourceGeneration::new)
            .ok_or(DashEndpointRefreshError::AttemptsExhausted)?;
        let adaptive_limits =
            crate::web_media_adaptive_config::adaptive_transport_limits(&self.network_config)
                .map_err(|_| DashEndpointRefreshError::AttemptsExhausted)?;
        let transport_request = native_transport_request(
            &self.parent,
            &self.source,
            MediaPresentation::Live,
            generation,
            self.cancellation.clone(),
        )
        .map_err(|_| DashEndpointRefreshError::AttemptsExhausted)?;
        let http =
            native_adaptive_http_context(transport_request, &self.network_config, adaptive_limits)
                .map_err(|_| DashEndpointRefreshError::AttemptsExhausted)?;
        if self.cancellation.is_cancelled() {
            return Err(DashEndpointRefreshError::Cancelled);
        }
        Ok(DashEndpointRefreshReply {
            http: Box::new(http),
            generation,
            manifest: DashManifestInput {
                target: self.source.target().clone(),
                xml_budgets: crate::web_media_dash_open::dash_xml_budgets()
                    .map_err(|_| DashEndpointRefreshError::AttemptsExhausted)?,
                mpd_limits: crate::web_media_dash_open::dash_mpd_limits(),
            },
        })
    }
}
