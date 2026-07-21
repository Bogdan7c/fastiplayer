//! Direct-media adapter поверх neutral transport и demux registries.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

use demux_api::{
    DemuxContainerId, DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget,
    DemuxSourceExtension, ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
};
use media_core::{DemuxRetryHint, Demuxer};
use source_core::{CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};
use tracing::debug;
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_http::WebMediaHttpProvider;
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration, TransportInput,
    TransportOpenRequest, TransportProvider, TransportRegistry, TransportSeekability,
};

use crate::{DirectMediaExtension, DirectMediaOpenError, DirectMediaOpenResult, DirectMediaUrl};

/// Direct URL redirect chain остаётся bounded даже без user-facing config knob-а.
const DIRECT_MEDIA_REDIRECT_HOP_LIMIT: u8 = 10;

/// Первая runtime generation нового direct component-а.
const INITIAL_SOURCE_GENERATION: u64 = 1;

/// Первая immutable descriptor generation direct-media adapter-а.
const DIRECT_DESCRIPTOR_GENERATION: u64 = 1;

/// Process-local authority выдаёт разные source lineage даже одинаковым URL opens.
static NEXT_DIRECT_SOURCE_IDENTITY: AtomicU64 = AtomicU64::new(1);

/// Открывает direct media через единый S22 HTTP provider/demux path.
pub(super) fn open_direct_media_url_with_options(
    direct_url: &DirectMediaUrl,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    demuxer_options: DemuxerOptions,
) -> Result<DirectMediaOpenResult, DirectMediaOpenError> {
    let cancellation = CancellationToken::new();
    let mut transport_registry = TransportRegistry::new();
    let provider =
        WebMediaHttpProvider::new(source_config.clone(), prefetch_config).map_err(|source| {
            DirectMediaOpenError::HttpProvider {
                locator: direct_url.clone(),
                source,
            }
        })?;
    let provider_id = provider.descriptor().provider_id().clone();
    transport_registry
        .register(Box::new(provider))
        .map_err(|source| DirectMediaOpenError::TransportRegistry {
            locator: direct_url.clone(),
            source,
        })?;

    let target =
        HttpRequestTarget::parse_exact(direct_url.expose_secret_for_open()).map_err(|_| {
            DirectMediaOpenError::AdapterContract {
                locator: direct_url.clone(),
                reason: "validated direct locator cannot build HTTP request target",
            }
        })?;
    let component = build_direct_component_identity(direct_url)?;
    let secret_scope =
        SecretRequestScope::from_target(&target, HttpPathScope::from_target_path(&target));
    let redirect_limit = RedirectHopLimit::new(DIRECT_MEDIA_REDIRECT_HOP_LIMIT).map_err(|_| {
        DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "direct redirect hop limit exceeds transport safety ceiling",
        }
    })?;
    let request = TransportOpenRequest::new(
        provider_id,
        component,
        target,
        MediaPresentation::Vod,
        SourceGeneration::new(INITIAL_SOURCE_GENERATION),
        SecretRequestContext::builder(secret_scope).build(),
        RedirectPolicy::cross_origin_without_secrets(redirect_limit),
        cancellation.clone(),
    )
    .map_err(|_| DirectMediaOpenError::AdapterContract {
        locator: direct_url.clone(),
        reason: "direct transport request violates secret scope contract",
    })?;
    let opened_transport =
        transport_registry
            .open(request)
            .map_err(|source| DirectMediaOpenError::TransportOpen {
                locator: direct_url.clone(),
                source,
            })?;
    let transport_seekability = opened_transport.seekability();
    let demux_input = match opened_transport.into_input() {
        TransportInput::Seekable(source) => DemuxInput::byte_source(source),
        TransportInput::Streaming(source) => {
            DemuxInput::streaming_source(source, cancellation.clone())
        }
    };

    let mut demux_registry = DemuxRegistry::new();
    let symphonia_factory = SymphoniaDemuxFactory::new(demuxer_options).map_err(|source| {
        DirectMediaOpenError::DemuxIdentity {
            locator: direct_url.clone(),
            source,
        }
    })?;
    demux_registry
        .register(Box::new(symphonia_factory))
        .map_err(|source| DirectMediaOpenError::DemuxRegistry {
            locator: direct_url.clone(),
            source,
        })?;

    let hints = direct_demux_hints(direct_url)?;
    let sniff_budget = direct_sniff_budget(direct_url, &source_config, prefetch_config)?;
    let demuxer = demux_registry
        .open(demux_input, hints, sniff_budget, cancellation.clone())
        .map_err(|source| DirectMediaOpenError::DemuxOpen {
            locator: direct_url.clone(),
            source,
        })?;
    let demuxer: Box<dyn Demuxer + Send> = match transport_seekability {
        TransportSeekability::Seekable => demuxer,
        TransportSeekability::Streaming => {
            let limits = progressive_limits(direct_url, prefetch_config)?;
            let retry_hint =
                DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER).map_err(|_| {
                    DirectMediaOpenError::AdapterContract {
                        locator: direct_url.clone(),
                        reason: "minimum demux retry hint violates media-core bounds",
                    }
                })?;
            Box::new(
                ProgressiveDemuxer::new(demuxer, cancellation, limits, retry_hint).map_err(
                    |source| DirectMediaOpenError::ProgressiveDemuxStartup {
                        locator: direct_url.clone(),
                        source,
                    },
                )?,
            )
        }
    };

    debug!(
        source = %direct_url,
        extension = direct_url.extension().as_extension_hint(),
        seekability = ?demuxer.seekability(),
        "Direct media открыт через neutral progressive HTTP transport"
    );

    let source_label = direct_url.safe_label().to_owned();
    let tracks = demuxer.tracks().to_vec();
    let duration = demuxer.duration();
    let seekability = demuxer.seekability();
    Ok(DirectMediaOpenResult {
        source_label,
        demuxer,
        tracks,
        duration,
        seekability,
    })
}

/// Создаёт process-local exact/semantic identity без переноса raw URL.
fn build_direct_component_identity(
    direct_url: &DirectMediaUrl,
) -> Result<MediaComponentIdentity, DirectMediaOpenError> {
    let source_value = NEXT_DIRECT_SOURCE_IDENTITY
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "direct source identity space exhausted",
        })?;
    let source = SourceIdentity::new(source_value);
    let exact_format = CandidateFormatIdentity::new("direct-resource").map_err(|_| {
        DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "static direct format identity is invalid",
        }
    })?;
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(DIRECT_DESCRIPTOR_GENERATION),
        exact_format,
    );
    let semantic = SemanticIdentity::new(source, "direct-resource").map_err(|_| {
        DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "static direct semantic identity is invalid",
        }
    })?;
    MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed).map_err(|_| {
        DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "direct component identities have different source lineage",
        }
    })
}

/// Передаёт registry одновременно extension и normalized container evidence.
fn direct_demux_hints(direct_url: &DirectMediaUrl) -> Result<DemuxHints, DirectMediaOpenError> {
    let extension = DemuxSourceExtension::new(direct_url.extension().as_extension_hint()).map_err(
        |source| DirectMediaOpenError::DemuxIdentity {
            locator: direct_url.clone(),
            source,
        },
    )?;
    let container_name = match direct_url.extension() {
        DirectMediaExtension::Mp4 | DirectMediaExtension::Mov => "iso-bmff",
        DirectMediaExtension::Mkv => "matroska",
        DirectMediaExtension::Webm => "webm",
    };
    let container = DemuxContainerId::new(container_name).map_err(|source| {
        DirectMediaOpenError::DemuxIdentity {
            locator: direct_url.clone(),
            source,
        }
    })?;
    Ok(DemuxHints::none()
        .with_extension(extension)
        .with_container(container))
}

/// Строит bounded registry sniff policy из уже validated network settings.
fn direct_sniff_budget(
    direct_url: &DirectMediaUrl,
    source_config: &SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
) -> Result<DemuxSniffBudget, DirectMediaOpenError> {
    let max_bytes = usize::try_from(prefetch_config.initial_chunk_bytes())
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "prefetch initial chunk cannot become demux sniff byte budget",
        })?;
    DemuxSniffBudget::new(max_bytes, NonZeroUsize::MIN, source_config.read_timeout()).map_err(
        |_| DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "source read timeout cannot become demux sniff deadline",
        },
    )
}

/// Делит existing transport RAM window между bounded event slots без второго cache policy.
fn progressive_limits(
    direct_url: &DirectMediaUrl,
    prefetch_config: media_prefetch::PrefetchConfig,
) -> Result<ProgressiveDemuxBufferLimits, DirectMediaOpenError> {
    let event_capacity = prefetch_config
        .window_bytes()
        .div_ceil(prefetch_config.chunk_bytes());
    let event_capacity = usize::try_from(event_capacity)
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "prefetch window cannot become progressive event capacity",
        })?;
    let encoded_byte_capacity = usize::try_from(prefetch_config.window_bytes())
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| DirectMediaOpenError::AdapterContract {
            locator: direct_url.clone(),
            reason: "prefetch window cannot become progressive encoded-byte capacity",
        })?;
    Ok(ProgressiveDemuxBufferLimits::new(
        event_capacity,
        encoded_byte_capacity,
    ))
}
