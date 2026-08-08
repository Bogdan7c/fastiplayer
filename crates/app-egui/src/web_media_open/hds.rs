//! App-owned composition S38 HDS F4M/F4F VOD runtime-а.

use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bounded_xml_reader::XmlBudgets;
use demux_api::{
    DemuxRegistry, DemuxSniffBudget, ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle,
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use hds_manifest_core::{F4mManifestLimits, HdsBootstrapLimits};
use media_core::{DemuxRetryHint, Demuxer, MediaTime};
use player_core::{
    MediaPlaybackWindow, PreparedDemuxSeekEnqueueError, PreparedDemuxSeekOutcome,
    PreparedDemuxSeekPort, PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};
use rustiplayer_config::NetworkConfig;
use service_ytdlp::{YtDlpLiveIntent, YtDlpNormalizedCandidate, YtDlpTransportRequestContext};
use source_core::{CancellationToken, SourceRuntimeConfig};
use web_media_adaptive::{AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{ContainerFamily, PreferredHeightPolicy, StreamLayout, TransportFamily};
use web_media_hds::{
    HdsCatalogDiscoveryRequest, HdsRenditionCapabilityProbe, HdsVodOpenPolicy,
    discover_hds_renditions, prepare_discovered_hds_vod,
};
use web_media_transport_api::TransportProviderId;

#[cfg(test)]
mod provider_default_tests;

/// Prepared HDS candidate перед player commit barrier-ом.
pub(super) struct PreparedHdsCandidate {
    /// Nonblocking worker-owned S30 demuxer.
    pub(super) demuxer: Box<dyn Demuxer + Send>,
    /// Receipted VOD seek control.
    pub(super) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    /// Absolute source window, которое player проецирует в public zero-based timeline.
    pub(super) playback_window: MediaPlaybackWindow,
    pub(super) component_variants:
        crate::web_media_open::component_variants::PreparedComponentVariantCatalog,
}

/// Проверяет единый provider-probed HDS/F4F contract из normalized descriptor-а.
pub(super) fn candidate_is_hds(candidate: &YtDlpNormalizedCandidate) -> bool {
    match candidate.descriptor().layout() {
        StreamLayout::ContentProbed(component) => {
            component.transport().family() == TransportFamily::Hds
                && component.probe_container() == ContainerFamily::F4f
        }
        StreamLayout::Muxed(_)
        | StreamLayout::VideoOnly(_)
        | StreamLayout::AudioOnly(_)
        | StreamLayout::Separate { .. }
        | StreamLayout::HlsMuxedCodecDeferred(_) => false,
    }
}

/// Выполняет bounded F4M hierarchy/bootstrap preparation на media-open worker-е.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_hds_candidate(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    demux_registry: Arc<DemuxRegistry>,
    cancellation: CancellationToken,
    live_intent: YtDlpLiveIntent,
    preferred_height: PreferredHeightPolicy,
    component_selection_intent:
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent,
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    capability_probe: &dyn HdsRenditionCapabilityProbe,
) -> Result<PreparedHdsCandidate> {
    ensure_hds_vod_intent(live_intent)?;
    let StreamLayout::ContentProbed(_) = candidate.descriptor().layout() else {
        bail!("HDS candidate должен сохранять provider-owned content-probed F4F contract");
    };
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let context = YtDlpTransportRequestContext::new(provider_id, generation, cancellation);
    let transport_request = candidate
        .hds_transport_request(&context)
        .context("YtDlp HDS request material нельзя выразить как F4M manifest request")?;
    let adaptive_limits =
        crate::web_media_adaptive_config::adaptive_transport_limits(network_config)
            .context("Не удалось собрать HDS adaptive transport limits")?;
    let policy = hds_policy(adaptive_limits)?;
    let discovered = discover_hds_renditions(HdsCatalogDiscoveryRequest {
        transport_request,
        source_config: source_config.clone(),
        demux_registry,
        policy,
        catalog_identity,
        capability_probe,
        preferred_height,
    })?;
    let catalog = Arc::new(discovered.catalog().clone());
    let provider_selection = match component_selection_intent {
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::ProviderDefault => {
            discovered.provider_default().clone()
        }
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::Semantic(
            semantic,
        ) => catalog.rematch_semantic(semantic)?,
    };
    let web_media_core::ComponentVariantSelectionRequest::Coupled { presentation } =
        provider_selection.exact_selection_request()
    else {
        bail!("HDS catalog selection must remain coupled A/V");
    };
    let opened = prepare_discovered_hds_vod(discovered, presentation)?;
    let component_variants =
        crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Installed {
            catalog,
            provider_selection,
        };
    let hds_window = opened.presentation_window();
    let playback_window = hds_playback_window(hds_window.start(), hds_window.end_exclusive())?;
    let seek_port: Arc<dyn PreparedDemuxSeekPort> = Arc::new(HdsPreparedDemuxSeekPort {
        handle: opened.async_seek_handle(),
    });
    let demuxer = opened.into_demuxer();
    Ok(PreparedHdsCandidate {
        demuxer,
        seek_port,
        playback_window,
        component_variants,
    })
}

/// Останавливает HDS live/DVR до materialization request-а и любого network I/O.
fn ensure_hds_vod_intent(live_intent: YtDlpLiveIntent) -> Result<()> {
    if matches!(
        live_intent,
        YtDlpLiveIntent::Unspecified | YtDlpLiveIntent::NotLive
    ) {
        return Ok(());
    }

    bail!("HDS live/DVR не входит в approved S38 base/VOD profile");
}

/// Переводит neutral HDS clock boundary в существующий player-owned window.
fn hds_playback_window(start: Duration, end_exclusive: Duration) -> Result<MediaPlaybackWindow> {
    MediaPlaybackWindow::new(
        MediaTime::from_duration(start),
        Some(MediaTime::from_duration(end_exclusive)),
    )
    .context("HDS presentation window нельзя выразить через player boundary")
}

/// Adapter не переносит Progressive vocabulary в player-core.
struct HdsPreparedDemuxSeekPort {
    /// Cloneable worker control handle.
    handle: ProgressiveAsyncSeekHandle,
}

impl PreparedDemuxSeekPort for HdsPreparedDemuxSeekPort {
    /// Enqueues seek without blocking the player owner.
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: media_core::DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        self.handle
            .enqueue(
                ProgressiveSeekFence {
                    runtime_generation: self.handle.runtime_generation(),
                    request_id: ProgressiveSeekRequestId::new(request_id.value()),
                },
                request,
            )
            .map_err(map_enqueue_error)
    }

    /// Converts the neutral progressive receipt into player vocabulary.
    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        self.handle
            .poll_receipt()
            .map(|receipt| PreparedDemuxSeekReceipt {
                request_id: PreparedDemuxSeekRequestId::new(receipt.fence.request_id.value()),
                outcome: match receipt.outcome {
                    ProgressiveAsyncSeekOutcome::Succeeded(result) => {
                        PreparedDemuxSeekOutcome::Succeeded(result)
                    }
                    ProgressiveAsyncSeekOutcome::Failed => PreparedDemuxSeekOutcome::Failed,
                    ProgressiveAsyncSeekOutcome::Cancelled => PreparedDemuxSeekOutcome::Cancelled,
                    ProgressiveAsyncSeekOutcome::Superseded => PreparedDemuxSeekOutcome::Superseded,
                    ProgressiveAsyncSeekOutcome::Stale => PreparedDemuxSeekOutcome::Stale,
                },
            })
    }
}

/// Ставит все S38 budgets в одном app-owned policy object.
fn hds_policy(adaptive_limits: AdaptiveTransportLimits) -> Result<HdsVodOpenPolicy> {
    Ok(HdsVodOpenPolicy {
        xml_budgets: hds_xml_budgets()?,
        manifest_limits: F4mManifestLimits::new(
            NonZeroUsize::new(64).expect("HDS media rows"),
            NonZeroUsize::new(32).expect("HDS bootstrap rows"),
            NonZeroUsize::new(2 * 1024 * 1024).expect("HDS bootstrap bytes"),
            NonZeroUsize::new(4096).expect("HDS manifest string bytes"),
        ),
        bootstrap_limits: HdsBootstrapLimits {
            maximum_bytes: NonZeroUsize::new(2 * 1024 * 1024).expect("HDS abst bytes"),
            maximum_boxes: NonZeroUsize::new(128).expect("HDS bootstrap boxes"),
            maximum_fragments: NonZeroUsize::new(16_384).expect("HDS timeline fragments"),
            maximum_string_bytes: NonZeroUsize::new(4096).expect("HDS bootstrap strings"),
        },
        adaptive_limits,
        adaptive_retry: AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("HDS retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .context("HDS adaptive retry policy invalid")?,
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(256 * 1024).expect("HDS sniff bytes"),
            NonZeroUsize::new(2).expect("HDS sniff segments"),
            Duration::from_secs(2),
        )
        .context("HDS demux sniff budget invalid")?,
        demux_buffer_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(256).expect("HDS demux event queue"),
            NonZeroUsize::new(16 * 1024 * 1024).expect("HDS demux encoded queue"),
        ),
        demux_retry_hint: DemuxRetryHint::new(Duration::from_millis(10))
            .context("HDS demux retry hint invalid")?,
        async_seek_limits: ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(16).expect("HDS outstanding seek receipts"),
        ),
        maximum_hierarchy_depth: 8,
        maximum_manifest_documents: 32,
        maximum_renditions: 64,
    })
}

/// S04X budgets ограничивают untrusted F4M XML до domain parsing.
fn hds_xml_budgets() -> Result<XmlBudgets> {
    XmlBudgets::builder()
        .maximum_document_bytes(2 * 1024 * 1024)
        .maximum_depth(32)
        .maximum_tokens(65_536)
        .maximum_attributes_per_element(64)
        .maximum_attribute_count(65_536)
        .maximum_attribute_bytes(512 * 1024)
        .maximum_namespace_declarations_per_element(16)
        .maximum_namespace_declaration_count(1024)
        .maximum_namespace_bytes(64 * 1024)
        .maximum_text_bytes(512 * 1024)
        .build()
        .context("HDS XML budgets invalid")
}

/// Maps the adaptive layer's async seek errors into the player boundary.
fn map_enqueue_error(error: ProgressiveAsyncSeekEnqueueError) -> PreparedDemuxSeekEnqueueError {
    match error {
        ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull => {
            PreparedDemuxSeekEnqueueError::ReceiptQueueFull
        }
        ProgressiveAsyncSeekEnqueueError::NonMonotonicRequestIdentity => {
            PreparedDemuxSeekEnqueueError::NonMonotonicRequestIdentity
        }
        ProgressiveAsyncSeekEnqueueError::WorkerStopped => {
            PreparedDemuxSeekEnqueueError::WorkerStopped
        }
        ProgressiveAsyncSeekEnqueueError::CapabilityAbsent => {
            PreparedDemuxSeekEnqueueError::CapabilityUnavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ensure_hds_vod_intent, hds_playback_window};
    use service_ytdlp::YtDlpLiveIntent;
    use std::time::Duration;

    /// Закрепляет S38 no-op: live states не доходят до HDS request preparation.
    #[test]
    fn rejects_every_hds_live_intent_before_vod_preparation() {
        for unsupported_intent in [
            YtDlpLiveIntent::Live,
            YtDlpLiveIntent::Upcoming,
            YtDlpLiveIntent::PostLive,
            YtDlpLiveIntent::Incompatible,
        ] {
            let error = ensure_hds_vod_intent(unsupported_intent)
                .expect_err("HDS live intent must stay outside the approved S38 VOD profile");

            assert!(
                error
                    .to_string()
                    .contains("HDS live/DVR не входит в approved S38 base/VOD profile")
            );
        }

        ensure_hds_vod_intent(YtDlpLiveIntent::Unspecified)
            .expect("missing live metadata keeps the bounded VOD-only admission path");
        ensure_hds_vod_intent(YtDlpLiveIntent::NotLive)
            .expect("explicit VOD intent remains supported");
    }

    /// App сохраняет absolute origin; zero-based projection остаётся player-owned.
    #[test]
    fn maps_hds_clock_to_player_playback_window() {
        let window = hds_playback_window(Duration::from_secs(5), Duration::from_secs(7))
            .expect("valid HDS playback window");

        assert_eq!(window.start().as_duration(), Duration::from_secs(5));
        assert_eq!(
            window.end_exclusive().map(|end| end.as_duration()),
            Some(Duration::from_secs(7))
        );
    }
}
