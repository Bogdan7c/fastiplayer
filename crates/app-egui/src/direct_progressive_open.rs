//! App-owned neutral runtime для stable progressive HTTP(S)/FTP(S) resource-ов.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use demux_api::{
    DemuxHints, DemuxInput, DemuxSniffBudget, DemuxSourceExtension, ProgressiveDemuxBufferLimits,
    ProgressiveDemuxer,
};
use media_core::{DemuxRetryHint, Demuxer};
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use source_core::{CancellationToken, HttpPathScope, SourceRuntimeConfig};
use symphonia_demux::{DemuxerOptions, MediaMetadata, TrackInfo};
use tracing::debug;
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_ftp::WebMediaFtpProvider;
use web_media_http::WebMediaHttpProvider;
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration, TransportInput,
    TransportOpenRequest, TransportProvider, TransportRegistry, TransportRequestTarget,
    TransportSeekability,
};

/// Один кибибайт в bytes для явной конвертации пользовательского config-а.
const KIB_BYTES: u64 = 1024;
/// Один мебибайт в bytes для явной конвертации пользовательского config-а.
const MIB_BYTES: u64 = KIB_BYTES * 1024;
/// Direct redirect chain остаётся bounded без отдельного скрытого config-а.
const DIRECT_REDIRECT_HOP_LIMIT: u8 = 10;
/// Первая transport generation стабильного root resource-а.
const INITIAL_SOURCE_GENERATION: u64 = 1;
/// Descriptor generation process-local direct selection-а.
const DIRECT_DESCRIPTOR_GENERATION: u64 = 1;
/// Process-local source identity не содержит locator или его hash.
static NEXT_DIRECT_SOURCE_IDENTITY: AtomicU64 = AtomicU64::new(1);

/// App-owned результат direct open до общей prepared-media composition boundary.
pub(crate) struct DirectProgressiveOpenResult {
    /// Безопасный label без userinfo/path/query/fragment.
    source_label: String,
    /// Demuxer, готовый к передаче player owner-у.
    demuxer: Box<dyn Demuxer + Send>,
    /// Stable-resource recovery gate, armed только после полного direct open-а.
    endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
    /// Snapshot треков до перемещения demuxer-а.
    tracks: Vec<TrackInfo>,
    /// Snapshot container duration.
    duration: Option<Duration>,
}

impl DirectProgressiveOpenResult {
    /// Возвращает safe source label.
    #[must_use]
    pub(crate) fn source_label(&self) -> &str {
        &self.source_label
    }

    /// Возвращает track snapshot.
    #[must_use]
    pub(crate) fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Возвращает duration snapshot.
    #[must_use]
    pub(crate) const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Читает metadata до передачи demuxer-а player owner-у.
    #[must_use]
    pub(crate) fn media_metadata(&self) -> Option<MediaMetadata> {
        self.demuxer.media_metadata()
    }

    /// Передаёт demuxer и Installed recovery attachment общему composition owner-у.
    #[must_use]
    pub(crate) fn into_runtime_parts(
        self,
    ) -> (
        Box<dyn Demuxer + Send>,
        crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
    ) {
        (self.demuxer, self.endpoint_recovery)
    }
}

/// Классифицирует locator по capability rows production web demux registry.
pub(crate) fn classify_direct_media_url(
    argument: &str,
) -> Result<service_direct_media::DirectMediaUrl, service_direct_media::DirectMediaOpenError> {
    let composition =
        crate::web_media_demux_registry::WebDemuxComposition::new(DemuxerOptions::default())
            .expect("production web demux registrations должны собираться из static identities");
    service_direct_media::parse_direct_media_url(argument, &composition.registry)
}

/// Открывает stable resource через общие transport/demux registries без extractor-а.
pub(crate) fn open_direct_media(
    locator: &service_direct_media::DirectMediaUrl,
    network_config: &NetworkConfig,
    demux_config: &PlayerDemuxConfig,
    cancellation: CancellationToken,
) -> Result<DirectProgressiveOpenResult> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .context("Network config нельзя преобразовать в source runtime policy")?;
    let prefetch_config = prefetch_config(network_config)?;
    let mut transport_registry = TransportRegistry::new();

    let http_provider = WebMediaHttpProvider::new(source_config.clone(), prefetch_config)
        .context("Не удалось создать progressive HTTP provider")?;
    let http_provider_id = http_provider.descriptor().provider_id().clone();
    transport_registry
        .register(Box::new(http_provider))
        .context("Не удалось зарегистрировать progressive HTTP provider")?;

    let ftp_provider = WebMediaFtpProvider::new(source_config.clone())
        .context("Не удалось создать progressive FTP provider")?;
    let ftp_provider_id = ftp_provider.descriptor().provider_id().clone();
    transport_registry
        .register(Box::new(ftp_provider))
        .context("Не удалось зарегистрировать progressive FTP provider")?;

    let request_target = locator.request_target_for_open();
    let component = build_direct_component_identity()?;
    let endpoint_recovery = crate::web_media_vod_recovery::VodEndpointRecoveryAttachment::new();
    let request = match request_target {
        TransportRequestTarget::Http(target) => {
            let secret_scope =
                SecretRequestScope::from_target(&target, HttpPathScope::from_target_path(&target));
            let redirect_limit = RedirectHopLimit::new(DIRECT_REDIRECT_HOP_LIMIT)
                .context("Direct redirect limit нарушает transport bounds")?;
            TransportOpenRequest::new(
                http_provider_id,
                component,
                target,
                MediaPresentation::Vod,
                SourceGeneration::new(INITIAL_SOURCE_GENERATION),
                SecretRequestContext::builder(secret_scope).build(),
                RedirectPolicy::cross_origin_without_secrets(redirect_limit),
                cancellation.clone(),
            )
            .context("Direct HTTP request нарушает secret-scope contract")?
            .with_endpoint_expiry_observer(endpoint_recovery.observer())
        }
        TransportRequestTarget::Ftp(target) => TransportOpenRequest::for_ftp(
            ftp_provider_id,
            component,
            target,
            MediaPresentation::Vod,
            SourceGeneration::new(INITIAL_SOURCE_GENERATION),
            cancellation.clone(),
        )
        .context("Direct FTP request нарушает no-HTTP-material contract")?
        .with_endpoint_expiry_observer(endpoint_recovery.observer()),
    };

    let opened_transport = transport_registry
        .open(request)
        .with_context(|| format!("Progressive transport не открыл {locator}"))?;
    let transport_seekability = opened_transport.seekability();
    let demux_input = match opened_transport.into_input() {
        TransportInput::Seekable(source) => DemuxInput::byte_source(source),
        TransportInput::Streaming(source) => {
            DemuxInput::streaming_source(source, cancellation.clone())
        }
    };

    let demuxer_options = DemuxerOptions::from_max_consecutive_corrupted_packets(
        demux_config.max_consecutive_corrupted_packets,
    )
    .context("Player demux config нарушает validated runtime bounds")?;
    let demux_composition =
        crate::web_media_demux_registry::WebDemuxComposition::new(demuxer_options)
            .context("Не удалось собрать production web demux registry")?;
    let hints = DemuxHints::none().with_extension(
        DemuxSourceExtension::new(locator.extension().as_extension_hint())
            .context("Service вернул некорректную direct extension identity")?,
    );
    let sniff_budget = direct_sniff_budget(&source_config, prefetch_config)?;
    let demuxer = demux_composition
        .registry
        .open(demux_input, hints, sniff_budget, cancellation.clone())
        .with_context(|| format!("Demux registry не открыл {locator}"))?;
    let demuxer: Box<dyn Demuxer + Send> = match transport_seekability {
        TransportSeekability::Seekable => demuxer,
        TransportSeekability::Streaming => {
            let limits = progressive_limits(prefetch_config)?;
            let retry_hint = DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER)
                .context("Minimum demux retry hint нарушает media-core bounds")?;
            Box::new(
                ProgressiveDemuxer::new(demuxer, cancellation, limits, retry_hint)
                    .context("Не удалось запустить progressive demux worker")?,
            )
        }
    };
    endpoint_recovery.arm_after_candidate_finalization();
    let demuxer = endpoint_recovery.wrap_demuxer(demuxer);

    debug!(
        source = %locator,
        extension = locator.extension().as_extension_hint(),
        seekability = ?demuxer.seekability(),
        "Direct media открыт через neutral HTTP/FTP progressive ingress"
    );

    Ok(DirectProgressiveOpenResult {
        source_label: locator.safe_label().to_owned(),
        tracks: demuxer.tracks().to_vec(),
        duration: demuxer.duration(),
        demuxer,
        endpoint_recovery,
    })
}

/// Создаёт process-local exact/semantic identity без locator payload.
fn build_direct_component_identity() -> Result<MediaComponentIdentity> {
    let source_value = NEXT_DIRECT_SOURCE_IDENTITY
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow!("direct source identity space исчерпан"))?;
    let source = SourceIdentity::new(source_value);
    let exact_format = CandidateFormatIdentity::new("direct-resource")
        .context("Static direct format identity некорректна")?;
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(DIRECT_DESCRIPTOR_GENERATION),
        exact_format,
    );
    let semantic = SemanticIdentity::new(source, "direct-resource")
        .context("Static direct semantic identity некорректна")?;
    MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .context("Direct component identities имеют разный source lineage")
}

/// Строит existing prefetch policy без второго cache/read-ahead config-а.
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

/// Выводит bounded sniff policy из уже validated source/prefetch policy.
fn direct_sniff_budget(
    source_config: &SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
) -> Result<DemuxSniffBudget> {
    let max_bytes = usize::try_from(prefetch_config.initial_chunk_bytes())
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| anyhow!("prefetch initial chunk нельзя использовать как sniff budget"))?;
    DemuxSniffBudget::new(max_bytes, NonZeroUsize::MIN, source_config.read_timeout())
        .context("Source read timeout нельзя использовать как demux sniff deadline")
}

/// Делит existing RAM window между bounded progressive event slots.
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
