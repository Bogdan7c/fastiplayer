//! App-owned staged re-extraction за neutral DASH endpoint refresh boundary.

use std::sync::atomic::{AtomicU64, Ordering};

use fastiplayer_config::{NetworkConfig, YtDlpConfig};
use service_ytdlp::{YtDlpCandidateSelection, YtDlpLiveIntent, YtDlpMediaLocator};
use source_core::{CancellationToken, SourceRuntimeConfig};
use web_media_core::ExtractionGeneration;
use web_media_dash::{
    DashEndpointRefreshError, DashEndpointRefreshPort, DashEndpointRefreshReply,
    DashEndpointRefreshRequest,
};
use web_media_transport_api::{SourceGeneration, TransportProviderId};

/// Process-lifetime state одной active DASH lineage.
pub(crate) struct AppDashEndpointRefreshPort {
    locator: YtDlpMediaLocator,
    yt_dlp_config: YtDlpConfig,
    extractor_adapter: service_ytdlp::YtDlpExtractorAdapter,
    network_config: NetworkConfig,
    source_config: SourceRuntimeConfig,
    provider_id: TransportProviderId,
    /// Immutable semantic identity; endpoint generations не меняют пользовательский выбор.
    semantic_anchor: YtDlpCandidateSelection,
    /// Отдельная монотонная lineage для дорогих yt-dlp extraction attempts.
    extraction_generations: ExtractionGenerationAllocator,
    cancellation: CancellationToken,
}

/// Checked atomic allocator не допускает reuse/wrap extraction generation.
struct ExtractionGenerationAllocator {
    last_issued: AtomicU64,
}

impl ExtractionGenerationAllocator {
    /// Начинает lineage с generation исходного semantic anchor-а.
    fn new(initial: ExtractionGeneration) -> Self {
        Self {
            last_issued: AtomicU64::new(initial.value()),
        }
    }

    /// Выдаёт строго следующую generation либо fail-close при `u64` overflow.
    fn allocate(&self) -> Result<ExtractionGeneration, DashEndpointRefreshError> {
        let previous = self
            .last_issued
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| DashEndpointRefreshError::AttemptsExhausted)?;
        previous
            .checked_add(1)
            .map(ExtractionGeneration::new)
            .ok_or(DashEndpointRefreshError::AttemptsExhausted)
    }
}

impl AppDashEndpointRefreshPort {
    /// Создаёт immutable semantic anchor и monotonic extraction allocator.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        locator: YtDlpMediaLocator,
        yt_dlp_config: YtDlpConfig,
        extractor_adapter: service_ytdlp::YtDlpExtractorAdapter,
        network_config: NetworkConfig,
        source_config: SourceRuntimeConfig,
        provider_id: TransportProviderId,
        selection: YtDlpCandidateSelection,
        cancellation: CancellationToken,
    ) -> Self {
        let initial_extraction_generation = selection.exact_identity().generation();
        Self {
            locator,
            yt_dlp_config,
            extractor_adapter,
            network_config,
            source_config,
            provider_id,
            semantic_anchor: selection,
            extraction_generations: ExtractionGenerationAllocator::new(
                initial_extraction_generation,
            ),
            cancellation,
        }
    }
}

impl DashEndpointRefreshPort for AppDashEndpointRefreshPort {
    fn refresh(
        &self,
        request: DashEndpointRefreshRequest,
    ) -> Result<DashEndpointRefreshReply, DashEndpointRefreshError> {
        if self.cancellation.is_cancelled() {
            return Err(DashEndpointRefreshError::Cancelled);
        }
        let extraction_generation = self.extraction_generations.allocate()?;
        let snapshot = self
            .extractor_adapter
            .resolve_candidate_snapshot_with_cancellation(
                &self.locator,
                self.semantic_anchor.exact_identity().source(),
                extraction_generation,
                &self.yt_dlp_config,
                web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
                &|| self.cancellation.is_cancelled(),
            )
            .map_err(|_| {
                if self.cancellation.is_cancelled() {
                    DashEndpointRefreshError::Cancelled
                } else {
                    DashEndpointRefreshError::AttemptsExhausted
                }
            })?;
        if snapshot.live_intent() != YtDlpLiveIntent::Live {
            return Err(DashEndpointRefreshError::IncompatibleLiveCandidate);
        }
        let matched = snapshot
            .rematch_exact(&self.semantic_anchor)
            .map_err(|_| DashEndpointRefreshError::SemanticRematchFailed)?;
        let generation = request
            .previous_generation
            .value()
            .checked_add(1)
            .map(SourceGeneration::new)
            .ok_or(DashEndpointRefreshError::AttemptsExhausted)?;
        let (http, manifest) = crate::web_media_dash_open::project_dash_live_runtime_material(
            matched.candidate(),
            self.provider_id.clone(),
            generation,
            &self.source_config,
            &self.network_config,
            self.cancellation.clone(),
        )
        .map_err(|_| DashEndpointRefreshError::IncompatibleLiveCandidate)?;
        Ok(DashEndpointRefreshReply {
            http,
            generation,
            manifest,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use web_media_core::ExtractionGeneration;

    use super::ExtractionGenerationAllocator;

    #[test]
    fn extraction_generation_allocator_is_monotonic_and_overflow_checked() {
        let allocator = ExtractionGenerationAllocator::new(ExtractionGeneration::new(40));
        assert_eq!(allocator.allocate().expect("next generation").value(), 41);
        assert_eq!(
            allocator.allocate().expect("following generation").value(),
            42
        );

        let exhausted = ExtractionGenerationAllocator {
            last_issued: AtomicU64::new(u64::MAX),
        };
        assert!(exhausted.allocate().is_err());
    }
}
