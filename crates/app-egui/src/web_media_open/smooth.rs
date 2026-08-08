//! App-owned composition строгого Smooth Streaming VOD runtime-а.

use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bounded_xml_reader::XmlBudgets;
use demux_api::{
    CompositeComponentLeadPolicy, DemuxContainerId, DemuxHints, DemuxInput, DemuxRegistry,
    DemuxSniffBudget, ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle,
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
use media_core::{DemuxRetryHint, Demuxer};
use player_core::{
    PreparedDemuxSeekEnqueueError, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};
use rustiplayer_config::NetworkConfig;
use service_ytdlp::{YtDlpLiveIntent, YtDlpNormalizedCandidate, YtDlpTransportRequestContext};
use source_core::{CancellationToken, SourceRuntimeConfig};
use symphonia_demux::PresentationWindowOrderedIsoMp4Demuxer;
use web_media_adaptive::{AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{
    ComponentVariantCatalogLimit, PreferredHeightPolicy, StreamLayout, TransportFamily,
};
use web_media_smooth::{
    AggregateInitializationByteLimit, FragmentInitializationLimits, FragmentInspectionLimits,
    FragmentWriteLimits, SmoothAudioDemuxOpenRequest, SmoothCatalogDiscoveryPolicy,
    SmoothCatalogDiscoveryRequest, SmoothFragmentSourcePolicy, SmoothIsoBmffDemuxFactory,
    SmoothManifestLimits, SmoothPreparationPolicy, SmoothPrepareRequest,
    SmoothVideoDemuxOpenRequest, SmoothVodDemuxPolicy, discover_smooth_vod_catalog,
    prepare_smooth_vod,
};
use web_media_transport_api::TransportProviderId;

use super::{PreparedComponentVariantCatalog, YtDlpComponentSelectionOpenIntent};

/// Готовый до player commit-а Smooth VOD candidate.
pub(super) struct PreparedSmoothCandidate {
    /// Nonblocking demuxer с worker-owned fragment I/O.
    pub(super) demuxer: Box<dyn Demuxer + Send>,
    /// Neutral receipted seek boundary этого exact runtime-а.
    pub(super) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    /// Fresh catalog и установленный exact provider selection.
    pub(super) component_variants: PreparedComponentVariantCatalog,
}

/// Concrete S28A adapters используют один app-owned registry.
struct AppSmoothIsoBmffDemuxFactory {
    /// Registry содержит exact production Symphonia ISO-BMFF factory.
    registry: Arc<DemuxRegistry>,
    /// Required identity исключает случайный выбор container-а по hint.
    required_container: DemuxContainerId,
}

impl AppSmoothIsoBmffDemuxFactory {
    /// Проверяет required container identity до запуска background worker-а.
    fn new(registry: Arc<DemuxRegistry>) -> Result<Self> {
        Ok(Self {
            registry,
            required_container: DemuxContainerId::new("iso-bmff")
                .context("ISO-BMFF demux container identity invalid")?,
        })
    }
}

impl SmoothIsoBmffDemuxFactory for AppSmoothIsoBmffDemuxFactory {
    /// Открывает обычную video-ось через S28A ordered-segment adapter.
    fn open_video(&self, request: SmoothVideoDemuxOpenRequest) -> Result<Box<dyn Demuxer + Send>> {
        let parts = request.into_parts();
        self.registry
            .open_required_container(
                DemuxInput::ordered_segments(parts.source),
                DemuxHints::none(),
                parts.sniff_budget,
                parts.cancellation,
                self.required_container.clone(),
            )
            .context("Smooth video ISO-BMFF adapter не открыл reconstructed fragments")
    }

    /// Открывает audio-ось через provenance-aware F3A adapter.
    fn open_audio(&self, request: SmoothAudioDemuxOpenRequest) -> Result<Box<dyn Demuxer + Send>> {
        let parts = request.into_parts();
        let demuxer = PresentationWindowOrderedIsoMp4Demuxer::new_with_registry(
            parts.source,
            parts.cancellation,
            parts.sniff_budget,
            Arc::clone(&self.registry),
        )
        .context("Smooth audio ISO-BMFF presentation-window adapter не открыл fragments")?;
        Ok(Box::new(demuxer))
    }
}

/// Adapter не переносит Smooth vocabulary в player-core.
struct SmoothPreparedDemuxSeekPort {
    /// Cloneable P5 control handle.
    handle: ProgressiveAsyncSeekHandle,
}

impl PreparedDemuxSeekPort for SmoothPreparedDemuxSeekPort {
    /// Строит exact runtime fence из player-owned request identity.
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

    /// Переводит provider receipt в neutral player vocabulary.
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

/// Проверяет exact transport family без provider open-а.
pub(super) fn candidate_is_smooth(candidate: &YtDlpNormalizedCandidate) -> bool {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(component) => {
            component.transport().family() == TransportFamily::SmoothStreaming
        }
        StreamLayout::VideoOnly(_)
        | StreamLayout::AudioOnly(_)
        | StreamLayout::Separate { .. }
        | StreamLayout::HlsMuxedCodecDeferred(_)
        | StreamLayout::ContentProbed(_) => false,
    }
}

/// Выполняет двухфазную подготовку catalog → exact selection → sources/demux.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_smooth_candidate(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    demux_registry: Arc<DemuxRegistry>,
    cancellation: CancellationToken,
    live_intent: YtDlpLiveIntent,
    component_selection_intent: YtDlpComponentSelectionOpenIntent,
    preferred_height: PreferredHeightPolicy,
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    capability_probe: &crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe,
) -> Result<PreparedSmoothCandidate> {
    if !matches!(
        live_intent,
        YtDlpLiveIntent::Unspecified | YtDlpLiveIntent::NotLive
    ) {
        bail!("Smooth live/DVR не входит в approved S36 profile");
    }

    let adaptive_limits =
        crate::web_media_adaptive_config::adaptive_transport_limits(network_config)
            .context("Не удалось собрать Smooth adaptive transport limits")?;
    let request_context = YtDlpTransportRequestContext::new(
        provider_id,
        crate::web_media_adaptive_config::initial_adaptive_source_generation(),
        cancellation,
    );
    let transport = candidate
        .smooth_manifest_transport_request(&request_context)
        .context("YtDlp ISM material нельзя выразить как Smooth manifest request")?;
    let preparation = SmoothPrepareRequest::new(
        transport,
        source_config,
        catalog_identity.generation(),
        preferred_height,
        preparation_policy(adaptive_limits)?,
    );
    let factory = Arc::new(AppSmoothIsoBmffDemuxFactory::new(demux_registry)?);
    let (opened, catalog, selected) = match component_selection_intent {
        YtDlpComponentSelectionOpenIntent::ProviderDefault => {
            let prepared = prepare_smooth_vod(preparation)
                .context("Smooth fast default preparation failed")?;
            let catalog = prepared.catalog().clone();
            let selected = prepared.provider_default_selection().clone();
            let opened = prepared
                .into_selected_fragment_sources(
                    selected.clone(),
                    fragment_source_policy(adaptive_limits)?,
                )?
                .into_progressive_demuxer(factory, demux_policy()?)?;
            (opened, catalog, selected)
        }
        YtDlpComponentSelectionOpenIntent::Semantic(semantic) => {
            let discovered = discover_smooth_vod_catalog(SmoothCatalogDiscoveryRequest::new(
                preparation,
                factory,
                capability_probe,
                discovery_policy(adaptive_limits)?,
            ))?;
            let catalog = discovered.catalog().clone();
            let selected = catalog.rematch_semantic(semantic.clone())?;
            let opened = discovered.open_semantic(
                semantic,
                fragment_source_policy(adaptive_limits)?,
                demux_policy()?,
            )?;
            (opened, catalog, selected)
        }
    };
    let seek_port: Arc<dyn PreparedDemuxSeekPort> = Arc::new(SmoothPreparedDemuxSeekPort {
        handle: opened.async_seek_handle(),
    });

    Ok(PreparedSmoothCandidate {
        demuxer: opened.into_demuxer(),
        seek_port,
        component_variants: PreparedComponentVariantCatalog::Installed {
            catalog: Arc::new(catalog),
            provider_selection: selected,
        },
    })
}

fn discovery_policy(limits: AdaptiveTransportLimits) -> Result<SmoothCatalogDiscoveryPolicy> {
    Ok(SmoothCatalogDiscoveryPolicy::new(
        fragment_source_policy(limits)?,
        DemuxSniffBudget::new(
            NonZeroUsize::new(64 * 1_024).expect("Smooth discovery sniff bytes"),
            NonZeroUsize::new(8).expect("Smooth discovery sniff segments"),
            Duration::from_secs(2),
        )?,
        NonZeroUsize::new(4_096).expect("Smooth discovery event limit"),
    ))
}

/// Все manifest/init/catalog budgets принадлежат app composition policy.
fn preparation_policy(limits: AdaptiveTransportLimits) -> Result<SmoothPreparationPolicy> {
    Ok(SmoothPreparationPolicy::new(
        limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("Smooth retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .context("Smooth retry policy invalid")?,
        smooth_xml_budgets()?,
        smooth_manifest_limits()?,
        FragmentInitializationLimits::builder()
            .maximum_output_bytes(64 * 1_024)
            .maximum_codec_configuration_bytes(16 * 1_024)
            .build()
            .map_err(|error| anyhow::anyhow!("Smooth initialization limits invalid: {error:?}"))?,
        AggregateInitializationByteLimit::new(
            NonZeroUsize::new(256 * 1_024).expect("Smooth aggregate initialization bytes"),
        ),
        ComponentVariantCatalogLimit::new(64).context("Smooth catalog limit invalid")?,
        web_media_core::ComponentVariantEdgeLimit::new(4_096)
            .context("Smooth compatibility edge limit invalid")?,
    ))
}

/// S04X budgets ограничивают untrusted client manifest до schema parsing.
fn smooth_xml_budgets() -> Result<XmlBudgets> {
    XmlBudgets::builder()
        .maximum_document_bytes(2 * 1_024 * 1_024)
        .maximum_depth(32)
        .maximum_tokens(65_536)
        .maximum_attributes_per_element(64)
        .maximum_attribute_count(65_536)
        .maximum_attribute_bytes(512 * 1_024)
        .maximum_namespace_declarations_per_element(16)
        .maximum_namespace_declaration_count(1_024)
        .maximum_namespace_bytes(64 * 1_024)
        .maximum_text_bytes(512 * 1_024)
        .build()
        .context("Smooth XML budgets invalid")
}

/// Bounded manifest profile соответствует approved H.264+AAC VOD row.
fn smooth_manifest_limits() -> Result<SmoothManifestLimits> {
    SmoothManifestLimits::builder()
        .maximum_streams(8)
        .maximum_qualities_per_stream(16)
        .maximum_total_qualities(32)
        .maximum_timeline_entries_per_stream(256)
        .maximum_total_timeline_entries(512)
        .maximum_fragments_per_stream(8_192)
        .maximum_total_fragments(16_384)
        .maximum_template_bytes(512)
        .maximum_string_bytes(256)
        .maximum_codec_bytes(4_096)
        .maximum_custom_attributes_per_quality(8)
        .maximum_total_custom_attributes(32)
        .maximum_custom_attribute_name_bytes(64)
        .maximum_custom_attribute_value_bytes(128)
        .build()
        .context("Smooth manifest limits invalid")
}

/// F1 reconstruction budgets связаны с app-owned segment byte limit.
fn fragment_source_policy(limits: AdaptiveTransportLimits) -> Result<SmoothFragmentSourcePolicy> {
    let maximum_segment_bytes = limits.maximum_segment_bytes.get();
    let inspection_limits = FragmentInspectionLimits::builder()
        .max_input_bytes(maximum_segment_bytes)
        .max_box_count(4_096)
        .max_box_depth(16)
        .max_traf_count(4)
        .max_trun_count(64)
        .max_samples(65_536)
        .max_sample_table_bytes(maximum_segment_bytes)
        .max_box_payload_bytes(maximum_segment_bytes)
        .build()
        .context("Smooth fragment inspection limits invalid")?;
    let write_limits = FragmentWriteLimits::try_new(maximum_segment_bytes)
        .context("Smooth fragment write limit invalid")?;
    Ok(SmoothFragmentSourcePolicy::new(
        inspection_limits,
        write_limits,
    ))
}

/// Readiness/interleave/seek queues ограничены независимо от HTTP cache.
fn demux_policy() -> Result<SmoothVodDemuxPolicy> {
    Ok(SmoothVodDemuxPolicy::new(
        DemuxSniffBudget::new(
            NonZeroUsize::new(256 * 1_024).expect("Smooth sniff bytes"),
            NonZeroUsize::new(2).expect("Smooth sniff segments"),
            Duration::from_secs(2),
        )
        .context("Smooth demux sniff budget invalid")?,
        CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(3),
            NonZeroUsize::new(4 * 1_024 * 1_024).expect("Smooth composite packet bytes"),
        )
        .context("Smooth composite lead policy invalid")?,
        ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(256).expect("Smooth event queue"),
            NonZeroUsize::new(16 * 1_024 * 1_024).expect("Smooth encoded queue"),
        ),
        DemuxRetryHint::new(Duration::from_millis(10))
            .context("Smooth demux retry hint invalid")?,
        ProgressiveAsyncSeekLimits::new(
            NonZeroUsize::new(16).expect("Smooth outstanding seek receipts"),
        ),
    ))
}

/// Сохраняет typed enqueue categories player boundary-а.
const fn map_enqueue_error(
    error: ProgressiveAsyncSeekEnqueueError,
) -> PreparedDemuxSeekEnqueueError {
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
