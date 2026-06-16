//! Временный YouTube resolver на базе `yt-dlp`.
//!
//! Этот crate является service boundary: он знает про YouTube/yt-dlp format
//! selection и HTTP headers, но не знает про UI, renderer или внутренний state
//! player-а. Production startup получает candidates, выбирает stream через
//! capability layer в `app-egui`, а затем возвращается сюда только для открытия
//! demuxer-а выбранной пары.

use std::io::Read;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig, YoutubeConfig};
use source_core::{ByteSource, HttpHeader, SourceRuntimeConfig};
use symphonia_demux::DemuxerOptions;
use url::Url;

/// Размер HTTP chunk, который fetcher передаёт demuxer-у.
const HTTP_READ_CHUNK_SIZE: usize = 64 * 1024;

/// Один кибибайт в bytes для slow-start prefetch policy.
const KIB_BYTES: u64 = 1024;

/// Один мебибайт в bytes для явной конвертации пользовательских network-настроек.
const MIB_BYTES: u64 = KIB_BYTES * 1024;

mod dto;
mod http_refresh;
mod process;
mod resolver;

pub use dto::{
    YoutubeDirectStreamDescriptor, YoutubeDirectStreams, YoutubeInsufficientVideoMetadata,
    YoutubeStreamCandidate, YoutubeStreamCandidates, YoutubeStreamKind, YoutubeStreamingMedia,
    YoutubeVideoRequirement,
};
pub use resolver::{
    resolve_youtube_direct_streams, resolve_youtube_stream_candidates,
    resolve_youtube_stream_candidates_with_config,
};

use http_refresh::{RefreshContext, YoutubeRefreshingRangeSource};
use resolver::{
    YoutubeDirectStreamResolver, YoutubeSelectedStreamIdentity, YtDlpDirectStreamResolver,
    YtDlpSelectedStreamResolver, build_streaming_description,
    direct_streams_from_selected_candidate,
};

#[cfg(test)]
use dto::{YtDlpFormat, YtDlpMetadata, YtDlpRequestedDownload};
#[cfg(test)]
use resolver::select_direct_media_streams;
#[cfg(test)]
use source_core::{CancellationToken, SourceValidators};
#[cfg(test)]
use std::time::Duration;

/// Проверяет, выглядит ли CLI-аргумент как web URL.
#[must_use]
pub fn is_probably_url(argument: &str) -> bool {
    // Явно поддерживаем только web URL, чтобы локальные пути с двоеточиями не ломали CLI.
    argument.starts_with("https://") || argument.starts_with("http://")
}

/// Проверяет, принадлежит ли web URL YouTube route allowlist.
#[must_use]
pub fn is_supported_youtube_url(argument: &str) -> bool {
    let Ok(parsed_url) = Url::parse(argument) else {
        return false;
    };

    if !matches!(parsed_url.scheme(), "http" | "https") {
        return false;
    }

    let Some(host) = parsed_url.host_str() else {
        return false;
    };
    let normalized_host = host.trim_end_matches('.').to_ascii_lowercase();

    matches!(
        normalized_host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
    )
}

/// Открывает YouTube URL старым compatibility path-ом без внешнего capability selection.
///
/// Production startup в `app-egui` должен использовать
/// `resolve_youtube_stream_candidates_with_config` и затем
/// `open_streaming_media_from_candidates_with_demux_config`.
pub fn open_streaming_media(
    video_url: &str,
    network_config: &NetworkConfig,
) -> Result<YoutubeStreamingMedia> {
    open_streaming_media_with_config(video_url, network_config, &YoutubeConfig::default())
}

/// Открывает YouTube URL старым compatibility path-ом с явной service policy.
pub fn open_streaming_media_with_config(
    video_url: &str,
    network_config: &NetworkConfig,
    youtube_config: &YoutubeConfig,
) -> Result<YoutubeStreamingMedia> {
    open_streaming_media_with_demux_config(
        video_url,
        network_config,
        youtube_config,
        &PlayerDemuxConfig::default(),
    )
}

/// Открывает YouTube URL старым compatibility path-ом с явной demux fail-safe политикой.
pub fn open_streaming_media_with_demux_config(
    video_url: &str,
    network_config: &NetworkConfig,
    youtube_config: &YoutubeConfig,
    demux_config: &PlayerDemuxConfig,
) -> Result<YoutubeStreamingMedia> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .context("Network config нельзя использовать для YouTube source")?;
    let prefetch_config = prefetch_config_from_network_config(network_config)
        .context("Network config нельзя использовать для YouTube prefetch")?;
    let demuxer_options = demuxer_options_from_config(demux_config);
    let resolver: Arc<dyn YoutubeDirectStreamResolver> = Arc::new(
        YtDlpDirectStreamResolver::from_youtube_config(youtube_config)?,
    );
    let mut direct_streams = resolver.resolve_direct_streams(video_url)?;
    let description = build_streaming_description(&direct_streams);
    let demuxer = build_demuxer_from_direct_streams(
        video_url,
        &mut direct_streams,
        source_config,
        prefetch_config,
        Arc::clone(&resolver),
        demuxer_options,
    )?;

    Ok(YoutubeStreamingMedia {
        demuxer,
        description,
        direct_streams,
    })
}

/// Открывает YouTube URL по candidate-у, выбранному внешним capability layer-ом.
///
/// Этот API является production path для startup/playback: service уже не
/// применяет legacy SDR-safe selector до открытия bytes, а только строит source
/// для выбранной `stream_id` пары.
pub fn open_streaming_media_from_candidates_with_demux_config(
    video_url: &str,
    stream_candidates: &YoutubeStreamCandidates,
    selected_stream_id: &str,
    network_config: &NetworkConfig,
    youtube_config: &YoutubeConfig,
    demux_config: &PlayerDemuxConfig,
) -> Result<YoutubeStreamingMedia> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .context("Network config нельзя использовать для YouTube source")?;
    let prefetch_config = prefetch_config_from_network_config(network_config)
        .context("Network config нельзя использовать для YouTube prefetch")?;
    let demuxer_options = demuxer_options_from_config(demux_config);
    let selected_candidate = stream_candidates
        .candidates
        .iter()
        .find(|candidate| candidate.stream_id == selected_stream_id)
        .with_context(|| {
            format!("selected YouTube stream id не найден в candidates: {selected_stream_id}")
        })?;
    let selected_stream_identity =
        YoutubeSelectedStreamIdentity::from_candidate(selected_candidate);
    let resolver: Arc<dyn YoutubeDirectStreamResolver> = Arc::new(
        YtDlpSelectedStreamResolver::from_youtube_config(youtube_config, selected_stream_identity)?,
    );
    let mut direct_streams =
        direct_streams_from_selected_candidate(stream_candidates, selected_stream_id)?;
    let description = build_streaming_description(&direct_streams);
    let demuxer = build_demuxer_from_direct_streams(
        video_url,
        &mut direct_streams,
        source_config,
        prefetch_config,
        resolver,
        demuxer_options,
    )?;

    Ok(YoutubeStreamingMedia {
        demuxer,
        description,
        direct_streams,
    })
}

/// Строит seekable Range demuxer для VOD или unseekable streaming fallback.
fn build_demuxer_from_direct_streams(
    original_video_url: &str,
    direct_streams: &mut YoutubeDirectStreams,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    resolver: Arc<dyn YoutubeDirectStreamResolver>,
    demuxer_options: DemuxerOptions,
) -> Result<Box<dyn symphonia_demux::Demuxer + Send>> {
    if direct_streams.live {
        tracing::info!("YouTube live stream открыт как not seekable streaming source");
        return open_unseekable_streaming_demuxer(direct_streams, &source_config, demuxer_options);
    }

    match open_range_backed_demuxer(
        original_video_url,
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
    original_video_url: &str,
    direct_streams: &mut YoutubeDirectStreams,
    source_config: SourceRuntimeConfig,
    prefetch_config: media_prefetch::PrefetchConfig,
    resolver: Arc<dyn YoutubeDirectStreamResolver>,
    demuxer_options: DemuxerOptions,
) -> Result<Option<Box<dyn symphonia_demux::Demuxer + Send>>> {
    let video_source = match YoutubeRefreshingRangeSource::open(
        direct_streams.video.clone(),
        source_config.clone(),
        RefreshContext {
            original_video_url: original_video_url.to_string(),
            stream_kind: YoutubeStreamKind::Video,
            resolver: Arc::clone(&resolver),
        },
    ) {
        Ok(source) => source,
        Err(error) => {
            tracing::warn!(error = %error, "YouTube video Range probe failed; source stays not seekable");
            return Ok(None);
        }
    };
    let audio_source = match YoutubeRefreshingRangeSource::open(
        direct_streams.audio.clone(),
        source_config.clone(),
        RefreshContext {
            original_video_url: original_video_url.to_string(),
            stream_kind: YoutubeStreamKind::Audio,
            resolver,
        },
    ) {
        Ok(source) => source,
        Err(error) => {
            tracing::warn!(error = %error, "YouTube audio Range probe failed; source stays not seekable");
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
            "YouTube HTTP Range probe не подтвердил seek; fallback на playback-only streaming"
        );
        return Ok(None);
    }

    let video_source =
        media_prefetch::PrefetchingByteSource::new(Box::new(video_source), prefetch_config);
    let audio_source =
        media_prefetch::PrefetchingByteSource::new(Box::new(audio_source), prefetch_config);
    let video_demuxer = symphonia_demux::SymphoniaDemuxer::from_byte_source_with_options(
        video_source,
        "webm",
        "youtube-video",
        demuxer_options,
    )
    .context("Не удалось открыть Range-backed video WebM")?;
    let audio_demuxer = symphonia_demux::SymphoniaDemuxer::from_byte_source_with_options(
        audio_source,
        "webm",
        "youtube-audio",
        demuxer_options,
    )
    .context("Не удалось открыть Range-backed audio WebM")?;
    let demuxer = symphonia_demux::DualStreamDemuxer::new(video_demuxer, audio_demuxer)
        .context("Не удалось объединить Range-backed video/audio demuxer-ы")?;

    Ok(Some(Box::new(demuxer)))
}

/// Строит нейтральный `media-prefetch` config из пользовательской network-секции.
fn prefetch_config_from_network_config(
    network_config: &NetworkConfig,
) -> Result<media_prefetch::PrefetchConfig> {
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
fn network_kibibytes_to_bytes(field_name: &'static str, value_kb: u64) -> Result<u64> {
    value_kb
        .checked_mul(KIB_BYTES)
        .with_context(|| format!("{field_name} не помещается в байтовый budget: {value_kb} KiB"))
}

/// Переводит MiB-поле config-а в bytes без переполнения.
fn network_mebibytes_to_bytes(field_name: &'static str, value_mb: u64) -> Result<u64> {
    value_mb
        .checked_mul(MIB_BYTES)
        .with_context(|| format!("{field_name} не помещается в байтовый budget: {value_mb} MiB"))
}

/// Открывает старый playback-only path через последовательный HTTP body stream.
fn open_unseekable_streaming_demuxer(
    direct_streams: &YoutubeDirectStreams,
    source_config: &SourceRuntimeConfig,
    demuxer_options: DemuxerOptions,
) -> Result<Box<dyn symphonia_demux::Demuxer + Send>> {
    let (video_writer, video_reader) = symphonia_demux::StreamingByteReader::channel();
    let (audio_writer, audio_reader) = symphonia_demux::StreamingByteReader::channel();

    spawn_http_fetcher(
        "youtube-video",
        direct_streams.video.clone(),
        source_config.clone(),
        video_writer,
    )?;
    spawn_http_fetcher(
        "youtube-audio",
        direct_streams.audio.clone(),
        source_config.clone(),
        audio_writer,
    )?;

    let video_demuxer = symphonia_demux::SymphoniaDemuxer::from_stream_with_options(
        video_reader,
        "webm",
        "youtube-video",
        demuxer_options,
    )
    .context("Не удалось открыть streaming video WebM")?;
    let audio_demuxer = symphonia_demux::SymphoniaDemuxer::from_stream_with_options(
        audio_reader,
        "webm",
        "youtube-audio",
        demuxer_options,
    )
    .context("Не удалось открыть streaming audio WebM")?;

    let demuxer = symphonia_demux::DualStreamDemuxer::new(video_demuxer, audio_demuxer)
        .context("Не удалось объединить streaming video/audio demuxer-ы")?;

    Ok(Box::new(demuxer))
}

/// Конвертирует validated TOML config в runtime options demuxer-а.
fn demuxer_options_from_config(config: &PlayerDemuxConfig) -> DemuxerOptions {
    DemuxerOptions::from_max_consecutive_corrupted_packets(config.max_consecutive_corrupted_packets)
        .expect("validated AppConfig must provide positive demux corrupted packet limit")
}

/// Запускает потоковую загрузку одного direct media URL.
fn spawn_http_fetcher(
    thread_name: &'static str,
    stream: YoutubeDirectStreamDescriptor,
    source_config: SourceRuntimeConfig,
    writer: symphonia_demux::StreamingByteWriter,
) -> Result<()> {
    thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            if let Err(fetch_error) = fetch_stream_to_writer(&stream, &source_config, &writer) {
                let fail_message = format!("{thread_name}: {fetch_error}");
                if let Err(writer_error) = writer.fail(fail_message) {
                    tracing::warn!(
                        error = %writer_error,
                        "Не удалось передать ошибку HTTP fetcher-а в streaming reader"
                    );
                }
            }
        })
        .with_context(|| format!("Не удалось запустить HTTP fetcher thread: {thread_name}"))?;

    Ok(())
}

/// Качает HTTP response body и пишет bytes в streaming writer.
fn fetch_stream_to_writer(
    stream: &YoutubeDirectStreamDescriptor,
    source_config: &SourceRuntimeConfig,
    writer: &symphonia_demux::StreamingByteWriter,
) -> Result<()> {
    tracing::info!(description = %stream.description, "HTTP streaming fetch started");

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(source_config.connect_timeout())
        .timeout(source_config.read_timeout())
        .build()
        .context("Не удалось создать reqwest blocking client")?;

    let headers = build_header_map(&stream.headers)?;

    // Выполняем GET и проверяем HTTP status.
    let mut response = client
        .get(&stream.url)
        .headers(headers)
        .send()
        .context("HTTP запрос direct media stream не удался")?
        .error_for_status()
        .context("YouTube direct media stream вернул HTTP ошибку")?;

    // Переиспользуем буфер, чтобы не аллоцировать на каждый read.
    let mut read_buffer = vec![0u8; HTTP_READ_CHUNK_SIZE];

    loop {
        // blocking read естественно ждёт сеть.
        let bytes_read = response
            .read(&mut read_buffer)
            .context("Ошибка чтения HTTP stream")?;

        // EOF response body.
        if bytes_read == 0 {
            writer.finish()?;
            tracing::info!(description = %stream.description, "HTTP streaming fetch finished");
            return Ok(());
        }

        // Bytes копирует только прочитанную часть буфера.
        writer.send_chunk(Bytes::copy_from_slice(&read_buffer[..bytes_read]))?;
    }
}

/// Конвертирует normalized service headers в reqwest HeaderMap.
fn build_header_map(headers: &[HttpHeader]) -> Result<HeaderMap> {
    let mut header_map = HeaderMap::new();

    for header in headers {
        let header_name = HeaderName::from_bytes(header.name.as_bytes())
            .with_context(|| format!("Некорректный HTTP header name от yt-dlp: {}", header.name))?;
        let header_value = HeaderValue::from_str(&header.value).with_context(|| {
            format!(
                "Некорректное значение HTTP header от yt-dlp: {}",
                header.name
            )
        })?;
        header_map.insert(header_name, header_value);
    }

    Ok(header_map)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::PathBuf;
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
        streams: Arc<Mutex<YoutubeDirectStreams>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeResolver {
        fn new(streams: YoutubeDirectStreams) -> Self {
            Self {
                streams: Arc::new(Mutex::new(streams)),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl YoutubeDirectStreamResolver for FakeResolver {
        fn resolve_direct_streams(&self, _video_url: &str) -> Result<YoutubeDirectStreams> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.streams.lock().expect("streams lock").clone())
        }
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
    fn youtube_url_allowlist_accepts_known_hosts() {
        assert!(is_supported_youtube_url("https://youtube.com/watch?v=abc"));
        assert!(is_supported_youtube_url(
            "https://www.youtube.com/watch?v=abc"
        ));
        assert!(is_supported_youtube_url(
            "https://m.youtube.com/watch?v=abc"
        ));
        assert!(is_supported_youtube_url(
            "https://music.youtube.com/watch?v=abc"
        ));
        assert!(is_supported_youtube_url("https://youtu.be/abc"));
    }

    #[test]
    fn youtube_url_allowlist_rejects_generic_http_media() {
        assert!(!is_supported_youtube_url(
            "https://cdn.example.test/video.mp4"
        ));
        assert!(!is_supported_youtube_url("rtsp://youtube.com/watch?v=abc"));
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

    fn descriptor(kind: YoutubeStreamKind, url: String) -> YoutubeDirectStreamDescriptor {
        YoutubeDirectStreamDescriptor {
            kind,
            url,
            headers: vec![HttpHeader::new("X-Test-Header", kind.as_str())],
            format_id: Some(kind.as_str().to_string()),
            service_media_id: Some("media-id".to_string()),
            validators: SourceValidators::default(),
            duration: Some(Duration::from_secs(2)),
            live: false,
            description: format!("{} descriptor", kind.as_str()),
        }
    }

    fn direct_streams(video_url: String, audio_url: String) -> YoutubeDirectStreams {
        YoutubeDirectStreams {
            title: Some("title".to_string()),
            service_media_id: Some("media-id".to_string()),
            format_id: Some("video+audio".to_string()),
            height: Some(1080),
            fps: Some(60.0),
            vcodec: Some("vp9".to_string()),
            acodec: Some("opus".to_string()),
            duration: Some(Duration::from_secs(2)),
            live: false,
            video: descriptor(YoutubeStreamKind::Video, video_url),
            audio: descriptor(YoutubeStreamKind::Audio, audio_url),
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
        direct_streams: &YoutubeDirectStreams,
    ) -> YoutubeStreamCandidates {
        YoutubeStreamCandidates {
            title: direct_streams.title.clone(),
            service_media_id: direct_streams.service_media_id.clone(),
            duration: direct_streams.duration,
            live: direct_streams.live,
            candidates: vec![YoutubeStreamCandidate {
                stream_id: stream_id.to_string(),
                format_id: direct_streams.format_id.clone(),
                video: direct_streams.video.clone(),
                audio: Some(direct_streams.audio.clone()),
                height: direct_streams.height,
                fps: direct_streams.fps,
                vcodec: direct_streams.vcodec.clone(),
                acodec: direct_streams.acodec.clone(),
                video_requirement: YoutubeVideoRequirement::Ready(vp9_profile2_requirement()),
                quality_score: 1,
            }],
        }
    }

    fn test_webm_bytes() -> Option<Arc<Vec<u8>>> {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/VP9/test.webm");
        match std::fs::read(&path) {
            Ok(bytes) => Some(Arc::new(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping optional media fixture {}", path.display());
                None
            }
            Err(error) => panic!("test webm bytes {}: {error}", path.display()),
        }
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
                        url: "http://video.test/media".to_string(),
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
                        url: "http://audio.test/media".to_string(),
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
        let Some(media) = test_webm_bytes() else {
            return;
        };
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
            "http://youtube.test/watch",
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
        let Some(media) = test_webm_bytes() else {
            return;
        };
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
            "http://youtube.test/watch",
            &candidates,
            "selected-vp9-p010",
            &NetworkConfig::default(),
            &YoutubeConfig::default(),
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
            "http://youtube.test/watch",
            &candidates,
            "video-only",
            &NetworkConfig::default(),
            &YoutubeConfig::default(),
            &PlayerDemuxConfig::default(),
        )
        .map(|_| ());
        let error = open_result.expect_err("video-only selected candidate is rejected");

        assert!(format!("{error:#}").contains("audio companion descriptor"));
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
        let mut source = YoutubeRefreshingRangeSource::open(
            descriptor(YoutubeStreamKind::Video, expired_server.url.clone()),
            test_source_config(),
            RefreshContext {
                original_video_url: "http://youtube.test/watch".to_string(),
                stream_kind: YoutubeStreamKind::Video,
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
        let Some(media) = test_webm_bytes() else {
            return;
        };
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
            "http://youtube.test/watch",
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
        assert!(
            demuxer
                .next_packet()
                .expect("fallback playback reads packets")
                .is_some()
        );
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
    fn live_streams_are_opened_as_not_seekable() {
        let Some(media) = test_webm_bytes() else {
            return;
        };
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
            "http://youtube.test/watch",
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
    fn real_youtube_smoke_is_env_gated() {
        let Ok(url) = std::env::var("RUSTIPLAYER_REAL_YOUTUBE_SMOKE_URL") else {
            return;
        };

        let media = open_streaming_media(&url, &NetworkConfig::default())
            .expect("env-gated real YouTube smoke opens");

        assert!(media.demuxer.duration().is_some() || media.direct_streams.live);
    }
}
