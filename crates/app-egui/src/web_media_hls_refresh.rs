//! App-owned yt-dlp re-extraction за neutral HLS endpoint request/reply port.

use std::sync::Mutex;

use rustiplayer_config::{NetworkConfig, YtDlpConfig};
use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpLiveIntent, YtDlpMediaLocator,
    resolve_yt_dlp_candidate_snapshot_with_config_and_cancellation,
};
use source_core::{CancellationToken, SourceRuntimeConfig};
use web_media_core::ExtractionGeneration;
use web_media_hls::{
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsManifestInput,
};
use web_media_transport_api::{SourceGeneration, TransportProviderId};

/// Process-lifetime state одной active HLS candidate lineage.
pub(crate) struct AppHlsEndpointRefreshPort {
    locator: YtDlpMediaLocator,
    yt_dlp_config: YtDlpConfig,
    network_config: NetworkConfig,
    source_config: SourceRuntimeConfig,
    provider_id: TransportProviderId,
    selection: Mutex<YtDlpCandidateSelection>,
    cancellation: CancellationToken,
}

impl AppHlsEndpointRefreshPort {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        locator: YtDlpMediaLocator,
        yt_dlp_config: YtDlpConfig,
        network_config: NetworkConfig,
        source_config: SourceRuntimeConfig,
        provider_id: TransportProviderId,
        selection: YtDlpCandidateSelection,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            locator,
            yt_dlp_config,
            network_config,
            source_config,
            provider_id,
            selection: Mutex::new(selection),
            cancellation,
        }
    }
}

impl HlsEndpointRefreshPort for AppHlsEndpointRefreshPort {
    fn refresh(
        &self,
        request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        if self.cancellation.is_cancelled() {
            return Err(HlsEndpointRefreshError::Cancelled);
        }
        let mut selection_guard = self
            .selection
            .lock()
            .map_err(|_| HlsEndpointRefreshError::OwnerDisconnected)?;
        let previous = selection_guard.clone();
        let extraction_generation = previous
            .exact_identity()
            .generation()
            .value()
            .checked_add(1)
            .map(ExtractionGeneration::new)
            .ok_or(HlsEndpointRefreshError::AttemptsExhausted)?;
        let snapshot = resolve_yt_dlp_candidate_snapshot_with_config_and_cancellation(
            &self.locator,
            previous.exact_identity().source(),
            extraction_generation,
            &self.yt_dlp_config,
            &|| self.cancellation.is_cancelled(),
        )
        .map_err(|_| {
            if self.cancellation.is_cancelled() {
                HlsEndpointRefreshError::Cancelled
            } else {
                HlsEndpointRefreshError::AttemptsExhausted
            }
        })?;
        if snapshot.live_intent() != YtDlpLiveIntent::Live {
            return Err(HlsEndpointRefreshError::IncompatibleLiveCandidate);
        }
        let matched = snapshot
            .rematch_exact(&previous)
            .map_err(|_| HlsEndpointRefreshError::SemanticRematchFailed)?;
        let fresh_selection = snapshot
            .selection_for(matched.candidate())
            .map_err(|_| HlsEndpointRefreshError::SemanticRematchFailed)?;
        let transport_generation = request
            .previous_generation
            .value()
            .checked_add(1)
            .map(SourceGeneration::new)
            .ok_or(HlsEndpointRefreshError::AttemptsExhausted)?;
        let projected = crate::web_media_hls_open::project_hls_runtime_material(
            matched.candidate(),
            self.provider_id.clone(),
            transport_generation,
            &self.source_config,
            &self.network_config,
            self.cancellation.clone(),
            None,
        )
        .map_err(|_| HlsEndpointRefreshError::IncompatibleLiveCandidate)?;
        if matches!(&projected.manifest, HlsManifestInput::InlineMedia { .. }) {
            return Err(HlsEndpointRefreshError::IncompatibleLiveCandidate);
        }
        *selection_guard = fresh_selection;
        Ok(HlsEndpointRefreshReply {
            http: projected.http,
            generation: transport_generation,
            manifest: projected.manifest,
            overrides: projected.overrides,
        })
    }
}
