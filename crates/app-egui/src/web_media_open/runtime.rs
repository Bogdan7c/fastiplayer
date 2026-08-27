//! Concrete transport/demux runtime одного yt-dlp candidate preparation attempt-а.
//!
//! Parent сохраняет extraction, exact identity/generation, cancellation fences,
//! component composition и pre-publish strong-install gates. Этот owner только
//! строит registries/capability snapshots и открывает уже выбранные resources.

use std::num::NonZeroUsize;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use demux_api::{
    DemuxContainerId, DemuxHints, DemuxInput, DemuxInputCapabilities, DemuxInputCapability,
    DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension, ProgressiveDemuxBufferLimits,
    ProgressiveDemuxer,
};
use media_core::{DemuxRetryHint, Demuxer};
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use service_ytdlp::{
    YtDlpLiveIntent, YtDlpNormalizedCandidate, YtDlpProgressiveTransportRequestContext,
};
use source_core::{CancellationToken, SourceRuntimeConfig};
use symphonia_demux::DemuxerOptions;
use web_media_core::{ContainerFamily, FtpScheme, HttpScheme, StreamLayout, TransportFamily};
use web_media_ftp::WebMediaFtpProvider;
use web_media_http::WebMediaHttpProvider;
use web_media_playback_plan::{
    DemuxCapabilitySnapshot, PlaybackCapabilitySnapshot, PlaybackSelectionPolicy,
    TransportCapabilityRegistration, TransportCapabilitySnapshot,
};
use web_media_transport_api::{
    SourceGeneration, TransportInput, TransportProvider, TransportRegistry, TransportSeekability,
};

use super::component_variants::PreparedComponentVariantCatalog;
use super::{
    OpenedCandidateComponent, OpenedWebCandidate, WebCandidateOpenContext, catalog_capabilities,
    compose_candidate_components, content_probe, content_probe_fallback, ensure_not_cancelled, hds,
    smooth, validate_component_tracks,
};

/// Один кибибайт в bytes для checked config conversion.
const KIB_BYTES: u64 = 1024;
/// Один мебибайт в bytes для checked config conversion.
const MIB_BYTES: u64 = KIB_BYTES * 1024;
/// Runtime generation первого transport open-а внутри одного preparation attempt-а.
const INITIAL_TRANSPORT_GENERATION: u64 = 1;

/// Держит concrete providers/factories и immutable capability snapshots одного attempt-а.
pub(super) struct WebOpenRuntime {
    /// Единственный S22 transport registry.
    transport_registry: TransportRegistry,
    /// Единственный neutral demux registry.
    demux_registry: Arc<DemuxRegistry>,
    /// HLS-only TS/fMP4 ordered-segment registry.
    hls_demux_registry: Arc<DemuxRegistry>,
    /// Provider capabilities для pure planner-а.
    pub(super) transport_capabilities: TransportCapabilitySnapshot,
    /// Factory capabilities для pure planner-а.
    pub(super) demux_capabilities: DemuxCapabilitySnapshot,
    /// Exact HTTP provider ID нужен service-owned neutral request adapter-у.
    pub(super) provider_id: web_media_transport_api::TransportProviderId,
    /// Exact FTP provider ID для progressive FTP candidates.
    ftp_provider_id: web_media_transport_api::TransportProviderId,
    /// Validated source policy нужна bounded sniff deadline.
    pub(super) source_config: SourceRuntimeConfig,
    /// Caller config нужен для named adaptive RAM budgets.
    network_config: NetworkConfig,
    /// Existing prefetch policy переиспользуется для readiness limits.
    prefetch_config: media_prefetch::PrefetchConfig,
}

impl WebOpenRuntime {
    /// Создаёт registries без network I/O и снимок именно зарегистрированных capabilities.
    pub(super) fn new(
        network_config: &NetworkConfig,
        demux_config: &PlayerDemuxConfig,
    ) -> Result<Self> {
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
        let hls_transport_limits =
            crate::web_media_adaptive_config::adaptive_transport_limits(network_config)
                .context("Network config нельзя преобразовать в HLS transport limits")?;
        let hls_mpeg_ts_options = mpeg_ts_demux::MpegTsDemuxOptions::default()
            .with_initial_probe_byte_budget(hls_transport_limits.maximum_segment_bytes);
        let hls_demux_composition = crate::web_media_demux_registry::WebDemuxComposition::new_hls(
            demuxer_options,
            hls_mpeg_ts_options,
        )
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

    /// Связывает planner только с immutable snapshots реально собранных runtime owners.
    pub(super) fn playback_capabilities<'runtime>(
        &'runtime self,
        system_capabilities: &'runtime capability_core::SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    ) -> PlaybackCapabilitySnapshot<'runtime> {
        PlaybackCapabilitySnapshot::new(
            &self.transport_capabilities,
            &self.demux_capabilities,
            system_capabilities,
            audio_capabilities,
        )
    }

    /// Открывает physical resources candidate-а и проверяет actual demux track shape.
    pub(super) fn open_candidate(
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
            vod_endpoint_recovery,
        } = context;
        let endpoint_expiry_observer = vod_endpoint_recovery
            .as_ref()
            .map(crate::web_media_vod_recovery::VodEndpointRecoveryAttachment::observer);
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
                endpoint_expiry_observer.clone(),
            )?;
            return Ok(OpenedWebCandidate {
                demuxer: prepared.demuxer,
                subtitles: Arc::from([]),
                timeline_port: None,
                demux_seek_port: Some(prepared.seek_port),
                playback_window: None,
                component_variants: prepared.component_variants,
                vod_endpoint_recovery,
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
                endpoint_expiry_observer.clone(),
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
                vod_endpoint_recovery,
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
                endpoint_expiry_observer.clone(),
            )?;
            return Ok(OpenedWebCandidate {
                demuxer: prepared.demuxer,
                subtitles: prepared.subtitles,
                timeline_port: prepared.timeline_port,
                demux_seek_port: Some(prepared.seek_port),
                playback_window: None,
                component_variants: prepared.component_variants,
                vod_endpoint_recovery,
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
                endpoint_expiry_observer.clone(),
            )?;
            return Ok(OpenedWebCandidate {
                demuxer: prepared.demuxer,
                subtitles: Arc::from([]),
                timeline_port: prepared.timeline_port,
                demux_seek_port: Some(prepared.seek_port),
                playback_window: None,
                component_variants: prepared.component_variants,
                vod_endpoint_recovery,
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
            let mut transport_request = component.into_request();
            if let Some(observer) = endpoint_expiry_observer.clone() {
                transport_request = transport_request.with_endpoint_expiry_observer(observer);
            }
            let opened_transport = self
                .transport_registry
                .open(transport_request)
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
            vod_endpoint_recovery,
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

/// Объявляет только реальные output shapes зарегистрированных progressive provider-ов.
pub(super) fn progressive_transport_capabilities() -> Result<TransportCapabilitySnapshot> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Planner view строится из тех же concrete registries, которыми runtime реально открывает media.
    #[test]
    fn runtime_capabilities_match_registered_transport_and_demux_owners() {
        let runtime = WebOpenRuntime::new(&NetworkConfig::default(), &PlayerDemuxConfig::default())
            .expect("web runtime registries");
        let system_capabilities = capability_core::SystemCapabilities::empty(1);
        let capabilities = runtime.playback_capabilities(
            &system_capabilities,
            audio::AudioDecodeCapabilitySnapshot::empty(),
        );

        assert_eq!(
            capabilities
                .transport()
                .output_inputs_for(TransportFamily::Dash),
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments)
                .with(DemuxInputCapability::SeekableBytes)
        );
        assert!(
            capabilities
                .demux()
                .input_capabilities_for(ContainerFamily::WebM)
                .contains(DemuxInputCapability::StreamingBytes),
            "зарегистрированный progressive demux должен принимать streaming WebM"
        );
    }
}
