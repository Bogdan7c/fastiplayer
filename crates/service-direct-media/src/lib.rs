//! Generic direct media opener for seekable `http(s)` container URLs.
//!
//! Crate является service boundary для обычных media URL: он знает только про
//! URL policy, HTTP Range source, prefetch и container demuxer. Он не зависит от
//! `player-core`, UI, renderer или extractor-specific resolver semantics.

use std::time::Duration;

use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use source_core::{NotSeekableReason, SourceError, SourceRuntimeConfig};
use symphonia_demux::{DemuxSeekability, Demuxer, DemuxerOptions, MediaMetadata, TrackInfo};
use thiserror::Error;
use url::Url;

mod locator;
mod transport;

pub use locator::DirectMediaUrl;

/// Один кибибайт в bytes для явного mapping-а network config.
const KIB_BYTES: u64 = 1024;

/// Один мебибайт в bytes для явного mapping-а network config.
const MIB_BYTES: u64 = KIB_BYTES * 1024;

/// Поддерживаемые container extensions для v1 direct media path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectMediaExtension {
    /// ISO BMFF / MP4.
    Mp4,

    /// QuickTime/MOV, тот же ISO BMFF probe path, но с отдельным extension hint.
    Mov,

    /// Matroska.
    Mkv,

    /// WebM.
    Webm,
}

impl DirectMediaExtension {
    /// Возвращает extension hint, который передаётся в Symphonia probe.
    #[must_use]
    pub const fn as_extension_hint(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
        }
    }

    /// Нормализует path extension из URL без content sniffing.
    #[must_use]
    pub fn from_path_extension(extension: &str) -> Option<Self> {
        if extension.eq_ignore_ascii_case("mp4") {
            return Some(Self::Mp4);
        }

        if extension.eq_ignore_ascii_case("mov") {
            return Some(Self::Mov);
        }

        if extension.eq_ignore_ascii_case("mkv") {
            return Some(Self::Mkv);
        }

        if extension.eq_ignore_ascii_case("webm") {
            return Some(Self::Webm);
        }

        None
    }
}

/// Neutral result открытия direct media без зависимости на `player-core`.
pub struct DirectMediaOpenResult {
    /// User-facing source label.
    source_label: String,

    /// Открытый demuxer, готовый к передаче владельцу playback pipeline.
    demuxer: Box<dyn Demuxer + Send>,

    /// Tracks snapshot, снятый сразу после demux open.
    tracks: Vec<TrackInfo>,

    /// Duration snapshot, если container её сообщил.
    duration: Option<Duration>,

    /// Seekability snapshot после HTTP Range + demux open.
    seekability: DemuxSeekability,
}

impl std::fmt::Debug for DirectMediaOpenResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DirectMediaOpenResult")
            .field("source_label", &self.source_label)
            .field("tracks", &self.tracks)
            .field("duration", &self.duration)
            .field("seekability", &self.seekability)
            .finish_non_exhaustive()
    }
}

impl DirectMediaOpenResult {
    /// Возвращает user-facing label source-а.
    #[must_use]
    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    /// Возвращает tracks snapshot без доступа к owned demuxer.
    #[must_use]
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Возвращает duration snapshot.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Возвращает seekability snapshot.
    #[must_use]
    pub const fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    /// Возвращает полный read-only metadata snapshot до ownership transfer demuxer-а.
    ///
    /// Это позволяет app-owned prepared envelope-у не выполнять второй target open/probe.
    #[must_use]
    pub fn media_metadata(&self) -> Option<MediaMetadata> {
        self.demuxer.media_metadata()
    }

    /// Передаёт demuxer во владение app/player boundary.
    #[must_use]
    pub fn into_demuxer(self) -> Box<dyn Demuxer + Send> {
        self.demuxer
    }
}

/// Причина, почему URL не входит в v1 direct media policy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DirectMediaUrlUnsupportedReason {
    /// Поддерживаются только `http` и `https`.
    #[error("protocol не поддерживается; direct media v1 принимает только http(s)")]
    UnsupportedProtocol,

    /// HTTP URL без host нельзя открыть через Range source.
    #[error("http(s) URL не содержит host")]
    MissingHost,

    /// URL path не содержит явного `.mp4`, `.mov`, `.mkv` или `.webm`.
    #[error("URL path не содержит поддерживаемый media extension")]
    MissingExtension,

    /// HLS/DASH manifest остаются future feature и не открываются как файл.
    #[error("manifest URL пока не поддерживается direct media v1")]
    ManifestUnsupported,

    /// Extension есть, но v1 opener его не поддерживает.
    #[error("URL extension не поддерживается direct media v1")]
    UnsupportedExtension,
}

/// Типизированные ошибки direct media open path.
#[derive(Debug, Error)]
pub enum DirectMediaOpenError {
    /// URL parser не смог разобрать CLI аргумент как absolute URL.
    #[error("некорректный direct media URL")]
    InvalidUrl {
        /// Ошибка `url` crate.
        #[source]
        source: url::ParseError,
    },

    /// URL валиден синтаксически, но не входит в v1 policy.
    #[error("unsupported direct media URL: {reason}")]
    UnsupportedUrl {
        /// Стабильная причина rejection-а.
        reason: DirectMediaUrlUnsupportedReason,
    },

    /// Network config не может быть применён к source layer.
    #[error("network config нельзя использовать для direct media source")]
    SourceConfig {
        /// Исходная source-core ошибка validation-а.
        #[source]
        source: SourceError,
    },

    /// Network prefetch budget переполнился при переводе в bytes.
    #[error("{field} не помещается в byte budget: {value} {unit}")]
    NetworkBudgetOverflow {
        /// TOML-путь поля.
        field: &'static str,

        /// Пользовательское значение.
        value: u64,

        /// Единица исходного поля.
        unit: &'static str,
    },

    /// Prefetch config отклонил нормализованные значения.
    #[error("network prefetch config нельзя использовать для direct media")]
    PrefetchConfig {
        /// Ошибка validation-а `media-prefetch`.
        #[source]
        source: media_prefetch::PrefetchConfigError,
    },

    /// Background prefetch worker не удалось создать до передачи source demuxer-у.
    #[error("не удалось запустить prefetch для {locator}")]
    PrefetchStartup {
        /// Redacted typed locator сохраняет service-level контекст открытия.
        locator: DirectMediaUrl,

        /// Исходная typed ошибка startup boundary из `media-prefetch`.
        #[source]
        source: media_prefetch::PrefetchStartupError,
    },

    /// Demux config не может быть представлен безопасными runtime options.
    #[error("player.demux.max_consecutive_corrupted_packets должен быть положительным: {value}")]
    DemuxConfig {
        /// Значение из config.
        value: usize,
    },

    /// HTTP Range source не открылся или упал на probe/read boundary.
    #[error("HTTP Range source error для {locator}")]
    Source {
        /// Redacted typed locator.
        locator: DirectMediaUrl,

        /// Ошибка source-core.
        #[source]
        source: SourceError,
    },

    /// Source открылся, но не доказал обязательный byte seek.
    #[error("direct media source {locator} не поддерживает обязательный Range seek: {reason:?}")]
    NonSeekable {
        /// Redacted typed locator.
        locator: DirectMediaUrl,

        /// Причина из source-core probe.
        reason: NotSeekableReason,
    },

    /// Symphonia не смогла открыть container по явному extension hint.
    #[error("demux/probe error для {locator} как .{extension_hint}")]
    Demux {
        /// Redacted typed locator.
        locator: DirectMediaUrl,

        /// Extension hint, переданный demuxer-у.
        extension_hint: &'static str,

        /// Ошибка symphonia-demux.
        #[source]
        source: symphonia_demux::DemuxError,
    },

    /// Static HTTP provider registration нарушила compile-time contract.
    #[error("HTTP transport provider нельзя собрать для {locator}")]
    HttpProvider {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// Static provider descriptor error.
        #[source]
        source: web_media_http::WebMediaHttpProviderBuildError,
    },

    /// Neutral transport registry отклонил provider registration.
    #[error("HTTP transport registry отклонил provider для {locator}")]
    TransportRegistry {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// Typed registry error без request payload.
        #[source]
        source: web_media_transport_api::TransportRegistryError,
    },

    /// Neutral transport open завершился typed operational error-ом.
    #[error("HTTP transport не открыл direct media {locator}")]
    TransportOpen {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// S21T typed error без raw URL/header/body.
        #[source]
        source: web_media_transport_api::TransportOpenError,
    },

    /// Static identity/hint/request assembly нарушила internal direct adapter contract.
    #[error("direct media adapter contract нарушен для {locator}: {reason}")]
    AdapterContract {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// Фиксированная safe причина без locator payload.
        reason: &'static str,
    },

    /// Symphonia factory descriptor не прошёл neutral identity validation.
    #[error("demux factory registration нельзя собрать для {locator}")]
    DemuxIdentity {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// Neutral identity grammar error.
        #[source]
        source: demux_api::DemuxIdentityError,
    },

    /// Neutral demux registry отклонил factory registration.
    #[error("demux registry отклонил factory для {locator}")]
    DemuxRegistry {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// Typed registration error.
        #[source]
        source: demux_api::DemuxRegistryError,
    },

    /// Neutral registry не смог probe/open container.
    #[error("demux registry не открыл {locator}")]
    DemuxOpen {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// Typed bounded probe/factory error.
        #[source]
        source: demux_api::DemuxOpenError,
    },

    /// Non-Range demux worker не запустился до публикации prepared media.
    #[error("progressive demux worker не запустился для {locator}")]
    ProgressiveDemuxStartup {
        /// Redacted direct-media locator.
        locator: DirectMediaUrl,
        /// Typed worker startup error.
        #[source]
        source: demux_api::ProgressiveDemuxStartupError,
    },
}

/// Проверяет, выглядит ли строка как URL с authority-style scheme.
#[must_use]
pub fn looks_like_url(argument: &str) -> bool {
    argument.contains("://")
}

/// Классифицирует URL как supported direct media URL без сетевых запросов.
pub fn parse_direct_media_url(argument: &str) -> Result<DirectMediaUrl, DirectMediaOpenError> {
    let parsed_url =
        Url::parse(argument).map_err(|source| DirectMediaOpenError::InvalidUrl { source })?;

    direct_media_url_from_parsed(argument, parsed_url)
}

/// Открывает seekable direct media URL через default app configs.
pub fn open_direct_media_url(
    locator: &DirectMediaUrl,
    network_config: &NetworkConfig,
    demux_config: &PlayerDemuxConfig,
) -> Result<DirectMediaOpenResult, DirectMediaOpenError> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .map_err(|source| DirectMediaOpenError::SourceConfig { source })?;
    let prefetch_config = prefetch_config_from_network_config(network_config)?;
    let demuxer_options = demuxer_options_from_config(demux_config)?;

    open_direct_media_url_with_options(locator, source_config, prefetch_config, demuxer_options)
}

/// Открывает seekable direct media URL с уже нормализованными runtime options.
pub fn open_direct_media_url_with_options(
    direct_url: &DirectMediaUrl,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    demuxer_options: DemuxerOptions,
) -> Result<DirectMediaOpenResult, DirectMediaOpenError> {
    transport::open_direct_media_url_with_options(
        direct_url,
        source_config,
        prefetch_config,
        demuxer_options,
    )
}

/// Валидирует URL components и извлекает supported extension из path.
fn direct_media_url_from_parsed(
    original_argument: &str,
    parsed_url: Url,
) -> Result<DirectMediaUrl, DirectMediaOpenError> {
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err(unsupported_url(
            DirectMediaUrlUnsupportedReason::UnsupportedProtocol,
        ));
    }

    if parsed_url.host_str().is_none() {
        return Err(unsupported_url(
            DirectMediaUrlUnsupportedReason::MissingHost,
        ));
    }

    let extension = path_extension(&parsed_url)
        .ok_or_else(|| unsupported_url(DirectMediaUrlUnsupportedReason::MissingExtension))?;
    let media_extension = DirectMediaExtension::from_path_extension(extension)
        .ok_or_else(|| unsupported_url(unsupported_extension_reason(extension)))?;
    let safe_label = direct_media_safe_label(&parsed_url, media_extension);
    let requires_sensitive_persistence_acknowledgement = !parsed_url.username().is_empty()
        || parsed_url.password().is_some()
        || parsed_url.query().is_some_and(|query| !query.is_empty());

    Ok(DirectMediaUrl::new(
        original_argument.to_string(),
        media_extension,
        safe_label,
        requires_sensitive_persistence_acknowledgement,
    ))
}

/// Извлекает extension только из последнего сегмента path, игнорируя query/fragment.
fn path_extension(parsed_url: &Url) -> Option<&str> {
    let last_segment = parsed_url
        .path_segments()?
        .rfind(|segment| !segment.is_empty())?;
    let (_, extension) = last_segment.rsplit_once('.')?;

    if extension.is_empty() {
        return None;
    }

    Some(extension)
}

/// Создаёт typed unsupported URL error без потери исходной причины.
fn unsupported_url(reason: DirectMediaUrlUnsupportedReason) -> DirectMediaOpenError {
    DirectMediaOpenError::UnsupportedUrl { reason }
}

/// Строит bounded label только из scheme, host, optional port и extension.
fn direct_media_safe_label(url: &Url, extension: DirectMediaExtension) -> String {
    const MAX_HOST_LABEL_BYTES: usize = 253;

    let host = url
        .host_str()
        .filter(|host| host.len() <= MAX_HOST_LABEL_BYTES)
        .unwrap_or("<redacted-host>");
    let port = url
        .port()
        .map_or_else(String::new, |value| format!(":{value}"));
    format!(
        "direct media {}://{host}{port}/<redacted>.{}",
        url.scheme(),
        extension.as_extension_hint()
    )
}

/// Отделяет будущие manifest protocols от обычного unsupported extension.
fn unsupported_extension_reason(extension: &str) -> DirectMediaUrlUnsupportedReason {
    if extension.eq_ignore_ascii_case("m3u8") || extension.eq_ignore_ascii_case("mpd") {
        return DirectMediaUrlUnsupportedReason::ManifestUnsupported;
    }

    DirectMediaUrlUnsupportedReason::UnsupportedExtension
}

/// Строит нейтральный `media-prefetch` config из пользовательской network-секции.
fn prefetch_config_from_network_config(
    network_config: &NetworkConfig,
) -> Result<media_prefetch::PrefetchConfig, DirectMediaOpenError> {
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

    media_prefetch::PrefetchConfig::new(initial_chunk_bytes, chunk_bytes, window_bytes)
        .map_err(|source| DirectMediaOpenError::PrefetchConfig { source })
}

/// Переводит KiB-поле config-а в bytes без переполнения.
fn network_kibibytes_to_bytes(
    field: &'static str,
    value: u64,
) -> Result<u64, DirectMediaOpenError> {
    value
        .checked_mul(KIB_BYTES)
        .ok_or(DirectMediaOpenError::NetworkBudgetOverflow {
            field,
            value,
            unit: "KiB",
        })
}

/// Переводит MiB-поле config-а в bytes без переполнения.
fn network_mebibytes_to_bytes(
    field: &'static str,
    value: u64,
) -> Result<u64, DirectMediaOpenError> {
    value
        .checked_mul(MIB_BYTES)
        .ok_or(DirectMediaOpenError::NetworkBudgetOverflow {
            field,
            value,
            unit: "MiB",
        })
}

/// Конвертирует validated TOML config в runtime options demuxer-а.
fn demuxer_options_from_config(
    config: &PlayerDemuxConfig,
) -> Result<DemuxerOptions, DirectMediaOpenError> {
    DemuxerOptions::from_max_consecutive_corrupted_packets(config.max_consecutive_corrupted_packets)
        .ok_or(DirectMediaOpenError::DemuxConfig {
            value: config.max_consecutive_corrupted_packets,
        })
}

#[cfg(test)]
mod tests;
