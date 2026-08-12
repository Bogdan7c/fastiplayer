//! Единый app-owned candidate → transport → demux composition path для YtDlp media.
//!
//! Queue/media-open владеют моментом commit-а, `service-ytdlp` — extraction и
//! request material, pure planner — выбором playable candidate, а этот модуль
//! только соединяет concrete runtime registries до существующего commit barrier.

pub(crate) mod catalog;
pub(crate) mod catalog_capabilities;
pub(crate) mod component_variants;
#[cfg(test)]
mod component_variants_tests;
mod content_probe;
mod content_probe_fallback;
#[cfg(test)]
mod content_probe_tests;
mod hds;
/// Fresh extraction/rematch и process-local generation allocators.
mod preparation;
mod smooth;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use demux_api::{
    CompositeAvDemuxer, CompositeAvTrackSelection, CompositeComponentLeadPolicy, DemuxContainerId,
    DemuxHints, DemuxInput, DemuxInputCapabilities, DemuxInputCapability, DemuxRegistry,
    DemuxSniffBudget, DemuxSourceExtension, ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
};
use media_core::{
    DemuxRetryHint, Demuxer, DynamicMediaTimelinePort, DynamicMediaTimelinePortGeneration, TrackId,
    TrackKind,
};
use rustiplayer_config::{
    NetworkConfig, PlayerDemuxConfig, VideoCodec as ConfigVideoCodec, YtDlpConfig,
    YtDlpHdrSelection,
};
use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpLiveIntent, YtDlpMediaLocator,
    YtDlpNormalizedCandidate, YtDlpProgressiveTransportRequestContext,
};
use source_core::{CancellationToken, SourceRuntimeConfig};
use symphonia_demux::DemuxerOptions;
use web_media_core::{
    ContainerFamily, ExactSelectionIdentity, ExtractionGeneration, FtpScheme, HttpScheme,
    SelectionRequest, SourceIdentity, StreamLayout, TransportFamily,
};
use web_media_dash::DashEndpointRefreshPort;
use web_media_ftp::WebMediaFtpProvider;
use web_media_hls::HlsEndpointRefreshPort;
use web_media_http::WebMediaHttpProvider;
use web_media_playback_plan::{
    DemuxCapabilitySnapshot, HdrSelectionPolicy, PlaybackCapabilitySnapshot,
    PlaybackSelectionPolicy, TransportCapabilityRegistration, TransportCapabilitySnapshot,
    plan_playback,
};
use web_media_transport_api::{
    MediaComponentRole, SourceGeneration, TransportInput, TransportProvider, TransportRegistry,
    TransportSeekability,
};

pub(crate) use component_variants::{
    ComponentVariantFinalizationError, YtDlpComponentSelectionOpenIntent,
    YtDlpExactCandidateOpenIntent,
};
use component_variants::{
    PreparedComponentVariantCatalog, finalize_component_variant_configuration,
};

/// Один кибибайт в bytes для checked config conversion.
const KIB_BYTES: u64 = 1024;
/// Один мебибайт в bytes для checked config conversion.
const MIB_BYTES: u64 = KIB_BYTES * 1024;
/// Runtime generation первого transport open-а внутри одного preparation attempt-а.
const INITIAL_TRANSPORT_GENERATION: u64 = 1;
/// Separate A/V не должен опережать companion более чем на этот интервал.
const COMPOSITE_MAX_TIMESTAMP_LEAD: Duration = Duration::from_millis(500);
/// Один retained packet на component bounded независимо от transport sniff/bootstrap chunk.
const COMPOSITE_MAX_PENDING_PACKET_BYTES: usize = 4 * 1024 * 1024;

/// Process-local source identity allocator не связывает neutral core с queue ID representation.
static NEXT_YT_DLP_SOURCE_IDENTITY: AtomicU64 = AtomicU64::new(1);
/// Process-local identity allocator для S31L timeline ports.
static NEXT_DYNAMIC_TIMELINE_PORT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Process-local allocator независимой generation component catalog-а.
static NEXT_COMPONENT_VARIANT_CATALOG_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Намерение selection: новый лучший playable candidate либо semantic rematch старого exact выбора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum YtDlpCandidateOpenIntent {
    /// Первичное открытие либо явная runtime override/reselection.
    BestPlayable,
    /// Restore/rebuild обязан сохранить semantic candidate identity.
    Exact(Box<YtDlpExactCandidateOpenIntent>),
    /// Service-owned video-only + audio-only composition из одного fresh snapshot-а.
    Composed(Box<component_variants::YtDlpComposedCandidateOpenIntent>),
}

/// Результат pre-barrier подготовки, который ещё не меняет player/queue state.
pub(crate) struct PreparedYtDlpWebMedia {
    /// Player-facing demuxer выбранного single либо compound candidate-а.
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    /// Service metadata из того же extraction snapshot-а, что и exact selection.
    pub(crate) playlist_metadata: service_ytdlp::YtDlpPlaylistMetadata,
    /// Exact установленный выбор для active source и последующего rematch-а.
    pub(crate) candidate_selection: YtDlpCandidateSelection,
    /// Service-owned composed intent, если installed runtime собран из inventory components.
    pub(crate) composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
    /// Secret-safe inventory, публикуемый только вместе с exact Installed source.
    pub(crate) stream_configuration: crate::web_media_stream_model::WebMediaStreamConfiguration,
    /// Полный declared yt-dlp catalog публикуется только после Installed.
    pub(crate) catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
    /// Neutral S31L port присутствует только у proven HLS live runtime.
    pub(crate) timeline_port: Option<DynamicMediaTimelinePort>,
    /// Worker-receipted demux seek port присутствует у static DASH/Smooth/HDS VOD.
    pub(crate) demux_seek_port: Option<Arc<dyn player_core::PreparedDemuxSeekPort>>,
    /// Optional absolute source window для zero-based public presentation.
    pub(crate) playback_window: Option<player_core::MediaPlaybackWindow>,
}

/// Общий pre-barrier runtime result concrete transport branches.
struct OpenedWebCandidate {
    /// Player-facing demuxer.
    demuxer: Box<dyn Demuxer + Send>,
    /// Descriptor-only HLS subtitles.
    subtitles: Arc<[crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition]>,
    /// Dynamic timeline only для proven live provider-а.
    timeline_port: Option<DynamicMediaTimelinePort>,
    /// Async demux seek only для provider-а, который требует worker receipt.
    demux_seek_port: Option<Arc<dyn player_core::PreparedDemuxSeekPort>>,
    /// Provider-owned absolute source window до player commit.
    playback_window: Option<player_core::MediaPlaybackWindow>,
    /// Fresh provider result финализируется до authorization barrier.
    component_variants: PreparedComponentVariantCatalog,
}

/// Source-specific live refresh ports, собранные одним app composition boundary.
struct AdaptiveEndpointRefreshPorts {
    /// HLS endpoint owner присутствует только для выбранного HLS live candidate.
    hls: Option<Arc<dyn HlsEndpointRefreshPort>>,
    /// DASH endpoint owner присутствует только для выбранного DASH live candidate.
    dash: Option<Arc<dyn DashEndpointRefreshPort>>,
}

/// Named context одного concrete candidate open-а.
struct WebCandidateOpenContext {
    /// Extractor-declared live/VOD intent.
    live_intent: YtDlpLiveIntent,
    /// Provider-specific endpoint refresh owners.
    endpoint_refresh_ports: AdaptiveEndpointRefreshPorts,
    /// Reserved neutral timeline generation для live provider-а.
    timeline_port_generation: DynamicMediaTimelinePortGeneration,
    /// Fresh component catalog должен применить этот exact reopen intent.
    component_selection_intent: YtDlpComponentSelectionOpenIntent,
    /// App-owned quality preference для provider default selection.
    preferred_height: web_media_core::PreferredHeightPolicy,
    /// Exact parent и caller-owned catalog generation одного discovery pass-а.
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    /// Общая cancellation generation transport/demux runtime-а.
    cancellation: CancellationToken,
}

/// Открывает YtDlp locator одним S19 → S21C → S22 production path-ом.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_yt_dlp_web_media(
    locator: &YtDlpMediaLocator,
    network_config: &NetworkConfig,
    yt_dlp_config: &YtDlpConfig,
    demux_config: &PlayerDemuxConfig,
    preferred_video_codec_order: &[ConfigVideoCodec],
    system_capabilities: &capability_core::SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    intent: YtDlpCandidateOpenIntent,
    cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedYtDlpWebMedia> {
    ensure_not_cancelled(&is_cancelled)?;
    let component_selection_intent = intent.component_selection_intent();
    let selection_preference = match &intent {
        YtDlpCandidateOpenIntent::BestPlayable => {
            crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
                yt_dlp_config,
            )
        }
        YtDlpCandidateOpenIntent::Exact(exact) => exact.preference,
        YtDlpCandidateOpenIntent::Composed(composed) => composed.preference,
    };
    let (candidate_snapshot, resolved_intent) =
        preparation::resolve_candidate_snapshot(locator, yt_dlp_config, intent, &is_cancelled)
            .context("Не удалось подготовить exact YtDlp candidate snapshot")?;
    let planning_snapshot = candidate_snapshot
        .planning_snapshot()
        .context("Не удалось выразить YtDlp candidates через playback planner")?;
    candidate_snapshot
        .validate_planning_snapshot_alignment(&planning_snapshot)
        .context("YtDlp service/planner candidate snapshots не соответствуют друг другу")?;
    let runtime = WebOpenRuntime::new(network_config, demux_config)
        .context("Не удалось собрать web-media runtime registries")?;
    let policy = selection_policy(yt_dlp_config, preferred_video_codec_order)
        .context("Не удалось собрать YtDlp playback selection policy")?;
    let capabilities = PlaybackCapabilitySnapshot::new(
        &runtime.transport_capabilities,
        &runtime.demux_capabilities,
        system_capabilities,
        audio_capabilities,
    );
    let planning_candidate_count = planning_snapshot.candidates().len();
    let normalization_rejection_count = candidate_snapshot
        .inventory()
        .iter()
        .filter(|entry| entry.rejected().is_some())
        .count()
        + usize::from(
            candidate_snapshot
                .selected()
                .is_some_and(|entry| entry.rejected().is_some()),
        );
    let playlist_metadata = candidate_snapshot.playlist_metadata().clone();
    ensure_not_cancelled(&is_cancelled)?;
    let mut catalog_capability_probe = catalog_capabilities::AppCatalogCapabilityProbe::new(
        system_capabilities.clone(),
        audio_capabilities,
    );
    let mut attempt_context = content_probe_fallback::CandidateAttemptContext {
        locator,
        network_config,
        yt_dlp_config,
        candidate_snapshot: &candidate_snapshot,
        runtime: &runtime,
        component_selection_intent: &component_selection_intent,
        preferred_height: crate::web_media_quality::preferred_height_policy(
            yt_dlp_config.preferred_video_height,
        ),
        cancellation: &cancellation,
        is_cancelled: &is_cancelled,
        playback_policy: &policy,
        catalog_capability_probe: &mut catalog_capability_probe,
    };
    let (candidate_selection, composed_selection, opened_candidate) = match resolved_intent {
        preparation::ResolvedCandidateIntent::Planner(SelectionRequest::BestPlayable) => {
            let ranked = content_probe_fallback::ranked_best_playable_candidates(
                &candidate_snapshot,
                &planning_snapshot,
                capabilities,
                &policy,
            )
            .with_context(|| {
                format!(
                    "YtDlp planner не нашёл playable candidate (planning_candidates={planning_candidate_count}, normalization_rejections={normalization_rejection_count})"
                )
            })?;
            let (_, opened_attempt) =
                content_probe_fallback::open_ranked_best(ranked, &is_cancelled, |candidate| {
                    let selection = candidate_snapshot
                        .selection_for(candidate)
                        .context("Planner-ranked YtDlp candidate не имеет exact selection")?;
                    attempt_context.open(candidate, selection)
                })
                .context("Не удалось открыть planner-ranked BestPlayable YtDlp candidate")?;
            let (selection, opened) = opened_attempt.into_parts();
            (selection, None, opened)
        }
        preparation::ResolvedCandidateIntent::Planner(
            selection_request @ SelectionRequest::Exact(_),
        ) => {
            let outcome = plan_playback(
                &planning_snapshot,
                capabilities,
                &selection_request,
                &policy,
            )
            .map_err(|error| {
                let safe_summary = error.safe_summary();
                anyhow::Error::new(error).context(format!(
                    "YtDlp planner не нашёл exact playable candidate (planning_candidates={planning_candidate_count}, normalization_rejections={normalization_rejection_count}, {safe_summary})"
                ))
            })?;
            let selected = candidate_snapshot
                .canonical_candidate_for_planning_identity(
                    outcome.selected().exact_identity(),
                    outcome.selected().semantic_identity(),
                )
                .ok_or_else(|| anyhow!("planner выбрал отсутствующий YtDlp candidate"))?;
            let selection = candidate_snapshot
                .selection_for(selected)
                .context("Exact YtDlp candidate не имеет exact selection")?;
            let opened_attempt = content_probe_fallback::open_single(selected, |candidate| {
                attempt_context.open(candidate, selection)
            })
            .context("Не удалось открыть exact YtDlp candidate")?;
            let (selection, opened) = opened_attempt.into_parts();
            (selection, None, opened)
        }
        preparation::ResolvedCandidateIntent::Composed {
            candidate,
            selection,
            parent_preference,
        } => {
            let opened_attempt =
                content_probe_fallback::open_single(candidate.as_ref(), |candidate| {
                    attempt_context.open(candidate, *parent_preference)
                })
                .context("Не удалось открыть composed YtDlp candidate")?;
            let (candidate_selection, opened) = opened_attempt.into_parts();
            (candidate_selection, Some(selection), opened)
        }
    };
    let stream_configuration =
        crate::web_media_stream_model::WebMediaStreamConfiguration::from_yt_dlp_snapshot(
            &candidate_snapshot,
            &planning_snapshot,
            capabilities,
            &policy,
            &candidate_selection,
            selection_preference,
        )
        .context("Не удалось построить secret-safe URL sidebar stream model")?;
    let stream_configuration =
        stream_configuration.with_hls_subtitle_renditions(opened_candidate.subtitles);
    ensure_not_cancelled(&is_cancelled)?;
    let stream_configuration = finalize_component_variant_configuration(
        stream_configuration,
        component_selection_intent.clone(),
        opened_candidate.component_variants,
    )
    .context("Не удалось финализировать fresh component variant configuration")?;
    ensure_not_cancelled(&is_cancelled)?;
    let catalog_attachment = catalog::catalog_attachment(catalog::CatalogAttachmentRequest {
        candidate_snapshot: &candidate_snapshot,
        planning_snapshot: &planning_snapshot,
        capabilities,
        policy: &policy,
        active_selection: &candidate_selection,
        active_composed: composed_selection.as_deref(),
    })?;
    Ok(PreparedYtDlpWebMedia {
        demuxer: opened_candidate.demuxer,
        playlist_metadata,
        candidate_selection,
        composed_selection,
        stream_configuration,
        catalog_attachment,
        timeline_port: opened_candidate.timeline_port,
        demux_seek_port: opened_candidate.demux_seek_port,
        playback_window: opened_candidate.playback_window,
    })
}

/// Держит concrete providers/factories и immutable capability snapshots одного attempt-а.
struct WebOpenRuntime {
    /// Единственный S22 transport registry.
    transport_registry: TransportRegistry,
    /// Единственный neutral demux registry.
    demux_registry: Arc<DemuxRegistry>,
    /// HLS-only TS/fMP4 ordered-segment registry.
    hls_demux_registry: Arc<DemuxRegistry>,
    /// Provider capabilities для pure planner-а.
    transport_capabilities: TransportCapabilitySnapshot,
    /// Factory capabilities для pure planner-а.
    demux_capabilities: DemuxCapabilitySnapshot,
    /// Exact HTTP provider ID нужен service-owned neutral request adapter-у.
    provider_id: web_media_transport_api::TransportProviderId,
    /// Exact FTP provider ID для progressive FTP candidates.
    ftp_provider_id: web_media_transport_api::TransportProviderId,
    /// Validated source policy нужна bounded sniff deadline.
    source_config: SourceRuntimeConfig,
    /// Caller config нужен для named adaptive RAM budgets.
    network_config: NetworkConfig,
    /// Existing prefetch policy переиспользуется для readiness limits.
    prefetch_config: media_prefetch::PrefetchConfig,
}

impl WebOpenRuntime {
    /// Создаёт registries без network I/O и снимок именно зарегистрированных capabilities.
    fn new(network_config: &NetworkConfig, demux_config: &PlayerDemuxConfig) -> Result<Self> {
        let source_config = SourceRuntimeConfig::from_network_config(network_config)
            .context("Network config нельзя преобразовать в source runtime policy")?;
        let prefetch_config = prefetch_config(network_config)?;
        let provider = WebMediaHttpProvider::new(source_config.clone(), prefetch_config)
            .context("Не удалось создать progressive HTTP provider")?;
        let provider_id = provider.descriptor().provider_id().clone();
        let ftp_provider = WebMediaFtpProvider::new(source_config.clone())
            .context("Не удалось создать progressive FTP provider")?;
        let ftp_provider_id = ftp_provider.descriptor().provider_id().clone();
        let mut transport_registry = TransportRegistry::new();
        transport_registry
            .register(Box::new(provider))
            .context("Не удалось зарегистрировать progressive HTTP provider")?;
        transport_registry
            .register(Box::new(ftp_provider))
            .context("Не удалось зарегистрировать progressive FTP provider")?;

        let demuxer_options = DemuxerOptions::from_max_consecutive_corrupted_packets(
            demux_config.max_consecutive_corrupted_packets,
        )
        .context("Player demux config нарушает validated runtime bounds")?;
        let demux_composition =
            crate::web_media_demux_registry::WebDemuxComposition::new(demuxer_options)
                .context("Не удалось собрать web demux registry")?;
        let hls_demux_composition =
            crate::web_media_demux_registry::WebDemuxComposition::new_hls(demuxer_options)
                .context("Не удалось собрать HLS demux registry")?;
        let demux_capabilities = DemuxCapabilitySnapshot::new(
            demux_composition
                .capabilities
                .registrations()
                .iter()
                .chain(hls_demux_composition.capabilities.registrations())
                .cloned()
                .collect(),
        );

        Ok(Self {
            transport_registry,
            demux_registry: Arc::new(demux_composition.registry),
            hls_demux_registry: Arc::new(hls_demux_composition.registry),
            transport_capabilities: progressive_transport_capabilities()?,
            demux_capabilities,
            provider_id,
            ftp_provider_id,
            source_config,
            network_config: network_config.clone(),
            prefetch_config,
        })
    }

    /// Открывает physical resources candidate-а и проверяет actual demux track shape.
    fn open_candidate(
        &self,
        candidate: &YtDlpNormalizedCandidate,
        context: WebCandidateOpenContext,
        is_cancelled: &impl Fn() -> bool,
        catalog_capability_probe: &mut catalog_capabilities::AppCatalogCapabilityProbe,
        playback_policy: &PlaybackSelectionPolicy,
    ) -> std::result::Result<OpenedWebCandidate, content_probe_fallback::CandidateOpenError> {
        let WebCandidateOpenContext {
            live_intent,
            endpoint_refresh_ports,
            timeline_port_generation,
            component_selection_intent,
            preferred_height,
            catalog_identity,
            cancellation,
        } = context;
        if smooth::candidate_is_smooth(candidate) {
            ensure_not_cancelled(is_cancelled)?;
            let prepared = smooth::prepare_smooth_candidate(
                candidate,
                self.provider_id.clone(),
                &self.source_config,
                &self.network_config,
                Arc::clone(&self.demux_registry),
                cancellation,
                live_intent,
                component_selection_intent,
                preferred_height,
                catalog_identity,
                catalog_capability_probe,
            )?;
            return Ok(OpenedWebCandidate {
                demuxer: prepared.demuxer,
                subtitles: Arc::from([]),
                timeline_port: None,
                demux_seek_port: Some(prepared.seek_port),
                playback_window: None,
                component_variants: prepared.component_variants,
            });
        }
        if hds::candidate_is_hds(candidate) {
            ensure_not_cancelled(is_cancelled)?;
            let StreamLayout::ContentProbed(content_probe_descriptor) =
                candidate.descriptor().layout()
            else {
                return Err(anyhow!("HDS candidate потерял ContentProbed descriptor").into());
            };
            let hds_capability_probe = content_probe::ContentProbedHdsCapabilityProbe::new(
                catalog_capability_probe,
                content_probe_descriptor,
                playback_policy,
            );
            let prepared = hds::prepare_hds_candidate(
                candidate,
                self.provider_id.clone(),
                &self.source_config,
                &self.network_config,
                Arc::clone(&self.demux_registry),
                cancellation,
                live_intent,
                preferred_height,
                component_selection_intent,
                catalog_identity,
                &hds_capability_probe,
            )
            .map_err(|error| {
                if error
                    .downcast_ref::<web_media_hds::HdsNoPlayableRendition>()
                    .is_some()
                {
                    content_probe::ContentProbeRejection::NoPlayableAdaptiveVariant.into()
                } else {
                    content_probe_fallback::CandidateOpenError::from(error)
                }
            })?;
            return Ok(OpenedWebCandidate {
                demuxer: prepared.demuxer,
                subtitles: Arc::from([]),
                timeline_port: None,
                demux_seek_port: Some(prepared.seek_port),
                playback_window: Some(prepared.playback_window),
                component_variants: prepared.component_variants,
            });
        }
        if crate::web_media_hls_open::candidate_is_hls(candidate) {
            ensure_not_cancelled(is_cancelled)?;
            let prepared = crate::web_media_hls_open::prepare_hls_candidate(
                candidate,
                self.provider_id.clone(),
                &self.source_config,
                &self.network_config,
                Arc::clone(&self.hls_demux_registry),
                cancellation,
                live_intent,
                endpoint_refresh_ports.hls,
                timeline_port_generation,
                component_selection_intent,
                catalog_identity,
                catalog_capability_probe,
            )?;
            return Ok(OpenedWebCandidate {
                demuxer: prepared.demuxer,
                subtitles: prepared.subtitles,
                timeline_port: prepared.timeline_port,
                demux_seek_port: Some(prepared.seek_port),
                playback_window: None,
                component_variants: prepared.component_variants,
            });
        }
        if crate::web_media_dash_open::candidate_is_dash(candidate) {
            ensure_not_cancelled(is_cancelled)?;
            let prepared = crate::web_media_dash_open::prepare_dash_candidate(
                candidate,
                self.provider_id.clone(),
                &self.source_config,
                &self.network_config,
                Arc::clone(&self.demux_registry),
                cancellation,
                live_intent,
                endpoint_refresh_ports.dash,
                timeline_port_generation,
                component_selection_intent,
                catalog_identity,
                catalog_capability_probe,
            )?;
            return Ok(OpenedWebCandidate {
                demuxer: prepared.demuxer,
                subtitles: Arc::from([]),
                timeline_port: prepared.timeline_port,
                demux_seek_port: Some(prepared.seek_port),
                playback_window: None,
                component_variants: prepared.component_variants,
            });
        }
        if !matches!(
            live_intent,
            YtDlpLiveIntent::Unspecified | YtDlpLiveIntent::NotLive
        ) {
            return Err(anyhow!(
                "live yt-dlp candidate не имеет совместимого HLS transport profile"
            )
            .into());
        }
        let request_context = YtDlpProgressiveTransportRequestContext::new(
            self.provider_id.clone(),
            self.ftp_provider_id.clone(),
            SourceGeneration::new(INITIAL_TRANSPORT_GENERATION),
            cancellation.clone(),
        );
        let components = candidate
            .progressive_transport_components(&request_context)
            .context("YtDlp request material нельзя выразить через progressive transport")?;
        let mut opened_components = Vec::with_capacity(components.len());
        for component in components {
            ensure_not_cancelled(is_cancelled)?;
            let role = component.role();
            let container = component.container();
            let opened_transport = self
                .transport_registry
                .open(component.into_request())
                .context("Progressive provider не открыл YtDlp component")?;
            let transport_seekability = opened_transport.seekability();
            let demux_input = match opened_transport.into_input() {
                TransportInput::Seekable(source) => DemuxInput::byte_source(source),
                TransportInput::Streaming(source) => {
                    DemuxInput::streaming_source(source, cancellation.clone())
                }
            };
            let demuxer = self.open_demuxer(
                demux_input,
                transport_seekability,
                container,
                cancellation.clone(),
            )?;
            if let StreamLayout::ContentProbed(descriptor) = candidate.descriptor().layout() {
                let proof = content_probe::prove_content_probed_tracks(
                    catalog_capability_probe,
                    descriptor,
                    demuxer.tracks(),
                    playback_policy,
                )?;
                debug_assert!(proof.video().is_some() || proof.audio().is_some());
            }
            validate_component_tracks(role, demuxer.as_ref())?;
            opened_components.push(OpenedCandidateComponent { role, demuxer });
        }
        let demuxer = compose_candidate_components(opened_components)?;
        Ok(OpenedWebCandidate {
            demuxer,
            subtitles: Arc::from([]),
            timeline_port: None,
            demux_seek_port: None,
            playback_window: None,
            component_variants: PreparedComponentVariantCatalog::Unavailable,
        })
    }

    /// Открывает один resource через registry и адаптирует blocking streaming demuxer к readiness.
    fn open_demuxer(
        &self,
        input: DemuxInput,
        seekability: TransportSeekability,
        container: ContainerFamily,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn Demuxer + Send>> {
        let hints = demux_hints(container)?;
        let sniff_bytes = usize::try_from(self.prefetch_config.initial_chunk_bytes())
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or_else(|| {
                anyhow!("prefetch initial chunk нельзя использовать как sniff budget")
            })?;
        let sniff_budget = DemuxSniffBudget::new(
            sniff_bytes,
            NonZeroUsize::MIN,
            self.source_config.read_timeout(),
        )
        .context("Source read timeout нельзя использовать как demux sniff deadline")?;
        let demuxer = self
            .demux_registry
            .open(input, hints, sniff_budget, cancellation.clone())
            .context("Demux registry не открыл YtDlp component")?;
        match seekability {
            TransportSeekability::Seekable => Ok(demuxer),
            TransportSeekability::Streaming => {
                let limits = progressive_limits(self.prefetch_config)?;
                let retry_hint = DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER)
                    .context("Minimum demux retry hint нарушает media-core bounds")?;
                let progressive =
                    ProgressiveDemuxer::new(demuxer, cancellation, limits, retry_hint)
                        .context("Не удалось запустить progressive demux worker")?;
                Ok(Box::new(progressive))
            }
        }
    }
}

/// Открытый physical component до layout composition.
struct OpenedCandidateComponent {
    /// Exact semantic role из selected candidate.
    role: MediaComponentRole,
    /// Уже проверенный concrete demuxer.
    demuxer: Box<dyn Demuxer + Send>,
}

/// Выдаёт новую non-zero process-local source lineage без URL/queue representation coupling.
fn next_source_identity() -> Result<SourceIdentity> {
    let source_value = NEXT_YT_DLP_SOURCE_IDENTITY
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow!("YtDlp source identity space исчерпан"))?;
    Ok(SourceIdentity::new(source_value))
}

/// Строит pure selection policy из committed user config.
fn selection_policy(
    yt_dlp_config: &YtDlpConfig,
    preferred_video_codec_order: &[ConfigVideoCodec],
) -> Result<PlaybackSelectionPolicy> {
    let hdr = match yt_dlp_config.hdr_selection {
        YtDlpHdrSelection::SdrOnly => HdrSelectionPolicy::SdrOnly,
        YtDlpHdrSelection::PreferHdrWhenAvailable => HdrSelectionPolicy::PreferHdrWhenAvailable,
    };
    let codecs = preferred_video_codec_order
        .iter()
        .copied()
        .map(crate::startup_media::runtime_video_codec)
        .collect();
    let containers = vec![
        ContainerFamily::WebM,
        ContainerFamily::IsoBmff,
        ContainerFamily::FragmentedIsoBmff,
        ContainerFamily::Matroska,
        ContainerFamily::Ogg,
        ContainerFamily::Flac,
        ContainerFamily::MpegAudio,
        ContainerFamily::Wav,
        ContainerFamily::Aiff,
        ContainerFamily::Caf,
    ];
    PlaybackSelectionPolicy::new(
        hdr,
        codecs,
        crate::web_media_quality::preferred_height_policy(yt_dlp_config.preferred_video_height),
        containers,
    )
    .map_err(Into::into)
}

/// Объявляет только реальные output shapes зарегистрированных progressive provider-ов.
fn progressive_transport_capabilities() -> Result<TransportCapabilitySnapshot> {
    let outputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
        .with(DemuxInputCapability::StreamingBytes);
    let http = TransportCapabilityRegistration::new(
        TransportFamily::ProgressiveHttp(HttpScheme::Http),
        outputs,
    )?;
    let https = TransportCapabilityRegistration::new(
        TransportFamily::ProgressiveHttp(HttpScheme::Https),
        outputs,
    )?;
    let ftp = TransportCapabilityRegistration::new(
        TransportFamily::ProgressiveFtp(FtpScheme::Ftp),
        outputs,
    )?;
    let ftps = TransportCapabilityRegistration::new(
        TransportFamily::ProgressiveFtp(FtpScheme::Ftps),
        outputs,
    )?;
    let hls = TransportCapabilityRegistration::new(
        TransportFamily::Hls,
        DemuxInputCapabilities::only(crate::web_media_hls_open::hls_transport_input()),
    )?;
    let dash = TransportCapabilityRegistration::new(
        TransportFamily::Dash,
        DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments)
            .with(DemuxInputCapability::SeekableBytes),
    )?;
    let smooth = TransportCapabilityRegistration::new(
        TransportFamily::SmoothStreaming,
        DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments),
    )?;
    let hds = TransportCapabilityRegistration::new(
        TransportFamily::Hds,
        DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments),
    )?;
    Ok(TransportCapabilitySnapshot::new(vec![
        http, https, ftp, ftps, hls, dash, smooth, hds,
    ]))
}

/// Передаёт registry согласованные extension и container hints выбранной family.
fn demux_hints(family: ContainerFamily) -> Result<DemuxHints> {
    let (container_id, extension) = match family {
        ContainerFamily::IsoBmff | ContainerFamily::FragmentedIsoBmff => ("iso-bmff", "mp4"),
        ContainerFamily::Matroska => ("matroska", "mkv"),
        ContainerFamily::WebM => ("webm", "webm"),
        ContainerFamily::Ogg => ("ogg", "ogg"),
        ContainerFamily::Flac => ("flac", "flac"),
        ContainerFamily::Wav => ("wave", "wav"),
        ContainerFamily::Aiff => ("aiff", "aiff"),
        ContainerFamily::Caf => ("caf", "caf"),
        ContainerFamily::MpegAudio => ("mpeg-audio", "mp3"),
        ContainerFamily::Flv => ("flv", "flv"),
        ContainerFamily::F4f => ("f4f", "f4f"),
        _ => bail!("Selected YtDlp container не зарегистрирован в web demux registry"),
    };
    Ok(DemuxHints::none()
        .with_extension(DemuxSourceExtension::new(extension)?)
        .with_container(DemuxContainerId::new(container_id)?))
}

/// Переиспользует network prefetch knobs без второго cache/read-ahead policy.
fn prefetch_config(network_config: &NetworkConfig) -> Result<media_prefetch::PrefetchConfig> {
    let initial_chunk_bytes = network_config
        .prefetch_initial_chunk_kb
        .checked_mul(KIB_BYTES)
        .ok_or_else(|| anyhow!("network.prefetch_initial_chunk_kb overflow"))?;
    let chunk_bytes = network_config
        .prefetch_chunk_mb
        .checked_mul(MIB_BYTES)
        .ok_or_else(|| anyhow!("network.prefetch_chunk_mb overflow"))?;
    let window_bytes = network_config
        .read_ahead_mb
        .checked_mul(MIB_BYTES)
        .ok_or_else(|| anyhow!("network.read_ahead_mb overflow"))?;
    media_prefetch::PrefetchConfig::new(initial_chunk_bytes, chunk_bytes, window_bytes)
        .map_err(Into::into)
}

/// Делит existing prefetch RAM window на bounded progressive readiness slots.
fn progressive_limits(
    prefetch_config: media_prefetch::PrefetchConfig,
) -> Result<ProgressiveDemuxBufferLimits> {
    let event_capacity = prefetch_config
        .window_bytes()
        .div_ceil(prefetch_config.chunk_bytes());
    let event_capacity = usize::try_from(event_capacity)
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| anyhow!("prefetch window нельзя преобразовать в event capacity"))?;
    let encoded_byte_capacity = usize::try_from(prefetch_config.window_bytes())
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| anyhow!("prefetch window нельзя преобразовать в byte capacity"))?;
    Ok(ProgressiveDemuxBufferLimits::new(
        event_capacity,
        encoded_byte_capacity,
    ))
}

/// Проверяет actual tracks, чтобы descriptor shape не подменял runtime evidence.
fn validate_component_tracks(role: MediaComponentRole, demuxer: &dyn Demuxer) -> Result<()> {
    let has_video = selected_track(demuxer, TrackKind::Video).is_some();
    let has_audio = selected_track(demuxer, TrackKind::Audio).is_some();
    let shape_matches = match role {
        MediaComponentRole::Muxed => has_video && has_audio,
        MediaComponentRole::ContentProbed => has_video || has_audio,
        MediaComponentRole::Video => has_video,
        MediaComponentRole::Audio => has_audio,
        MediaComponentRole::Subtitle | MediaComponentRole::PresentationManifest => false,
    };
    if !shape_matches {
        bail!("Opened YtDlp component tracks не совпадают с selected descriptor role");
    }
    Ok(())
}

/// Композирует single layout либо ровно одну separate video/audio пару.
fn compose_candidate_components(
    mut components: Vec<OpenedCandidateComponent>,
) -> Result<Box<dyn Demuxer + Send>> {
    if components.len() == 1 {
        return Ok(components.remove(0).demuxer);
    }
    if components.len() != 2 {
        bail!("YtDlp candidate содержит неподдерживаемое число physical components");
    }
    let video_index = components
        .iter()
        .position(|component| component.role == MediaComponentRole::Video)
        .ok_or_else(|| anyhow!("Separate YtDlp candidate не содержит video component"))?;
    let video = components.remove(video_index).demuxer;
    let audio_index = components
        .iter()
        .position(|component| component.role == MediaComponentRole::Audio)
        .ok_or_else(|| anyhow!("Separate YtDlp candidate не содержит audio component"))?;
    let audio = components.remove(audio_index).demuxer;
    let video_track = selected_track(video.as_ref(), TrackKind::Video)
        .ok_or_else(|| anyhow!("Separate video demuxer не содержит video track"))?;
    let audio_track = selected_track(audio.as_ref(), TrackKind::Audio)
        .ok_or_else(|| anyhow!("Separate audio demuxer не содержит audio track"))?;
    let lead_policy = progressive_composite_lead_policy()?;
    let composite = CompositeAvDemuxer::new(
        video,
        audio,
        CompositeAvTrackSelection::new(video_track, audio_track),
        lead_policy,
    )
    .context("Не удалось скомпоновать separate YtDlp A/V demuxers")?;
    Ok(Box::new(composite))
}

/// Держит encoded-packet bound независимым от transport sniff/bootstrap настройки.
fn progressive_composite_lead_policy() -> Result<CompositeComponentLeadPolicy> {
    CompositeComponentLeadPolicy::single_pending_packet(
        COMPOSITE_MAX_TIMESTAMP_LEAD,
        NonZeroUsize::new(COMPOSITE_MAX_PENDING_PACKET_BYTES)
            .expect("composite pending packet limit ненулевой"),
    )
    .context("Progressive composite A/V safety bounds invalid")
}

/// Возвращает первый track нужного kind-а как explicit composition selection.
fn selected_track(demuxer: &dyn Demuxer, kind: TrackKind) -> Option<TrackId> {
    demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == kind)
        .map(|track| track.id)
}

/// Проверяет caller-owned cancellation между потенциально дорогими boundaries.
fn ensure_not_cancelled(is_cancelled: &impl Fn() -> bool) -> Result<()> {
    if is_cancelled() {
        bail!("YtDlp web media preparation отменена");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HTTP sniff chunk не должен становиться случайным лимитом encoded video packet-а.
    #[test]
    fn progressive_composite_packet_limit_is_independent_from_sniff_chunk() {
        let lead_policy =
            progressive_composite_lead_policy().expect("progressive composite policy валидна");

        assert_eq!(
            lead_policy.bootstrap_byte_limit(),
            COMPOSITE_MAX_PENDING_PACKET_BYTES
        );
        assert!(lead_policy.bootstrap_byte_limit() > 64 * 1024);
    }
    use demux_api::{
        DemuxContainerRegistration, DemuxFactoryDescriptor, DemuxFactoryId, DemuxFixtureId,
    };

    /// Shutdown cancellation завершается до запуска extractor/network side effects.
    #[test]
    fn cancellation_is_a_pre_barrier_failure() {
        let locator =
            service_ytdlp::parse_yt_dlp_media_locator("https://media.example.test/watch?id=secret")
                .expect("valid test locator");
        let result = prepare_yt_dlp_web_media(
            &locator,
            &NetworkConfig::default(),
            &YtDlpConfig::default(),
            &PlayerDemuxConfig::default(),
            &[ConfigVideoCodec::Vp9],
            &capability_core::SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
            YtDlpCandidateOpenIntent::BestPlayable,
            CancellationToken::new(),
            || true,
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("cancelled preparation не должна запускать yt-dlp"),
        };
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("отменена"));
        assert!(!diagnostic.contains("secret"));
    }

    /// Concrete Symphonia descriptor и planner snapshot не расходятся по S22 containers.
    #[test]
    fn demux_capability_snapshot_is_derived_from_registered_factory() {
        use demux_api::DemuxFactory;

        let factory = symphonia_demux::SymphoniaDemuxFactory::new(DemuxerOptions::default())
            .expect("Symphonia factory");
        let capabilities =
            crate::web_media_demux_registry::capabilities_for_descriptors([factory.descriptor()])
                .expect("capability snapshot");
        let expected_inputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes);
        for family in [
            ContainerFamily::IsoBmff,
            ContainerFamily::FragmentedIsoBmff,
            ContainerFamily::Matroska,
            ContainerFamily::WebM,
            ContainerFamily::Ogg,
            ContainerFamily::Flac,
            ContainerFamily::Wav,
            ContainerFamily::Aiff,
            ContainerFamily::Caf,
            ContainerFamily::MpegAudio,
        ] {
            let expected_family_inputs = if matches!(
                family,
                ContainerFamily::IsoBmff
                    | ContainerFamily::FragmentedIsoBmff
                    | ContainerFamily::Matroska
                    | ContainerFamily::WebM
            ) {
                expected_inputs.with(DemuxInputCapability::OrderedSegments)
            } else {
                expected_inputs
            };
            assert_eq!(
                capabilities.input_capabilities_for(family),
                expected_family_inputs,
                "family {family:?} должна получить exact registered inputs"
            );
        }
    }

    /// Planner projection не переносит capability между соседними container rows.
    #[test]
    fn demux_capability_snapshot_preserves_per_container_input_sets() {
        let iso_inputs = DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments);
        let webm_inputs = DemuxInputCapabilities::only(DemuxInputCapability::StreamingBytes);
        let descriptor = DemuxFactoryDescriptor::new(
            DemuxFactoryId::new("synthetic-per-container").expect("factory ID"),
            vec![
                DemuxContainerRegistration::new(
                    DemuxContainerId::new("iso-bmff").expect("ISO BMFF container ID"),
                    iso_inputs,
                    vec![],
                    vec![],
                ),
                DemuxContainerRegistration::new(
                    DemuxContainerId::new("webm").expect("WebM container ID"),
                    webm_inputs,
                    vec![],
                    vec![],
                ),
            ],
            vec![DemuxFixtureId::new("synthetic/per-container").expect("fixture ID")],
        );

        let capabilities =
            crate::web_media_demux_registry::capabilities_for_descriptors([&descriptor])
                .expect("capability snapshot");
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::IsoBmff),
            iso_inputs
        );
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::FragmentedIsoBmff),
            iso_inputs
        );
        assert_eq!(
            capabilities.input_capabilities_for(ContainerFamily::WebM),
            webm_inputs
        );
        assert!(
            !capabilities
                .input_capabilities_for(ContainerFamily::WebM)
                .contains(DemuxInputCapability::OrderedSegments),
            "WebM не должен наследовать synthetic ISO ordered capability"
        );
    }

    /// Component catalog generation монотонна и fail-closed при исчерпании.
    #[test]
    fn component_variant_catalog_generation_is_monotonic_and_overflow_checked() {
        let allocator = AtomicU64::new(41);

        assert_eq!(
            preparation::allocate_component_variant_catalog_generation(&allocator)
                .expect("first catalog generation")
                .value(),
            41
        );
        assert_eq!(
            preparation::allocate_component_variant_catalog_generation(&allocator)
                .expect("second catalog generation")
                .value(),
            42
        );

        allocator.store(u64::MAX, Ordering::Relaxed);
        assert!(
            preparation::allocate_component_variant_catalog_generation(&allocator).is_err(),
            "исчерпанный allocator не должен оборачивать catalog generation"
        );
    }

    /// Planner видит exact adaptive input shapes только после concrete runtimes.
    #[test]
    fn transport_capability_snapshot_advertises_dash_ordered_and_range_inputs() {
        let capabilities =
            progressive_transport_capabilities().expect("transport capability snapshot builds");
        let dash_inputs = capabilities.output_inputs_for(TransportFamily::Dash);

        assert_eq!(
            dash_inputs,
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments)
                .with(DemuxInputCapability::SeekableBytes)
        );
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::Hls),
            DemuxInputCapabilities::only(crate::web_media_hls_open::hls_transport_input()),
            "DASH registration не должна менять соседний HLS provider"
        );
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::SmoothStreaming),
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments),
            "S36 Smooth runtime должен рекламировать только реально используемый ordered input"
        );
        let progressive_outputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes);
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::ProgressiveFtp(FtpScheme::Ftp)),
            progressive_outputs,
            "S37 FTP runtime должен рекламировать seekable и streaming byte inputs"
        );
        assert_eq!(
            capabilities.output_inputs_for(TransportFamily::ProgressiveFtp(FtpScheme::Ftps)),
            progressive_outputs,
            "S37 FTPS runtime должен рекламировать seekable и streaming byte inputs"
        );
    }
}
