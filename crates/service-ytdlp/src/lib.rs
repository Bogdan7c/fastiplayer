//! Временный YtDlp resolver на базе `yt-dlp`.
//!
//! Этот crate является service boundary: он знает про YtDlp/yt-dlp format
//! selection и HTTP headers, но не знает про UI, renderer или внутренний state
//! player-а. Production startup получает candidates, вызывает service-owned
//! selection policy с neutral capability snapshot, а затем открывает demuxer
//! только для выбранной adaptive пары.

use std::sync::Arc;

use anyhow::Context;
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig, YtDlpConfig};
use source_core::{ByteSource, SourceRuntimeConfig};
use symphonia_demux::DemuxerOptions;

/// Один кибибайт в bytes для slow-start prefetch policy.
const KIB_BYTES: u64 = 1024;

/// Один мебибайт в bytes для явной конвертации пользовательских network-настроек.
const MIB_BYTES: u64 = KIB_BYTES * 1024;

mod admission;
mod candidate;
mod dto;
mod error;
mod http_refresh;
mod http_stream;
mod locator;
mod metadata;
mod process;
mod resolver;
mod selection;
mod topology;

pub use admission::YtDlpCompatibilityRejection;
pub use candidate::{
    YT_DLP_REQUEST_MATERIAL_SCHEMA_VERSION, YtDlpCandidateComponentRequestSummary,
    YtDlpCandidateComponentRole, YtDlpCandidateEntry, YtDlpCandidateMatch, YtDlpCandidateMatchKind,
    YtDlpCandidateNormalizationRejection, YtDlpCandidateOrigin, YtDlpCandidateRematchError,
    YtDlpCandidateSelection, YtDlpCandidateSelectionError, YtDlpCandidateSnapshot,
    YtDlpNormalizedCandidate, YtDlpRejectedCandidate, YtDlpRequestMaterial,
    YtDlpRequestMaterialSummary, YtDlpRequestMaterialV1, YtDlpRequestMaterialViolation,
    YtDlpSelectedCandidateShape, resolve_yt_dlp_candidate_snapshot_with_config,
};
pub use dto::{
    YtDlpDirectStreamDescriptor, YtDlpDirectStreams, YtDlpDynamicRange,
    YtDlpInsufficientVideoMetadata, YtDlpStreamCandidate, YtDlpStreamCandidates, YtDlpStreamKind,
    YtDlpStreamingMedia, YtDlpVideoRequirement,
};
pub use error::YtDlpServiceError;
pub use locator::{
    YtDlpDirectStreamUrl, YtDlpInputScheme, YtDlpLocatorParseError, YtDlpMediaLocator,
    parse_yt_dlp_media_locator,
};
pub use metadata::{YtDlpPlaylistMetadata, resolve_yt_dlp_playlist_metadata_with_config};
pub use resolver::{
    YtDlpSelectedStreamIdentity, resolve_yt_dlp_direct_streams, resolve_yt_dlp_stream_candidates,
    resolve_yt_dlp_stream_candidates_with_config,
};
pub use selection::{
    YtDlpCandidateRejection, YtDlpCandidateRejectionReason, YtDlpStreamSelectionError,
    select_yt_dlp_stream,
};
pub use topology::{
    DEFAULT_TOPOLOGY_DEPTH, DEFAULT_TOPOLOGY_ENTRY_COUNT, DEFAULT_TOPOLOGY_JSON_DEPTH,
    DEFAULT_TOPOLOGY_JSON_LINE_BYTES, DEFAULT_TOPOLOGY_STDERR_BYTES, DEFAULT_TOPOLOGY_STDOUT_BYTES,
    TOPOLOGY_IDENTITY_MAX_UTF8_BYTES, TOPOLOGY_LOCATOR_MAX_UTF8_BYTES,
    TOPOLOGY_METADATA_MAX_UTF8_BYTES, YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES,
    YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION, YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
    YtDlpDelegationMetadataPolicy, YtDlpDurableReopenClassificationError,
    YtDlpDurableReopenIdentityInput, YtDlpDurableReopenMaterialKind, YtDlpDurableReopenPayload,
    YtDlpTopology, YtDlpTopologyBudgetField, YtDlpTopologyBudgets, YtDlpTopologyCollection,
    YtDlpTopologyDelegation, YtDlpTopologyEntry, YtDlpTopologyEntryKind, YtDlpTopologyError,
    YtDlpTopologyIdentity, YtDlpTopologyInvalidResponseReason, YtDlpTopologyKind,
    YtDlpTopologyMetadata, YtDlpTopologyMultiVideo, YtDlpTopologyVideo,
    YtDlpUnavailableTopologyEntry, YtDlpUnavailableTopologyReason,
    classify_yt_dlp_delegation_reopen_target, classify_yt_dlp_durable_reopen_identity,
    extract_yt_dlp_topology_with_budgets, extract_yt_dlp_topology_with_config,
};

use http_refresh::{RefreshContext, YtDlpRefreshingRangeSource};
use http_stream::spawn_http_fetcher;
use resolver::{
    DirectStreamResolver, YtDlpProcessStreamResolver, YtDlpSelectedStreamResolver,
    build_streaming_description, direct_streams_from_matching_candidate,
    direct_streams_from_selected_candidate,
};

/// Result alias фиксирует typed service boundary для всех production open API.
type ServiceResult<T> = std::result::Result<T, YtDlpServiceError>;

#[cfg(test)]
use dto::{YtDlpFormat, YtDlpMetadata, YtDlpRequestedDownload};
#[cfg(test)]
use resolver::select_direct_media_streams;
#[cfg(test)]
use source_core::{CancellationToken, HttpHeader, SourceValidators};
#[cfg(test)]
use std::time::Duration;

/// Проверяет, выглядит ли CLI-аргумент как authority-style network URL.
#[must_use]
pub fn is_probably_url(argument: &str) -> bool {
    // `://` отличает URL form от обычного local path с двоеточием.
    argument.contains("://")
}

/// Проверяет, входит ли absolute URL в pure S00-approved input vocabulary.
#[must_use]
pub fn is_supported_yt_dlp_url(argument: &str) -> bool {
    parse_yt_dlp_media_locator(argument).is_ok()
}

/// Открывает YtDlp URL старым compatibility path-ом без внешнего capability selection.
///
/// Production startup в `app-egui` должен использовать
/// `resolve_yt_dlp_stream_candidates_with_config` и затем
/// `open_streaming_media_from_candidates_with_demux_config`.
pub fn open_streaming_media(
    locator: &YtDlpMediaLocator,
    network_config: &NetworkConfig,
) -> ServiceResult<YtDlpStreamingMedia> {
    open_streaming_media_with_config(locator, network_config, &YtDlpConfig::default())
}

/// Открывает YtDlp URL старым compatibility path-ом с явной service policy.
pub fn open_streaming_media_with_config(
    locator: &YtDlpMediaLocator,
    network_config: &NetworkConfig,
    yt_dlp_config: &YtDlpConfig,
) -> ServiceResult<YtDlpStreamingMedia> {
    open_streaming_media_with_demux_config(
        locator,
        network_config,
        yt_dlp_config,
        &PlayerDemuxConfig::default(),
    )
}

/// Открывает YtDlp URL старым compatibility path-ом с явной demux fail-safe политикой.
pub fn open_streaming_media_with_demux_config(
    locator: &YtDlpMediaLocator,
    network_config: &NetworkConfig,
    yt_dlp_config: &YtDlpConfig,
    demux_config: &PlayerDemuxConfig,
) -> ServiceResult<YtDlpStreamingMedia> {
    if !yt_dlp_config.enabled {
        return Err(YtDlpServiceError::AdapterDisabled);
    }
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let prefetch_config = prefetch_config_from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let demuxer_options = demuxer_options_from_config(demux_config);
    let resolver: Arc<dyn DirectStreamResolver> = Arc::new(
        YtDlpProcessStreamResolver::from_yt_dlp_config(yt_dlp_config)?,
    );
    let mut direct_streams = resolver.resolve_direct_streams(locator)?;
    let description = build_streaming_description(&direct_streams);
    let demuxer = build_demuxer_from_direct_streams(
        locator,
        &mut direct_streams,
        source_config,
        prefetch_config,
        Arc::clone(&resolver),
        demuxer_options,
    )?;

    Ok(YtDlpStreamingMedia {
        demuxer,
        description,
        direct_streams,
    })
}

/// Открывает YtDlp URL по candidate-у, выбранному service selection boundary.
///
/// Этот API является production path для startup/playback: service уже не
/// применяет legacy SDR-safe selector до открытия bytes, а только строит source
/// для выбранной `stream_id` пары.
pub fn open_streaming_media_from_candidates_with_demux_config(
    locator: &YtDlpMediaLocator,
    stream_candidates: &YtDlpStreamCandidates,
    selected_stream_id: &str,
    network_config: &NetworkConfig,
    yt_dlp_config: &YtDlpConfig,
    demux_config: &PlayerDemuxConfig,
) -> ServiceResult<YtDlpStreamingMedia> {
    if !yt_dlp_config.enabled {
        return Err(YtDlpServiceError::AdapterDisabled);
    }
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let prefetch_config = prefetch_config_from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let demuxer_options = demuxer_options_from_config(demux_config);
    let selected_candidate = stream_candidates
        .candidates
        .iter()
        .find(|candidate| candidate.stream_id == selected_stream_id)
        .ok_or(YtDlpServiceError::NoCompatibleStreams {
            reason: YtDlpCompatibilityRejection::MissingSeparateVideo,
        })?;
    let selected_stream_identity = YtDlpSelectedStreamIdentity::from_candidate(selected_candidate);
    let resolver: Arc<dyn DirectStreamResolver> = Arc::new(
        YtDlpSelectedStreamResolver::from_yt_dlp_config(yt_dlp_config, selected_stream_identity)?,
    );
    let mut direct_streams =
        direct_streams_from_selected_candidate(stream_candidates, selected_stream_id)?;
    let description = build_streaming_description(&direct_streams);
    let demuxer = build_demuxer_from_direct_streams(
        locator,
        &mut direct_streams,
        source_config,
        prefetch_config,
        resolver,
        demuxer_options,
    )?;

    Ok(YtDlpStreamingMedia {
        demuxer,
        description,
        direct_streams,
    })
}

/// Причина, почему YtDlp source нельзя открыть как обязательный seekable VOD source.
#[derive(Debug)]
pub enum YtDlpSeekableVodOpenError {
    /// Config/source/demux setup или yt-dlp refresh завершились ошибкой.
    Open(YtDlpServiceError),

    /// Live streams не имеют обязательного exact byte-seek контракта.
    LiveStream,

    /// HTTP Range probe не подтвердил seekability; этот путь не делает streaming fallback.
    RangeUnsupported,
}

impl std::fmt::Display for YtDlpSeekableVodOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(error) => write!(formatter, "{error}"),
            Self::LiveStream => {
                formatter.write_str("YtDlp live stream не поддержан для seekable VOD")
            }
            Self::RangeUnsupported => {
                formatter.write_str("YtDlp VOD не подтвердил обязательный HTTP Range seek")
            }
        }
    }
}

impl std::error::Error for YtDlpSeekableVodOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) => Some(error),
            Self::LiveStream | Self::RangeUnsupported => None,
        }
    }
}

impl From<YtDlpServiceError> for YtDlpSeekableVodOpenError {
    fn from(error: YtDlpServiceError) -> Self {
        Self::Open(error)
    }
}

/// Открывает YtDlp VOD только как seekable Range-backed source.
///
/// В отличие от playback API, этот путь не fallback-ит на streaming reader:
/// source обязан быть exact-seekable или вернуть typed unsupported.
pub fn open_seekable_vod_from_selected_identity_with_demux_config(
    locator: &YtDlpMediaLocator,
    selected_stream_identity: &YtDlpSelectedStreamIdentity,
    network_config: &NetworkConfig,
    yt_dlp_config: &YtDlpConfig,
    demux_config: &PlayerDemuxConfig,
) -> std::result::Result<YtDlpStreamingMedia, YtDlpSeekableVodOpenError> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let prefetch_config = prefetch_config_from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let demuxer_options = demuxer_options_from_config(demux_config);
    let stream_candidates = resolve_yt_dlp_stream_candidates_with_config(locator, yt_dlp_config)?;
    let mut direct_streams =
        direct_streams_from_matching_candidate(&stream_candidates, selected_stream_identity)?;
    let description = build_streaming_description(&direct_streams);
    let resolver: Arc<dyn DirectStreamResolver> =
        Arc::new(YtDlpSelectedStreamResolver::from_yt_dlp_config(
            yt_dlp_config,
            selected_stream_identity.clone(),
        )?);

    let demuxer = build_seekable_vod_demuxer_from_direct_streams(
        locator,
        &mut direct_streams,
        source_config,
        prefetch_config,
        resolver,
        demuxer_options,
    )?;

    Ok(YtDlpStreamingMedia {
        demuxer,
        description,
        direct_streams,
    })
}

/// Повторно открывает exact selected stream identity, сохраняя live/non-seekable fallback.
pub fn open_streaming_media_from_selected_identity_with_demux_config(
    locator: &YtDlpMediaLocator,
    selected_stream_identity: &YtDlpSelectedStreamIdentity,
    network_config: &NetworkConfig,
    yt_dlp_config: &YtDlpConfig,
    demux_config: &PlayerDemuxConfig,
) -> ServiceResult<YtDlpStreamingMedia> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let prefetch_config = prefetch_config_from_network_config(network_config)
        .map_err(YtDlpServiceError::transport)?;
    let demuxer_options = demuxer_options_from_config(demux_config);
    let stream_candidates = resolve_yt_dlp_stream_candidates_with_config(locator, yt_dlp_config)?;
    let mut direct_streams =
        direct_streams_from_matching_candidate(&stream_candidates, selected_stream_identity)?;
    let description = build_streaming_description(&direct_streams);
    let resolver: Arc<dyn DirectStreamResolver> =
        Arc::new(YtDlpSelectedStreamResolver::from_yt_dlp_config(
            yt_dlp_config,
            selected_stream_identity.clone(),
        )?);
    let demuxer = build_demuxer_from_direct_streams(
        locator,
        &mut direct_streams,
        source_config,
        prefetch_config,
        resolver,
        demuxer_options,
    )?;
    Ok(YtDlpStreamingMedia {
        demuxer,
        description,
        direct_streams,
    })
}

fn build_seekable_vod_demuxer_from_direct_streams(
    locator: &YtDlpMediaLocator,
    direct_streams: &mut YtDlpDirectStreams,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    resolver: Arc<dyn DirectStreamResolver>,
    demuxer_options: DemuxerOptions,
) -> std::result::Result<Box<dyn symphonia_demux::Demuxer + Send>, YtDlpSeekableVodOpenError> {
    if direct_streams.live {
        return Err(YtDlpSeekableVodOpenError::LiveStream);
    }

    let Some(demuxer) = open_range_backed_demuxer(
        locator,
        direct_streams,
        source_config,
        prefetch_config,
        resolver,
        demuxer_options,
    )?
    else {
        return Err(YtDlpSeekableVodOpenError::RangeUnsupported);
    };

    Ok(demuxer)
}

/// Строит seekable Range demuxer для VOD или unseekable streaming fallback.
fn build_demuxer_from_direct_streams(
    locator: &YtDlpMediaLocator,
    direct_streams: &mut YtDlpDirectStreams,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    resolver: Arc<dyn DirectStreamResolver>,
    demuxer_options: DemuxerOptions,
) -> ServiceResult<Box<dyn symphonia_demux::Demuxer + Send>> {
    if direct_streams.live {
        tracing::info!("YtDlp live stream открыт как not seekable streaming source");
        return open_unseekable_streaming_demuxer(direct_streams, &source_config, demuxer_options);
    }

    match open_range_backed_demuxer(
        locator,
        direct_streams,
        source_config.clone(),
        prefetch_config,
        resolver,
        demuxer_options,
    )? {
        Some(demuxer) => Ok(demuxer),
        None => open_unseekable_streaming_demuxer(direct_streams, &source_config, demuxer_options),
    }
}

/// Открывает pair demuxer-ов поверх HTTP Range sources, если оба source seekable.
fn open_range_backed_demuxer(
    locator: &YtDlpMediaLocator,
    direct_streams: &mut YtDlpDirectStreams,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    resolver: Arc<dyn DirectStreamResolver>,
    demuxer_options: DemuxerOptions,
) -> ServiceResult<Option<Box<dyn symphonia_demux::Demuxer + Send>>> {
    let video_source = match YtDlpRefreshingRangeSource::open(
        direct_streams.video.clone(),
        source_config.clone(),
        RefreshContext {
            original_locator: locator.clone(),
            stream_kind: YtDlpStreamKind::Video,
            resolver: Arc::clone(&resolver),
        },
    ) {
        Ok(source) => source,
        Err(error) => {
            tracing::warn!(error = %error, "YtDlp video Range probe failed; source stays not seekable");
            return Ok(None);
        }
    };
    let audio_source = match YtDlpRefreshingRangeSource::open(
        direct_streams.audio.clone(),
        source_config.clone(),
        RefreshContext {
            original_locator: locator.clone(),
            stream_kind: YtDlpStreamKind::Audio,
            resolver,
        },
    ) {
        Ok(source) => source,
        Err(error) => {
            tracing::warn!(error = %error, "YtDlp audio Range probe failed; source stays not seekable");
            return Ok(None);
        }
    };

    direct_streams.video = video_source.descriptor().clone();
    direct_streams.video.validators = video_source.validators();
    direct_streams.audio = audio_source.descriptor().clone();
    direct_streams.audio.validators = audio_source.validators();

    if !video_source.seekability().is_seekable() || !audio_source.seekability().is_seekable() {
        tracing::warn!(
            video_seekability = ?video_source.seekability(),
            audio_seekability = ?audio_source.seekability(),
            "YtDlp HTTP Range probe не подтвердил seek; fallback на playback-only streaming"
        );
        return Ok(None);
    }

    let video_source =
        media_prefetch::PrefetchingByteSource::new(Box::new(video_source), prefetch_config)
            .map_err(YtDlpServiceError::transport)?;
    let audio_source =
        media_prefetch::PrefetchingByteSource::new(Box::new(audio_source), prefetch_config)
            .map_err(YtDlpServiceError::transport)?;
    let video_demuxer = symphonia_demux::SymphoniaDemuxer::from_byte_source_with_options(
        video_source,
        "webm",
        "yt_dlp-video",
        demuxer_options,
    )
    .map_err(YtDlpServiceError::demux)?;
    let audio_demuxer = symphonia_demux::SymphoniaDemuxer::from_byte_source_with_options(
        audio_source,
        "webm",
        "yt_dlp-audio",
        demuxer_options,
    )
    .map_err(YtDlpServiceError::demux)?;
    let demuxer = symphonia_demux::DualStreamDemuxer::new(video_demuxer, audio_demuxer)
        .map_err(YtDlpServiceError::demux)?;

    Ok(Some(Box::new(demuxer)))
}

/// Строит нейтральный `media-prefetch` config из пользовательской network-секции.
fn prefetch_config_from_network_config(
    network_config: &NetworkConfig,
) -> anyhow::Result<media_prefetch::PrefetchConfig> {
    let initial_chunk_bytes = network_kibibytes_to_bytes(
        "network.prefetch_initial_chunk_kb",
        network_config.prefetch_initial_chunk_kb,
    )?;
    let chunk_bytes = network_mebibytes_to_bytes(
        "network.prefetch_chunk_mb",
        network_config.prefetch_chunk_mb,
    )?;
    let window_bytes =
        network_mebibytes_to_bytes("network.read_ahead_mb", network_config.read_ahead_mb)?;

    let prefetch_config =
        media_prefetch::PrefetchConfig::new(initial_chunk_bytes, chunk_bytes, window_bytes);
    prefetch_config.with_context(|| {
        format!(
            "некорректные prefetch настройки: prefetch_initial_chunk_kb={}, prefetch_chunk_mb={}, read_ahead_mb={}",
            network_config.prefetch_initial_chunk_kb,
            network_config.prefetch_chunk_mb,
            network_config.read_ahead_mb
        )
    })
}

/// Переводит KiB-поле config-а в bytes без переполнения.
fn network_kibibytes_to_bytes(field_name: &'static str, value_kb: u64) -> anyhow::Result<u64> {
    value_kb
        .checked_mul(KIB_BYTES)
        .with_context(|| format!("{field_name} не помещается в байтовый budget: {value_kb} KiB"))
}

/// Переводит MiB-поле config-а в bytes без переполнения.
fn network_mebibytes_to_bytes(field_name: &'static str, value_mb: u64) -> anyhow::Result<u64> {
    value_mb
        .checked_mul(MIB_BYTES)
        .with_context(|| format!("{field_name} не помещается в байтовый budget: {value_mb} MiB"))
}

/// Открывает старый playback-only path через последовательный HTTP body stream.
fn open_unseekable_streaming_demuxer(
    direct_streams: &YtDlpDirectStreams,
    source_config: &SourceRuntimeConfig,
    demuxer_options: DemuxerOptions,
) -> ServiceResult<Box<dyn symphonia_demux::Demuxer + Send>> {
    let (video_writer, video_reader) = symphonia_demux::StreamingByteReader::channel();
    let (audio_writer, audio_reader) = symphonia_demux::StreamingByteReader::channel();

    spawn_http_fetcher(
        "yt_dlp-video",
        direct_streams.video.clone(),
        source_config.clone(),
        video_writer,
    )
    .map_err(YtDlpServiceError::transport)?;
    spawn_http_fetcher(
        "yt_dlp-audio",
        direct_streams.audio.clone(),
        source_config.clone(),
        audio_writer,
    )
    .map_err(YtDlpServiceError::transport)?;

    let video_demuxer = symphonia_demux::SymphoniaDemuxer::from_stream_with_options(
        video_reader,
        "webm",
        "yt_dlp-video",
        demuxer_options,
    )
    .map_err(YtDlpServiceError::demux)?;
    let audio_demuxer = symphonia_demux::SymphoniaDemuxer::from_stream_with_options(
        audio_reader,
        "webm",
        "yt_dlp-audio",
        demuxer_options,
    )
    .map_err(YtDlpServiceError::demux)?;

    let demuxer = symphonia_demux::DualStreamDemuxer::new(video_demuxer, audio_demuxer)
        .map_err(YtDlpServiceError::demux)?;

    Ok(Box::new(demuxer))
}

/// Конвертирует validated TOML config в runtime options demuxer-а.
fn demuxer_options_from_config(config: &PlayerDemuxConfig) -> DemuxerOptions {
    DemuxerOptions::from_max_consecutive_corrupted_packets(config.max_consecutive_corrupted_packets)
        .expect("validated AppConfig must provide positive demux corrupted packet limit")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use codec_core::{
        BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement, VideoProfile, Vp9Profile,
    };
    use media_core::TimelineNotSeekableReason;
    use source_core::ByteSource;
    use symphonia_demux::DemuxSeekability;

    use super::*;

    #[derive(Debug, Clone)]
    struct TestRequest {
        headers: BTreeMap<String, String>,
    }

    struct TestHttpServer {
        url: String,
        address: SocketAddr,
        stop: Arc<std::sync::atomic::AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
        requests: Arc<Mutex<Vec<TestRequest>>>,
    }

    impl TestHttpServer {
        fn spawn(handler: impl Fn(usize, TestRequest, TcpStream) + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("test server bound");
            listener
                .set_nonblocking(true)
                .expect("test server nonblocking");
            let address = listener.local_addr().expect("test server address");
            let url = format!("http://{address}/media.webm");
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_for_thread = Arc::clone(&stop);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let requests_for_thread = Arc::clone(&requests);
            let handler = Arc::new(handler);

            let handle = thread::spawn(move || {
                let mut request_index = 0_usize;
                while !stop_for_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stop_for_thread.load(Ordering::SeqCst) {
                                break;
                            }

                            let Ok(request) = read_test_request(&stream) else {
                                continue;
                            };
                            requests_for_thread
                                .lock()
                                .expect("requests lock")
                                .push(request.clone());
                            handler(request_index, request, stream);
                            request_index = request_index.saturating_add(1);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                url,
                address,
                stop,
                handle: Some(handle),
                requests,
            }
        }

        fn requests(&self) -> Vec<TestRequest> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(self.address);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    #[derive(Clone)]
    struct FakeResolver {
        streams: Arc<Mutex<YtDlpDirectStreams>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeResolver {
        fn new(streams: YtDlpDirectStreams) -> Self {
            Self {
                streams: Arc::new(Mutex::new(streams)),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl DirectStreamResolver for FakeResolver {
        fn resolve_direct_streams(
            &self,
            _locator: &YtDlpMediaLocator,
        ) -> ServiceResult<YtDlpDirectStreams> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.streams.lock().expect("streams lock").clone())
        }
    }

    /// Создаёт service-owned page locator для hermetic transport tests.
    fn test_yt_dlp_locator() -> YtDlpMediaLocator {
        parse_yt_dlp_media_locator("https://www.youtube.com/watch?v=hermetic-test")
            .expect("canonical test locator должен проходить service parse")
    }

    fn test_network_config(
        read_ahead_mb: u64,
        prefetch_initial_chunk_kb: u64,
        prefetch_chunk_mb: u64,
    ) -> NetworkConfig {
        NetworkConfig {
            memory_cache_mb: 1,
            read_ahead_mb,
            prefetch_initial_chunk_kb,
            prefetch_chunk_mb,
            connect_timeout_ms: 1_000,
            read_timeout_ms: 1_000,
        }
    }

    fn test_source_config() -> SourceRuntimeConfig {
        SourceRuntimeConfig::from_network_config(&test_network_config(1, 64, 1))
            .expect("source config")
    }

    fn test_prefetch_config() -> media_prefetch::PrefetchConfig {
        media_prefetch::PrefetchConfig::new(MIB_BYTES, MIB_BYTES, MIB_BYTES)
            .expect("test prefetch config")
    }

    #[test]
    fn yt_dlp_http_locator_accepts_every_legacy_youtube_host() {
        assert!(is_supported_yt_dlp_url("https://youtube.com/watch?v=abc"));
        assert!(is_supported_yt_dlp_url(
            "https://www.youtube.com/watch?v=abc"
        ));
        assert!(is_supported_yt_dlp_url("https://m.youtube.com/watch?v=abc"));
        assert!(is_supported_yt_dlp_url(
            "https://music.youtube.com/watch?v=abc"
        ));
        assert!(is_supported_yt_dlp_url("https://youtu.be/abc"));
    }

    #[test]
    fn yt_dlp_locator_accepts_http_and_exact_extended_schemes() {
        assert!(is_supported_yt_dlp_url(
            "https://cdn.example.test/video.mp4"
        ));
        assert!(is_supported_yt_dlp_url(
            "https://media.example.test/article/without-extension"
        ));
        assert!(is_supported_yt_dlp_url(
            "ftp://media.example.test/video.webm"
        ));
        assert!(is_supported_yt_dlp_url("rtmp://media.example.test/live"));
        assert!(!is_supported_yt_dlp_url("rtsp://youtube.com/watch?v=abc"));
        assert!(!is_supported_yt_dlp_url("rtmps://media.example.test/live"));
    }

    /// Disabled adapter обязан завершиться typed ошибкой до запуска внешнего процесса.
    #[test]
    fn disabled_adapter_rejects_resolution_without_process_side_effects() {
        // Строим валидный locator, чтобы тест изолировал именно service policy.
        let locator = test_yt_dlp_locator();
        // Меняем только admission-флаг, сохраняя остальные production defaults.
        let yt_dlp_config = YtDlpConfig {
            enabled: false,
            ..YtDlpConfig::default()
        };

        // Public resolver должен остановиться до создания process configuration.
        let resolution_error =
            resolve_yt_dlp_stream_candidates_with_config(&locator, &yt_dlp_config)
                .expect_err("disabled yt-dlp adapter must reject resolution");

        // Typed variant позволяет app composition отличить настройку от process failure.
        assert!(matches!(
            resolution_error,
            YtDlpServiceError::AdapterDisabled
        ));
    }

    #[test]
    fn direct_stream_descriptor_debug_redacts_url_and_header_values() {
        let mut stream = descriptor(
            YtDlpStreamKind::Video,
            "https://user:password@media.example.test/video?signature=secret".to_string(),
        );
        stream
            .headers
            .push(HttpHeader::new("Authorization", "Bearer private-cookie"));
        let formatted = format!("{stream:?}");

        assert!(!formatted.contains("password"));
        assert!(!formatted.contains("signature"));
        assert!(!formatted.contains("private-cookie"));
        assert!(formatted.contains("redacted"));
        assert_eq!(
            stream.url.expose_secret_for_open(),
            "https://user:password@media.example.test/video?signature=secret"
        );
    }

    #[test]
    fn prefetch_config_uses_network_chunk_and_readahead_window() {
        let network_config = test_network_config(9, 128, 3);

        let prefetch_config = prefetch_config_from_network_config(&network_config)
            .expect("valid network prefetch config");

        assert_eq!(prefetch_config.initial_chunk_bytes(), 128 * KIB_BYTES);
        assert_eq!(prefetch_config.chunk_bytes(), 3 * MIB_BYTES);
        assert_eq!(prefetch_config.window_bytes(), 9 * MIB_BYTES);
    }

    #[test]
    fn prefetch_config_rejects_window_smaller_than_chunk() {
        let network_config = test_network_config(4, 64, 8);

        let error = prefetch_config_from_network_config(&network_config)
            .expect_err("window smaller than chunk must be rejected");

        assert!(format!("{error:#}").contains("prefetch_initial_chunk_kb=64"));
        assert!(format!("{error:#}").contains("prefetch_chunk_mb=8"));
        assert!(format!("{error:#}").contains("read_ahead_mb=4"));
    }

    fn descriptor(kind: YtDlpStreamKind, url: String) -> YtDlpDirectStreamDescriptor {
        YtDlpDirectStreamDescriptor {
            kind,
            url: YtDlpDirectStreamUrl::from_secret_for_open(url),
            headers: vec![HttpHeader::new("X-Test-Header", kind.as_str())],
            format_id: Some(kind.as_str().to_string()),
            service_media_id: Some("media-id".to_string()),
            validators: SourceValidators::default(),
            duration: Some(Duration::from_secs(2)),
            live: false,
            description: format!("{} descriptor", kind.as_str()),
        }
    }

    fn direct_streams(video_url: String, audio_url: String) -> YtDlpDirectStreams {
        YtDlpDirectStreams {
            title: Some("title".to_string()),
            service_media_id: Some("media-id".to_string()),
            format_id: Some("video+audio".to_string()),
            height: Some(1080),
            fps: Some(60.0),
            vcodec: Some("vp9".to_string()),
            acodec: Some("opus".to_string()),
            duration: Some(Duration::from_secs(2)),
            live: false,
            video: descriptor(YtDlpStreamKind::Video, video_url),
            audio: descriptor(YtDlpStreamKind::Audio, audio_url),
        }
    }

    fn vp9_profile2_requirement() -> VideoDecodeRequirement {
        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420)
            .with_resolution(3840, 2160)
            .with_frame_rate(60.0)
    }

    fn stream_candidates_from_streams(
        stream_id: &str,
        direct_streams: &YtDlpDirectStreams,
    ) -> YtDlpStreamCandidates {
        YtDlpStreamCandidates {
            title: direct_streams.title.clone(),
            service_media_id: direct_streams.service_media_id.clone(),
            duration: direct_streams.duration,
            live: direct_streams.live,
            candidates: vec![YtDlpStreamCandidate {
                stream_id: stream_id.to_string(),
                format_id: direct_streams.format_id.clone(),
                video: direct_streams.video.clone(),
                audio: Some(direct_streams.audio.clone()),
                height: direct_streams.height,
                fps: direct_streams.fps,
                vcodec: direct_streams.vcodec.clone(),
                acodec: direct_streams.acodec.clone(),
                dynamic_range: YtDlpDynamicRange::Hdr,
                video_requirement: YtDlpVideoRequirement::Ready(vp9_profile2_requirement()),
                quality_score: 1,
            }],
        }
    }

    fn structured_webm_bytes() -> Arc<Vec<u8>> {
        let ebml_header = vec![
            0x1A, 0x45, 0xDF, 0xA3, 0x93, 0x42, 0x86, 0x81, 0x01, 0x42, 0xF7, 0x81, 0x01, 0x42,
            0xF2, 0x81, 0x04, 0x42, 0x82, 0x84, b'w', b'e', b'b', b'm',
        ];
        let info = vec![
            0x15, 0x49, 0xA9, 0x66, 0x8F, 0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40, 0x4D, 0x80,
            0x81, b't', 0x57, 0x41, 0x81, b't',
        ];
        let video_track = vec![
            0xAE, 0x99, 0xD7, 0x81, 0x01, 0x73, 0xC5, 0x81, 0x01, 0x83, 0x81, 0x01, 0x86, 0x85,
            b'V', b'_', b'V', b'P', b'9', 0xE0, 0x86, 0xB0, 0x81, 0x01, 0xBA, 0x81, 0x01,
        ];
        let audio_track = vec![
            0xAE, 0x9A, 0xD7, 0x81, 0x02, 0x73, 0xC5, 0x81, 0x02, 0x83, 0x81, 0x02, 0x86, 0x86,
            b'A', b'_', b'O', b'P', b'U', b'S', 0xE1, 0x86, 0xB5, 0x84, 0x47, 0x3B, 0x80, 0x00,
        ];
        let mut tracks = vec![0x16, 0x54, 0xAE, 0x6B, 0xB7];
        tracks.extend_from_slice(&video_track);
        tracks.extend_from_slice(&audio_track);
        let cluster = vec![
            0x1F, 0x43, 0xB6, 0x75, 0x8A, 0xE7, 0x81, 0x00, 0xA3, 0x85, 0x81, 0x00, 0x00, 0x80,
            0x00,
        ];
        let segment_length = info.len() + tracks.len() + cluster.len();
        let segment_size = u8::try_from(segment_length)
            .expect("structured WebM segment length fits one-byte EBML vint");
        let mut bytes = ebml_header;
        bytes.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0x80 | segment_size]);
        bytes.extend_from_slice(&info);
        bytes.extend_from_slice(&tracks);
        bytes.extend_from_slice(&cluster);
        Arc::new(bytes)
    }

    /// Читает explicit WebM path для ignored manual YtDlp transport acceptance test-ов.
    fn selected_manual_webm_path() -> PathBuf {
        let path = std::env::var_os("RUSTIPLAYER_MEDIA_PATH")
            .map(PathBuf::from)
            .expect("RUSTIPLAYER_MEDIA_PATH must select a local WebM file");
        assert!(
            path.is_file(),
            "selected YtDlp media path is not a regular file: {}",
            path.display()
        );
        assert!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("webm")),
            "selected YtDlp media must be a WebM file: {}",
            path.display()
        );
        path
    }

    /// Читает bytes selected файла, не связывая test с именем или каталогом local asset-а.
    fn selected_manual_webm_bytes(path: &Path) -> Arc<Vec<u8>> {
        Arc::new(
            fs::read(path)
                .unwrap_or_else(|error| panic!("read selected WebM {}: {error}", path.display())),
        )
    }

    fn read_test_request(stream: &TcpStream) -> std::io::Result<TestRequest> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;

        let mut headers = BTreeMap::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let trimmed_line = line.trim_end_matches(['\r', '\n']);
            if trimmed_line.is_empty() {
                break;
            }

            if let Some((name, value)) = trimmed_line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }

        Ok(TestRequest { headers })
    }

    fn write_response(
        mut stream: TcpStream,
        status: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) {
        if write!(stream, "HTTP/1.1 {status}\r\n").is_err() {
            return;
        }
        for (name, value) in headers {
            if write!(stream, "{name}: {value}\r\n").is_err() {
                return;
            }
        }
        if write!(stream, "Connection: close\r\n\r\n").is_err() {
            return;
        }
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    fn respond_with_range(stream: TcpStream, request: &TestRequest, media: &[u8]) {
        let (start, end) = parse_test_range(
            request
                .headers
                .get("range")
                .expect("range header is present"),
        );
        let body = &media[start..=end];
        write_response(
            stream,
            "206 Partial Content",
            &[
                ("Content-Length", body.len().to_string()),
                (
                    "Content-Range",
                    format!("bytes {start}-{end}/{}", media.len()),
                ),
                ("ETag", "\"service-test\"".to_string()),
            ],
            body,
        );
    }

    fn respond_with_full_body(stream: TcpStream, media: &[u8]) {
        write_response(
            stream,
            "200 OK",
            &[("Content-Length", media.len().to_string())],
            media,
        );
    }

    fn parse_test_range(range_header: &str) -> (usize, usize) {
        let range = range_header
            .strip_prefix("bytes=")
            .expect("bytes range prefix");
        let (start, end) = range.split_once('-').expect("range separator");
        (
            start.parse::<usize>().expect("range start"),
            end.parse::<usize>().expect("range end"),
        )
    }

    #[test]
    fn yt_dlp_metadata_is_normalized_to_direct_stream_descriptors() {
        let mut video_headers = BTreeMap::new();
        video_headers.insert("User-Agent".to_string(), "agent".to_string());
        let metadata = YtDlpMetadata {
            entry_type: Some("video".to_string()),
            entries: None,
            has_drm: Some(false),
            title: Some("sample".to_string()),
            id: Some("abc123".to_string()),
            format_id: Some("303+251".to_string()),
            height: Some(1080),
            fps: Some(60.0),
            vcodec: Some("vp9".to_string()),
            acodec: Some("opus".to_string()),
            duration: Some(42.5),
            is_live: Some(false),
            live_status: None,
            requested_downloads: Some(vec![YtDlpRequestedDownload {
                requested_formats: Some(vec![
                    YtDlpFormat {
                        url: Some("http://video.test/media".to_string()),
                        protocol: Some("http".to_string()),
                        has_drm: Some(false),
                        format_id: Some("303".to_string()),
                        ext: Some("webm".to_string()),
                        vcodec: Some("vp9".to_string()),
                        acodec: Some("none".to_string()),
                        width: Some(1920),
                        height: Some(1080),
                        fps: Some(60.0),
                        tbr: Some(2_500.0),
                        vbr: Some(2_500.0),
                        abr: None,
                        dynamic_range: Some("SDR".to_string()),
                        filesize: Some(10),
                        filesize_approx: None,
                        duration: None,
                        http_headers: Some(video_headers),
                    },
                    YtDlpFormat {
                        url: Some("http://audio.test/media".to_string()),
                        protocol: Some("http".to_string()),
                        has_drm: Some(false),
                        format_id: Some("251".to_string()),
                        ext: Some("webm".to_string()),
                        vcodec: Some("none".to_string()),
                        acodec: Some("opus".to_string()),
                        width: None,
                        height: None,
                        fps: None,
                        tbr: Some(160.0),
                        vbr: None,
                        abr: Some(160.0),
                        dynamic_range: None,
                        filesize: None,
                        filesize_approx: Some(5),
                        duration: Some(42.0),
                        http_headers: None,
                    },
                ]),
            }]),
            requested_formats: None,
            formats: None,
        };

        let streams = select_direct_media_streams(&metadata).expect("streams selected");

        assert_eq!(streams.service_media_id.as_deref(), Some("abc123"));
        assert_eq!(streams.video.format_id.as_deref(), Some("303"));
        assert_eq!(streams.audio.format_id.as_deref(), Some("251"));
        assert_eq!(streams.video.duration, Some(Duration::from_secs_f64(42.5)));
        assert_eq!(streams.audio.duration, Some(Duration::from_secs_f64(42.0)));
        assert!(!streams.live);
        assert_eq!(streams.video.headers[0].name, "User-Agent");
        assert!(!streams.video.has_persistent_validators());
    }

    #[test]
    fn range_backed_demuxer_opens_dual_http_sources() {
        let media = structured_webm_bytes();
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, request, stream| {
            assert_eq!(
                request.headers.get("x-test-header").map(String::as_str),
                Some("video")
            );
            respond_with_range(stream, &request, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, request, stream| {
            assert_eq!(
                request.headers.get("x-test-header").map(String::as_str),
                Some("audio")
            );
            respond_with_range(stream, &request, &audio_media);
        });
        let mut streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        let resolver = Arc::new(FakeResolver::new(streams.clone()));

        let demuxer = open_range_backed_demuxer(
            &test_yt_dlp_locator(),
            &mut streams,
            test_source_config(),
            test_prefetch_config(),
            resolver,
            DemuxerOptions::default(),
        )
        .expect("range demuxer attempt succeeds")
        .expect("range demuxer is seekable");

        assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
        assert!(streams.video.has_persistent_validators());
        assert!(streams.audio.has_persistent_validators());
        assert!(
            video_server
                .requests()
                .iter()
                .any(|request| request.headers.contains_key("range"))
        );
        assert!(
            audio_server
                .requests()
                .iter()
                .any(|request| request.headers.contains_key("range"))
        );
    }

    #[test]
    fn selected_candidate_open_path_builds_range_backed_demuxer() {
        let media = structured_webm_bytes();
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, request, stream| {
            assert_eq!(
                request.headers.get("x-test-header").map(String::as_str),
                Some("video")
            );
            respond_with_range(stream, &request, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, request, stream| {
            assert_eq!(
                request.headers.get("x-test-header").map(String::as_str),
                Some("audio")
            );
            respond_with_range(stream, &request, &audio_media);
        });
        let direct_streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        let candidates = stream_candidates_from_streams("selected-vp9-p010", &direct_streams);

        let media = open_streaming_media_from_candidates_with_demux_config(
            &test_yt_dlp_locator(),
            &candidates,
            "selected-vp9-p010",
            &NetworkConfig::default(),
            &YtDlpConfig::default(),
            &PlayerDemuxConfig::default(),
        )
        .expect("selected candidate opens");

        assert_eq!(media.demuxer.seekability(), DemuxSeekability::Seekable);
        assert_eq!(
            media.direct_streams.format_id.as_deref(),
            Some("video+audio")
        );
        assert!(
            video_server
                .requests()
                .iter()
                .any(|request| request.headers.contains_key("range"))
        );
        assert!(
            audio_server
                .requests()
                .iter()
                .any(|request| request.headers.contains_key("range"))
        );
    }

    #[test]
    fn selected_candidate_open_path_requires_audio_companion() {
        let direct_streams = direct_streams(
            "http://video.test/media.webm".to_string(),
            "http://audio.test/media.webm".to_string(),
        );
        let mut candidates = stream_candidates_from_streams("video-only", &direct_streams);
        candidates.candidates[0].audio = None;

        let open_result = open_streaming_media_from_candidates_with_demux_config(
            &test_yt_dlp_locator(),
            &candidates,
            "video-only",
            &NetworkConfig::default(),
            &YtDlpConfig::default(),
            &PlayerDemuxConfig::default(),
        )
        .map(|_| ());
        let error = open_result.expect_err("video-only selected candidate is rejected");

        assert!(matches!(
            error,
            YtDlpServiceError::NoCompatibleStreams {
                reason: YtDlpCompatibilityRejection::MissingSeparateAudio,
            }
        ));
    }

    #[test]
    fn expired_direct_url_refreshes_once_before_range_read() {
        let expired_server = TestHttpServer::spawn(move |_index, _request, stream| {
            write_response(
                stream,
                "403 Forbidden",
                &[("Content-Length", "0".to_string())],
                b"",
            );
        });
        let media = Arc::new(b"0123456789".to_vec());
        let media_for_server = Arc::clone(&media);
        let fresh_server = TestHttpServer::spawn(move |_index, request, stream| {
            respond_with_range(stream, &request, &media_for_server);
        });
        let refreshed_streams = direct_streams(fresh_server.url.clone(), fresh_server.url.clone());
        let fake_resolver = FakeResolver::new(refreshed_streams);
        let mut source = YtDlpRefreshingRangeSource::open(
            descriptor(YtDlpStreamKind::Video, expired_server.url.clone()),
            test_source_config(),
            RefreshContext {
                original_locator: test_yt_dlp_locator(),
                stream_kind: YtDlpStreamKind::Video,
                resolver: Arc::new(fake_resolver.clone()),
            },
        )
        .expect("source refreshes during open");
        let mut output = [0_u8; 4];

        let bytes_read = source
            .read(&mut output, &CancellationToken::never_cancelled())
            .expect("fresh range read works");

        assert_eq!(bytes_read, 4);
        assert_eq!(&output, b"0123");
        assert_eq!(fake_resolver.calls(), 1);
        assert_eq!(expired_server.requests().len(), 1);
    }

    #[test]
    fn range_unsupported_falls_back_to_playback_only_streaming() {
        let media = structured_webm_bytes();
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &audio_media);
        });
        let mut streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        let resolver = Arc::new(FakeResolver::new(streams.clone()));

        let mut demuxer = build_demuxer_from_direct_streams(
            &test_yt_dlp_locator(),
            &mut streams,
            test_source_config(),
            test_prefetch_config(),
            resolver,
            DemuxerOptions::default(),
        )
        .expect("fallback demuxer opens");

        assert_eq!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable
            }
        );
        assert!(matches!(
            demuxer
                .next_event()
                .expect("fallback playback reads packets"),
            media_core::DemuxReadEvent::Packet(_)
        ));
        assert!(
            video_server
                .requests()
                .iter()
                .any(|request| request.headers.contains_key("range"))
        );
        assert!(
            video_server
                .requests()
                .iter()
                .any(|request| !request.headers.contains_key("range"))
        );
    }

    #[test]
    fn range_unsupported_is_typed_unsupported_for_seekable_vod() {
        let media = structured_webm_bytes();
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &audio_media);
        });
        let mut streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        let resolver = Arc::new(FakeResolver::new(streams.clone()));

        let error = build_seekable_vod_demuxer_from_direct_streams(
            &test_yt_dlp_locator(),
            &mut streams,
            test_source_config(),
            test_prefetch_config(),
            resolver,
            DemuxerOptions::default(),
        )
        .map(|_| ())
        .expect_err("seekable VOD path must not fallback to streaming");

        assert!(matches!(error, YtDlpSeekableVodOpenError::RangeUnsupported));
    }

    #[test]
    fn live_streams_are_opened_as_not_seekable() {
        let media = structured_webm_bytes();
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &audio_media);
        });
        let mut streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        streams.live = true;
        streams.video.live = true;
        streams.audio.live = true;
        let resolver = Arc::new(FakeResolver::new(streams.clone()));

        let demuxer = build_demuxer_from_direct_streams(
            &test_yt_dlp_locator(),
            &mut streams,
            test_source_config(),
            test_prefetch_config(),
            resolver,
            DemuxerOptions::default(),
        )
        .expect("live demuxer opens");

        assert!(matches!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable { .. }
        ));
        assert!(
            video_server
                .requests()
                .iter()
                .all(|request| !request.headers.contains_key("range"))
        );
    }

    #[test]
    #[ignore = "manual network acceptance requires an explicit non-YouTube URL"]
    fn real_generic_non_youtube_smoke_requires_explicit_url() -> anyhow::Result<()> {
        let url = std::env::var("RUSTIPLAYER_REAL_YT_DLP_NON_YOUTUBE_SMOKE_URL").context(
            "RUSTIPLAYER_REAL_YT_DLP_NON_YOUTUBE_SMOKE_URL must select a generic media page",
        )?;
        let parsed_url = url::Url::parse(&url).context("manual smoke URL must be absolute")?;
        let normalized_host = parsed_url
            .host_str()
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        anyhow::ensure!(
            !matches!(
                normalized_host.as_str(),
                "youtube.com"
                    | "www.youtube.com"
                    | "m.youtube.com"
                    | "music.youtube.com"
                    | "youtu.be"
            ),
            "manual generic smoke URL must not use a YouTube host"
        );

        let locator = parse_yt_dlp_media_locator(&url)?;
        let media = open_streaming_media(&locator, &NetworkConfig::default())?;

        assert!(media.demuxer.duration().is_some() || media.direct_streams.live);
        Ok(())
    }

    #[test]
    #[ignore = "manual media regression; use scripts/media-regression.sh"]
    fn selected_webm_opens_over_yt_dlp_range_sources() {
        let path = selected_manual_webm_path();
        let media = selected_manual_webm_bytes(&path);
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, request, stream| {
            respond_with_range(stream, &request, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, request, stream| {
            respond_with_range(stream, &request, &audio_media);
        });
        let mut streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        let resolver = Arc::new(FakeResolver::new(streams.clone()));

        let demuxer = open_range_backed_demuxer(
            &test_yt_dlp_locator(),
            &mut streams,
            test_source_config(),
            test_prefetch_config(),
            resolver,
            DemuxerOptions::default(),
        )
        .expect("selected YtDlp Range demuxer attempt succeeds")
        .expect("selected YtDlp Range source is seekable");

        assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
        assert!(streams.video.has_persistent_validators());
        assert!(streams.audio.has_persistent_validators());
    }

    #[test]
    #[ignore = "manual media regression; use scripts/media-regression.sh"]
    fn selected_webm_falls_back_when_yt_dlp_range_is_rejected() {
        let path = selected_manual_webm_path();
        let media = selected_manual_webm_bytes(&path);
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &audio_media);
        });
        let mut streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        let resolver = Arc::new(FakeResolver::new(streams.clone()));

        let mut demuxer = build_demuxer_from_direct_streams(
            &test_yt_dlp_locator(),
            &mut streams,
            test_source_config(),
            test_prefetch_config(),
            resolver,
            DemuxerOptions::default(),
        )
        .expect("selected YtDlp fallback demuxer opens");

        assert!(matches!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable
            }
        ));
        assert!(matches!(
            demuxer
                .next_event()
                .expect("selected fallback reads packets"),
            media_core::DemuxReadEvent::Packet(_)
        ));
    }

    #[test]
    #[ignore = "manual media regression; use scripts/media-regression.sh"]
    fn selected_webm_live_source_skips_yt_dlp_range_probe() {
        let path = selected_manual_webm_path();
        let media = selected_manual_webm_bytes(&path);
        let video_media = Arc::clone(&media);
        let audio_media = Arc::clone(&media);
        let video_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &video_media);
        });
        let audio_server = TestHttpServer::spawn(move |_index, _request, stream| {
            respond_with_full_body(stream, &audio_media);
        });
        let mut streams = direct_streams(video_server.url.clone(), audio_server.url.clone());
        streams.live = true;
        streams.video.live = true;
        streams.audio.live = true;
        let resolver = Arc::new(FakeResolver::new(streams.clone()));

        let demuxer = build_demuxer_from_direct_streams(
            &test_yt_dlp_locator(),
            &mut streams,
            test_source_config(),
            test_prefetch_config(),
            resolver,
            DemuxerOptions::default(),
        )
        .expect("selected YtDlp live demuxer opens");

        assert!(matches!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable { .. }
        ));
        assert!(
            video_server
                .requests()
                .iter()
                .all(|request| !request.headers.contains_key("range"))
        );
    }
}
