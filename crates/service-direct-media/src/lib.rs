//! Generic direct media opener for seekable `http(s)` container URLs.
//!
//! Crate является service boundary для обычных media URL: он знает только про
//! URL policy, HTTP Range source, prefetch и container demuxer. Он не зависит от
//! `player-core`, UI, renderer или YouTube-specific resolver semantics.

use std::time::Duration;

use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig};
use source_core::{
    ByteSource, HttpRangeSource, HttpRangeSourceConfig, NotSeekableReason, Seekability,
    SourceError, SourceRuntimeConfig,
};
use symphonia_demux::{DemuxSeekability, Demuxer, DemuxerOptions, TrackInfo};
use thiserror::Error;
use tracing::debug;
use url::Url;

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

/// Уже классифицированный direct media URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectMediaUrl {
    /// Нормализованный URL, который можно передать в HTTP source.
    url: String,

    /// Container extension из URL path.
    extension: DirectMediaExtension,

    /// User-facing label без попытки скрывать, что источник сетевой.
    source_label: String,
}

impl DirectMediaUrl {
    /// Возвращает URL для открытия HTTP Range source.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Возвращает container extension, доказанный URL path-ом.
    #[must_use]
    pub const fn extension(&self) -> DirectMediaExtension {
        self.extension
    }

    /// Возвращает label, который можно показать в UI/player snapshot.
    #[must_use]
    pub fn source_label(&self) -> &str {
        &self.source_label
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
    #[error("protocol `{scheme}` не поддерживается; direct media v1 принимает только http(s)")]
    UnsupportedProtocol {
        /// Scheme из URL.
        scheme: String,
    },

    /// HTTP URL без host нельзя открыть через Range source.
    #[error("http(s) URL не содержит host")]
    MissingHost,

    /// URL path не содержит явного `.mp4`, `.mov`, `.mkv` или `.webm`.
    #[error("URL path не содержит поддерживаемый media extension")]
    MissingExtension,

    /// HLS/DASH manifest остаются future feature и не открываются как файл.
    #[error("manifest extension `.{extension}` пока не поддерживается direct media v1")]
    ManifestUnsupported {
        /// Manifest extension из URL path.
        extension: String,
    },

    /// Extension есть, но v1 opener его не поддерживает.
    #[error("extension `.{extension}` не поддерживается direct media v1")]
    UnsupportedExtension {
        /// Extension из URL path.
        extension: String,
    },
}

/// Типизированные ошибки direct media open path.
#[derive(Debug, Error)]
pub enum DirectMediaOpenError {
    /// URL parser не смог разобрать CLI аргумент как absolute URL.
    #[error("некорректный direct media URL `{url}`: {source}")]
    InvalidUrl {
        /// Исходный аргумент.
        url: String,

        /// Ошибка `url` crate.
        #[source]
        source: url::ParseError,
    },

    /// URL валиден синтаксически, но не входит в v1 policy.
    #[error("unsupported direct media URL `{url}`: {reason}")]
    UnsupportedUrl {
        /// Исходный или нормализованный URL.
        url: String,

        /// Стабильная причина rejection-а.
        reason: DirectMediaUrlUnsupportedReason,
    },

    /// Network config не может быть применён к source layer.
    #[error("network config нельзя использовать для direct media source: {source}")]
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
    #[error("network prefetch config нельзя использовать для direct media: {source}")]
    PrefetchConfig {
        /// Ошибка validation-а `media-prefetch`.
        #[source]
        source: media_prefetch::PrefetchConfigError,
    },

    /// Demux config не может быть представлен безопасными runtime options.
    #[error("player.demux.max_consecutive_corrupted_packets должен быть положительным: {value}")]
    DemuxConfig {
        /// Значение из config.
        value: usize,
    },

    /// HTTP Range source не открылся или упал на probe/read boundary.
    #[error("HTTP Range source error для `{url}`: {source}")]
    Source {
        /// Direct URL.
        url: String,

        /// Ошибка source-core.
        #[source]
        source: SourceError,
    },

    /// Source открылся, но не доказал обязательный byte seek.
    #[error("direct media source `{url}` не поддерживает обязательный Range seek: {reason:?}")]
    NonSeekable {
        /// Direct URL.
        url: String,

        /// Причина из source-core probe.
        reason: NotSeekableReason,
    },

    /// Symphonia не смогла открыть container по явному extension hint.
    #[error("demux/probe error для `{url}` как .{extension_hint}: {source}")]
    Demux {
        /// Direct URL.
        url: String,

        /// Extension hint, переданный demuxer-у.
        extension_hint: &'static str,

        /// Ошибка symphonia-demux.
        #[source]
        source: symphonia_demux::DemuxError,
    },
}

/// Проверяет, выглядит ли строка как URL с authority-style scheme.
#[must_use]
pub fn looks_like_url(argument: &str) -> bool {
    argument.contains("://")
}

/// Классифицирует URL как supported direct media URL без сетевых запросов.
pub fn parse_direct_media_url(argument: &str) -> Result<DirectMediaUrl, DirectMediaOpenError> {
    let parsed_url = Url::parse(argument).map_err(|source| DirectMediaOpenError::InvalidUrl {
        url: argument.to_string(),
        source,
    })?;

    direct_media_url_from_parsed(parsed_url)
}

/// Открывает seekable direct media URL через default app configs.
pub fn open_direct_media_url(
    url: &str,
    network_config: &NetworkConfig,
    demux_config: &PlayerDemuxConfig,
) -> Result<DirectMediaOpenResult, DirectMediaOpenError> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .map_err(|source| DirectMediaOpenError::SourceConfig { source })?;
    let prefetch_config = prefetch_config_from_network_config(network_config)?;
    let demuxer_options = demuxer_options_from_config(demux_config)?;

    open_direct_media_url_with_options(url, source_config, prefetch_config, demuxer_options)
}

/// Открывает seekable direct media URL с уже нормализованными runtime options.
pub fn open_direct_media_url_with_options(
    url: &str,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    demuxer_options: DemuxerOptions,
) -> Result<DirectMediaOpenResult, DirectMediaOpenError> {
    let direct_url = parse_direct_media_url(url)?;
    let source = HttpRangeSource::open(HttpRangeSourceConfig::new(
        direct_url.url(),
        Vec::new(),
        source_config,
    ))
    .map_err(|source| DirectMediaOpenError::Source {
        url: direct_url.url().to_string(),
        source,
    })?;

    if let Seekability::NotSeekable { reason } = source.seekability() {
        return Err(DirectMediaOpenError::NonSeekable {
            url: direct_url.url().to_string(),
            reason,
        });
    }

    debug!(
        url = %direct_url.url(),
        extension = direct_url.extension().as_extension_hint(),
        "Direct media HTTP Range source открыт как seekable"
    );

    let extension_hint = direct_url.extension().as_extension_hint();
    let source_label = direct_url.source_label().to_string();
    let prefetch_source =
        media_prefetch::PrefetchingByteSource::new(Box::new(source), prefetch_config);
    let demuxer = symphonia_demux::SymphoniaDemuxer::from_byte_source_with_options(
        prefetch_source,
        extension_hint,
        &source_label,
        demuxer_options,
    )
    .map_err(|source| DirectMediaOpenError::Demux {
        url: direct_url.url().to_string(),
        extension_hint,
        source,
    })?;
    let tracks = demuxer.tracks().to_vec();
    let duration = demuxer.duration();
    let seekability = demuxer.seekability();

    Ok(DirectMediaOpenResult {
        source_label,
        demuxer: Box::new(demuxer),
        tracks,
        duration,
        seekability,
    })
}

/// Валидирует URL components и извлекает supported extension из path.
fn direct_media_url_from_parsed(parsed_url: Url) -> Result<DirectMediaUrl, DirectMediaOpenError> {
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err(unsupported_url(
            parsed_url.as_str(),
            DirectMediaUrlUnsupportedReason::UnsupportedProtocol {
                scheme: parsed_url.scheme().to_string(),
            },
        ));
    }

    if parsed_url.host_str().is_none() {
        return Err(unsupported_url(
            parsed_url.as_str(),
            DirectMediaUrlUnsupportedReason::MissingHost,
        ));
    }

    let extension = path_extension(&parsed_url).ok_or_else(|| {
        unsupported_url(
            parsed_url.as_str(),
            DirectMediaUrlUnsupportedReason::MissingExtension,
        )
    })?;
    let media_extension =
        DirectMediaExtension::from_path_extension(extension).ok_or_else(|| {
            unsupported_url(parsed_url.as_str(), unsupported_extension_reason(extension))
        })?;
    let normalized_url = parsed_url.as_str().to_string();

    Ok(DirectMediaUrl {
        url: normalized_url.clone(),
        extension: media_extension,
        source_label: normalized_url,
    })
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
fn unsupported_url(url: &str, reason: DirectMediaUrlUnsupportedReason) -> DirectMediaOpenError {
    DirectMediaOpenError::UnsupportedUrl {
        url: url.to_string(),
        reason,
    }
}

/// Отделяет будущие manifest protocols от обычного unsupported extension.
fn unsupported_extension_reason(extension: &str) -> DirectMediaUrlUnsupportedReason {
    if extension.eq_ignore_ascii_case("m3u8") || extension.eq_ignore_ascii_case("mpd") {
        return DirectMediaUrlUnsupportedReason::ManifestUnsupported {
            extension: extension.to_ascii_lowercase(),
        };
    }

    DirectMediaUrlUnsupportedReason::UnsupportedExtension {
        extension: extension.to_ascii_lowercase(),
    }
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
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, JoinHandle};

    use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig};

    use super::{
        DirectMediaExtension, DirectMediaOpenError, DirectMediaUrlUnsupportedReason,
        open_direct_media_url, parse_direct_media_url,
    };

    /// Поведение локального HTTP server-а для direct media tests.
    #[derive(Clone, Copy)]
    enum TestHttpBehavior {
        /// Всегда отдавать `200 OK`, даже если клиент прислал Range.
        IgnoreRange,

        /// Отдавать `206 Partial Content` для Range-запросов.
        ServeRange,
    }

    /// Минимальный локальный HTTP server с Range support для unit/integration tests.
    struct TestHttpServer {
        /// Адрес, на который можно собрать direct media URL.
        address: SocketAddr,

        /// Флаг остановки accept loop.
        stop: Arc<AtomicBool>,

        /// Thread с blocking accept loop.
        join_handle: Option<JoinHandle<()>>,
    }

    impl TestHttpServer {
        /// Запускает server на loopback без внешней сети.
        fn spawn(body: Vec<u8>, behavior: TestHttpBehavior) -> Self {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("test HTTP server должен bind-иться");
            let address = listener
                .local_addr()
                .expect("test HTTP server должен знать local_addr");
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let shared_body = Arc::new(body);
            let join_handle = thread::Builder::new()
                .name("direct-media-test-http".to_string())
                .spawn(move || {
                    while !thread_stop.load(Ordering::SeqCst) {
                        let Ok((mut stream, _peer)) = listener.accept() else {
                            break;
                        };

                        if thread_stop.load(Ordering::SeqCst) {
                            break;
                        }

                        handle_test_http_connection(&mut stream, &shared_body, behavior);
                    }
                })
                .expect("test HTTP server thread должен стартовать");

            Self {
                address,
                stop,
                join_handle: Some(join_handle),
            }
        }

        /// Возвращает URL с заданным path.
        fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.address, path)
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Err(error) = TcpStream::connect(self.address) {
                eprintln!("test HTTP server shutdown connect failed: {error}");
            }

            if let Some(join_handle) = self.join_handle.take()
                && join_handle.join().is_err()
            {
                eprintln!("test HTTP server thread panicked during shutdown");
            }
        }
    }

    /// Обрабатывает один HTTP request и закрывает соединение.
    fn handle_test_http_connection(
        stream: &mut TcpStream,
        body: &[u8],
        behavior: TestHttpBehavior,
    ) {
        let Some(request) = read_test_http_request(stream) else {
            return;
        };

        match (behavior, range_header(&request)) {
            (TestHttpBehavior::ServeRange, Some(range_header)) => {
                respond_with_range(stream, body, range_header);
            }
            _ => {
                respond_with_full_body(stream, body);
            }
        }
    }

    /// Читает HTTP headers до пустой строки.
    fn read_test_http_request(stream: &mut TcpStream) -> Option<String> {
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(300)))
            .expect("test stream timeout должен установиться");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];

        loop {
            let bytes_read = stream.read(&mut buffer).ok()?;
            if bytes_read == 0 {
                return None;
            }

            request.extend_from_slice(&buffer[..bytes_read]);

            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }

            if request.len() > 16 * 1024 {
                return None;
            }
        }

        String::from_utf8(request).ok()
    }

    /// Возвращает `Range` header из тестового request-а.
    fn range_header(request: &str) -> Option<&str> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("range") {
                return Some(value.trim());
            }

            None
        })
    }

    /// Отдаёт `206 Partial Content` по byte range.
    fn respond_with_range(stream: &mut TcpStream, body: &[u8], range_header: &str) {
        let Some((start, requested_end)) = parse_range_header(range_header) else {
            respond_with_status(stream, 416, "Range Not Satisfiable", &[]);
            return;
        };

        if start >= body.len() {
            respond_with_status(stream, 416, "Range Not Satisfiable", &[]);
            return;
        }

        let end = requested_end.min(body.len().saturating_sub(1));
        let response_body = &body[start..=end];
        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            response_body.len(),
            start,
            end,
            body.len()
        );

        stream
            .write_all(headers.as_bytes())
            .expect("test HTTP Range response headers должны записаться");
        stream
            .write_all(response_body)
            .expect("test HTTP Range response body должен записаться");
    }

    /// Отдаёт `200 OK` со всем body.
    fn respond_with_full_body(stream: &mut TcpStream, body: &[u8]) {
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );

        stream
            .write_all(headers.as_bytes())
            .expect("test HTTP full response headers должны записаться");
        stream
            .write_all(body)
            .expect("test HTTP full response body должен записаться");
    }

    /// Отдаёт status без body для error paths.
    fn respond_with_status(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );

        stream
            .write_all(headers.as_bytes())
            .expect("test HTTP status response headers должны записаться");
        stream
            .write_all(body)
            .expect("test HTTP status response body должен записаться");
    }

    /// Парсит `Range: bytes=start-end`.
    fn parse_range_header(range_header: &str) -> Option<(usize, usize)> {
        let byte_range = range_header.strip_prefix("bytes=")?;
        let (start, end) = byte_range.split_once('-')?;

        Some((start.parse().ok()?, end.parse().ok()?))
    }

    /// Возвращает минимальный структурированный PCM WAV для HTTP Range regression.
    fn minimal_pcm_wav() -> Vec<u8> {
        let sample_rate_hz = 8_000_u32;
        let channel_count = 1_u16;
        let bits_per_sample = 16_u16;
        let samples = [0_i16, 1_024, -1_024, 0_i16];
        let data_length = u32::try_from(samples.len() * std::mem::size_of::<i16>())
            .expect("test WAV data length fits u32");
        let block_alignment = channel_count * (bits_per_sample / 8);
        let mut bytes = Vec::with_capacity(44 + data_length as usize);

        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36_u32 + data_length).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channel_count.to_le_bytes());
        bytes.extend_from_slice(&sample_rate_hz.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate_hz * u32::from(block_alignment)).to_le_bytes());
        bytes.extend_from_slice(&block_alignment.to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_length.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        bytes
    }

    /// Читает один explicit local media path для ignored manual acceptance test-а.
    fn selected_manual_media_path() -> PathBuf {
        let path = std::env::var_os("RUSTIPLAYER_MEDIA_PATH")
            .map(PathBuf::from)
            .expect("RUSTIPLAYER_MEDIA_PATH must select a local file");
        assert!(
            path.is_file(),
            "selected media path is not a regular file: {}",
            path.display()
        );
        path
    }

    /// Выводит поддерживаемое direct-media расширение выбранного файла без угадывания имени.
    fn selected_direct_extension(path: &Path) -> &str {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .expect("selected direct-media path must have a UTF-8 extension");
        match extension.to_ascii_lowercase().as_str() {
            "mp4" => "mp4",
            "mov" => "mov",
            "mkv" => "mkv",
            "webm" => "webm",
            _ => panic!("selected direct-media extension is unsupported: {extension}"),
        }
    }

    /// Проверяет real selected media через тот же HTTP Range -> Symphonia путь, что и production.
    fn assert_selected_media_opens_over_http_range(path: &Path) {
        let bytes = fs::read(path)
            .unwrap_or_else(|error| panic!("read selected media {}: {error}", path.display()));
        let extension = selected_direct_extension(path);
        let server = TestHttpServer::spawn(bytes, TestHttpBehavior::ServeRange);
        let opened_media = open_direct_media_url(
            &server.url(&format!("/selected.{extension}")),
            &NetworkConfig::default(),
            &PlayerDemuxConfig::default(),
        )
        .expect("selected Range-backed media must open through Symphonia");
        assert!(
            !opened_media.tracks().is_empty(),
            "selected Range-backed media must expose at least one public track"
        );
    }

    #[test]
    fn direct_url_accepts_supported_extension_on_ip() {
        let parsed = parse_direct_media_url("http://127.0.0.1:9001/media/video.MP4?token=1")
            .expect("IP URL с supported extension должен пройти классификацию");

        assert_eq!(parsed.extension(), DirectMediaExtension::Mp4);
    }

    #[test]
    fn direct_url_accepts_quicktime_mov_as_iso_bmff_extension() {
        let parsed = parse_direct_media_url("https://media.example.test/camera/ios-hevc-main10-aac-4k60.MOV")
            .expect("QuickTime .mov должен идти через direct media ISO BMFF path");

        assert_eq!(parsed.extension(), DirectMediaExtension::Mov);
        assert_eq!(parsed.extension().as_extension_hint(), "mov");
    }

    #[test]
    fn direct_url_rejects_hostname_without_extension() {
        let error = parse_direct_media_url("https://media.example.test/video")
            .expect_err("URL без extension должен быть rejected");

        assert!(matches!(
            error,
            DirectMediaOpenError::UnsupportedUrl {
                reason: DirectMediaUrlUnsupportedReason::MissingExtension,
                ..
            }
        ));
    }

    #[test]
    fn direct_url_rejects_unsupported_protocol() {
        let error = parse_direct_media_url("rtsp://media.example.test/video.mp4")
            .expect_err("RTSP остаётся future feature");

        assert!(matches!(
            error,
            DirectMediaOpenError::UnsupportedUrl {
                reason: DirectMediaUrlUnsupportedReason::UnsupportedProtocol { .. },
                ..
            }
        ));
    }

    #[test]
    fn direct_url_rejects_manifest_extension_as_future_feature() {
        let error = parse_direct_media_url("https://media.example.test/live/playlist.m3u8")
            .expect_err("HLS manifest не должен открываться как direct file");

        assert!(matches!(
            error,
            DirectMediaOpenError::UnsupportedUrl {
                reason: DirectMediaUrlUnsupportedReason::ManifestUnsupported { .. },
                ..
            }
        ));
    }

    #[test]
    fn non_range_http_source_returns_typed_non_seekable_error() {
        let server =
            TestHttpServer::spawn(b"not a real mp4".to_vec(), TestHttpBehavior::IgnoreRange);
        let error = open_direct_media_url(
            &server.url("/video.mp4"),
            &NetworkConfig::default(),
            &PlayerDemuxConfig::default(),
        )
        .expect_err("HTTP 200 на Range probe должен стать typed non-seekable");

        assert!(matches!(error, DirectMediaOpenError::NonSeekable { .. }));
    }

    #[test]
    fn range_backed_http_source_opens_structured_wav_through_symphonia() {
        let server = TestHttpServer::spawn(minimal_pcm_wav(), TestHttpBehavior::ServeRange);
        let opened_media = open_direct_media_url(
            &server.url("/media.mp4"),
            &NetworkConfig::default(),
            &PlayerDemuxConfig::default(),
        )
        .expect("Range-backed structured WAV должен открыться через Symphonia");

        assert!(
            opened_media.tracks().iter().any(|track| {
                let codec_id = track.codec_id.to_ascii_lowercase();
                codec_id.contains("pcm")
            }),
            "direct Range source должен вернуть PCM audio track"
        );
    }

    #[test]
    #[ignore = "manual media regression; use scripts/media-regression.sh"]
    fn selected_media_opens_over_direct_http_range() {
        let path = selected_manual_media_path();
        assert_selected_media_opens_over_http_range(&path);
    }
}
