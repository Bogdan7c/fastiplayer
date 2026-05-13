//! Временный YouTube resolver на базе `yt-dlp`.
//!
//! Этот crate является service boundary: он знает про YouTube/yt-dlp format
//! selection и HTTP headers, но не знает про UI, renderer или внутренний state
//! player-а. `app-egui` получает отсюда уже готовый streaming demuxer.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rustiplayer_config::NetworkConfig;
use serde::Deserialize;
use source_core::{
    ByteSource, CachedByteSource, CancellationToken, HttpHeader, HttpRangeSource,
    HttpRangeSourceConfig, Seekability, SourceError, SourceFingerprint, SourceResult,
    SourceRuntimeConfig, SourceValidators,
};

/// Имя env-переменной для полного переопределения selector-а `yt-dlp`.
const FORMAT_SELECTOR_ENV: &str = "VIDEO_PLAYER_YOUTUBE_FORMAT_SELECTOR";

/// Selector `yt-dlp` по умолчанию для поддерживаемых тестовых потоков.
///
/// Приоритет намеренно идёт от самого тяжёлого SDR-теста к более лёгким:
/// 4K60 -> 4K30 -> 1080p60 -> 1080p.
///
/// Важно: `vcodec=vp9` выбран строго, без `vp9.2`.
/// `vp9.2` обычно означает HDR/10-bit, а текущий production renderer Phase 9
/// рассчитан на NV12/8-bit для обычного playback path.
const DEFAULT_FORMAT_SELECTOR: &str = concat!(
    "bestvideo[ext=webm][vcodec=vp9][height=2160][fps>=60]+",
    "bestaudio[ext=webm][acodec=opus]/",
    "bestvideo[ext=webm][vcodec=vp9][height=2160]+",
    "bestaudio[ext=webm][acodec=opus]/",
    "bestvideo[ext=webm][vcodec=vp9][height=1080][fps>=60]+",
    "bestaudio[ext=webm][acodec=opus]/",
    "bestvideo[ext=webm][vcodec=vp9][height=1080]+",
    "bestaudio[ext=webm][acodec=opus]"
);

/// Размер HTTP chunk, который fetcher передаёт demuxer-у.
const HTTP_READ_CHUNK_SIZE: usize = 64 * 1024;

/// Минимальная metadata по выбранному YouTube формату.
#[derive(Debug, Deserialize)]
struct YtDlpMetadata {
    /// Заголовок ролика для логов.
    title: Option<String>,

    /// YouTube id для логов.
    id: Option<String>,

    /// Итоговый выбранный combined format.
    format_id: Option<String>,

    /// Высота выбранного video stream.
    height: Option<u32>,

    /// FPS выбранного video stream.
    fps: Option<f64>,

    /// Video codec выбранного stream.
    vcodec: Option<String>,

    /// Audio codec выбранного stream.
    acodec: Option<String>,

    /// Длительность VOD в секундах, если extractor её знает.
    duration: Option<f64>,

    /// Флаг live-трансляции от extractor-а.
    is_live: Option<bool>,

    /// Текстовый live status от yt-dlp для новых extractor-ов.
    live_status: Option<String>,

    /// Подробности выбранных adaptive streams после format selection.
    requested_downloads: Option<Vec<YtDlpRequestedDownload>>,

    /// Fallback-поле: некоторые версии yt-dlp кладут выбранные streams сюда.
    requested_formats: Option<Vec<YtDlpFormat>>,
}

/// Один download candidate из `requested_downloads`.
#[derive(Debug, Deserialize)]
struct YtDlpRequestedDownload {
    /// Составные adaptive streams: обычно video-only + audio-only.
    requested_formats: Option<Vec<YtDlpFormat>>,
}

/// Один конкретный media stream от `yt-dlp`.
#[derive(Debug, Clone, Deserialize)]
struct YtDlpFormat {
    /// Прямой media URL.
    url: String,

    /// Идентификатор формата, например `315` или `251`.
    format_id: Option<String>,

    /// Расширение/container.
    ext: Option<String>,

    /// Video codec или `none`.
    vcodec: Option<String>,

    /// Audio codec или `none`.
    acodec: Option<String>,

    /// Высота video stream.
    height: Option<u32>,

    /// FPS video stream.
    fps: Option<f64>,

    /// Размер stream, если известен.
    filesize: Option<u64>,

    /// Приблизительный размер stream, если точный неизвестен.
    filesize_approx: Option<u64>,

    /// Длительность конкретного adaptive stream, если yt-dlp её сообщает.
    duration: Option<f64>,

    /// HTTP headers, которые YouTube ожидает для этого URL.
    http_headers: Option<BTreeMap<String, String>>,
}

/// Вид adaptive stream-а внутри YouTube media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YoutubeStreamKind {
    /// Video-only WebM stream.
    Video,

    /// Audio-only WebM stream.
    Audio,
}

impl YoutubeStreamKind {
    /// Возвращает стабильную строку для логов и тестовых diagnostics.
    fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }
}

/// Нормализованный direct stream descriptor без yt-dlp-specific структуры.
#[derive(Debug, Clone)]
pub struct YoutubeDirectStreamDescriptor {
    /// Тип adaptive stream-а.
    pub kind: YoutubeStreamKind,

    /// Прямой media URL.
    pub url: String,

    /// HTTP headers, которые service layer получил от yt-dlp.
    pub headers: Vec<HttpHeader>,

    /// YouTube/yt-dlp format id, например `315` или `251`.
    pub format_id: Option<String>,

    /// Opaque id media в сервисе.
    pub service_media_id: Option<String>,

    /// HTTP validators, если их уже удалось получить через Range probe.
    pub validators: SourceValidators,

    /// Длительность stream/media, если extractor её знает.
    pub duration: Option<Duration>,

    /// Явный live-флаг: live не должен становиться seekable.
    pub live: bool,

    /// Описание stream для логов.
    pub description: String,
}

impl YoutubeDirectStreamDescriptor {
    /// Возвращает `true`, если stream имеет HTTP validators для стабильной byte identity.
    #[must_use]
    pub fn has_persistent_validators(&self) -> bool {
        self.validators.etag.is_some() || self.validators.last_modified.is_some()
    }
}

/// Нормализованная пара adaptive video/audio streams для одного YouTube media.
#[derive(Debug, Clone)]
pub struct YoutubeDirectStreams {
    /// Заголовок media, если extractor его сообщил.
    pub title: Option<String>,

    /// Opaque id media в сервисе.
    pub service_media_id: Option<String>,

    /// Итоговый format id выбранной adaptive пары.
    pub format_id: Option<String>,

    /// Высота выбранного video stream.
    pub height: Option<u32>,

    /// FPS выбранного video stream.
    pub fps: Option<f64>,

    /// Video codec выбранной пары.
    pub vcodec: Option<String>,

    /// Audio codec выбранной пары.
    pub acodec: Option<String>,

    /// Общая длительность VOD, если известна.
    pub duration: Option<Duration>,

    /// Общий live-флаг media.
    pub live: bool,

    /// Video-only stream descriptor.
    pub video: YoutubeDirectStreamDescriptor,

    /// Audio-only stream descriptor.
    pub audio: YoutubeDirectStreamDescriptor,
}

/// Результат подготовки YouTube ролика к streaming playback.
pub struct YoutubeStreamingMedia {
    /// Demuxer, который уже читает из HTTP-backed streaming sources.
    pub demuxer: Box<dyn webm_demux::Demuxer + Send>,

    /// Человекочитаемое описание выбранного YouTube формата.
    pub description: String,

    /// Нормализованные direct stream descriptors, выбранные service layer-ом.
    pub direct_streams: YoutubeDirectStreams,
}

/// Проверяет, похож ли CLI-аргумент на URL, который должен обрабатывать YouTube resolver.
#[must_use]
pub fn is_probably_url(argument: &str) -> bool {
    // Явно поддерживаем только web URL, чтобы локальные пути с двоеточиями не ломали CLI.
    argument.starts_with("https://") || argument.starts_with("http://")
}

/// Открывает YouTube URL как demuxer без предварительного скачивания файла.
pub fn open_streaming_media(
    video_url: &str,
    network_config: &NetworkConfig,
) -> Result<YoutubeStreamingMedia> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .context("Network config нельзя использовать для YouTube source")?;
    let resolver: Arc<dyn YoutubeDirectStreamResolver> = Arc::new(YtDlpDirectStreamResolver);
    let mut direct_streams = resolver.resolve_direct_streams(video_url)?;
    let description = build_streaming_description(&direct_streams);
    let demuxer = build_demuxer_from_direct_streams(
        video_url,
        &mut direct_streams,
        source_config,
        Arc::clone(&resolver),
    )?;

    Ok(YoutubeStreamingMedia {
        demuxer,
        description,
        direct_streams,
    })
}

/// Resolver direct stream descriptors для production и тестового refresh path.
trait YoutubeDirectStreamResolver: Send + Sync {
    /// Возвращает свежую пару direct stream descriptors для исходного YouTube URL.
    fn resolve_direct_streams(&self, video_url: &str) -> Result<YoutubeDirectStreams>;
}

/// Production resolver на базе `yt-dlp`.
struct YtDlpDirectStreamResolver;

impl YoutubeDirectStreamResolver for YtDlpDirectStreamResolver {
    fn resolve_direct_streams(&self, video_url: &str) -> Result<YoutubeDirectStreams> {
        resolve_youtube_direct_streams(video_url)
    }
}

/// Получает normalized descriptors через `yt-dlp`, не открывая media bytes.
pub fn resolve_youtube_direct_streams(video_url: &str) -> Result<YoutubeDirectStreams> {
    let metadata = resolve_youtube_metadata(video_url)?;

    select_direct_media_streams(&metadata)
}

/// Строит seekable Range demuxer для VOD или unseekable streaming fallback.
fn build_demuxer_from_direct_streams(
    original_video_url: &str,
    direct_streams: &mut YoutubeDirectStreams,
    source_config: SourceRuntimeConfig,
    resolver: Arc<dyn YoutubeDirectStreamResolver>,
) -> Result<Box<dyn webm_demux::Demuxer + Send>> {
    if direct_streams.live {
        tracing::info!("YouTube live stream открыт как not seekable streaming source");
        return open_unseekable_streaming_demuxer(direct_streams, &source_config);
    }

    match open_range_backed_demuxer(
        original_video_url,
        direct_streams,
        source_config.clone(),
        resolver,
    )? {
        Some(demuxer) => Ok(demuxer),
        None => open_unseekable_streaming_demuxer(direct_streams, &source_config),
    }
}

/// Открывает pair demuxer-ов поверх HTTP Range sources, если оба source seekable.
fn open_range_backed_demuxer(
    original_video_url: &str,
    direct_streams: &mut YoutubeDirectStreams,
    source_config: SourceRuntimeConfig,
    resolver: Arc<dyn YoutubeDirectStreamResolver>,
) -> Result<Option<Box<dyn webm_demux::Demuxer + Send>>> {
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

    direct_streams.video = video_source.descriptor.clone();
    direct_streams.video.validators = video_source.validators();
    direct_streams.audio = audio_source.descriptor.clone();
    direct_streams.audio.validators = audio_source.validators();

    if !video_source.seekability().is_seekable() || !audio_source.seekability().is_seekable() {
        tracing::warn!(
            video_seekability = ?video_source.seekability(),
            audio_seekability = ?audio_source.seekability(),
            "YouTube HTTP Range probe не подтвердил seek; fallback на playback-only streaming"
        );
        return Ok(None);
    }

    let video_source = CachedByteSource::new(video_source, &source_config);
    let audio_source = CachedByteSource::new(audio_source, &source_config);
    let video_demuxer =
        webm_demux::SymphoniaDemuxer::from_byte_source(video_source, "webm", "youtube-video")
            .context("Не удалось открыть Range-backed video WebM")?;
    let audio_demuxer =
        webm_demux::SymphoniaDemuxer::from_byte_source(audio_source, "webm", "youtube-audio")
            .context("Не удалось открыть Range-backed audio WebM")?;
    let demuxer = webm_demux::DualStreamDemuxer::new(video_demuxer, audio_demuxer)
        .context("Не удалось объединить Range-backed video/audio demuxer-ы")?;

    Ok(Some(Box::new(demuxer)))
}

/// Открывает старый playback-only path через последовательный HTTP body stream.
fn open_unseekable_streaming_demuxer(
    direct_streams: &YoutubeDirectStreams,
    source_config: &SourceRuntimeConfig,
) -> Result<Box<dyn webm_demux::Demuxer + Send>> {
    let (video_writer, video_reader) = webm_demux::StreamingByteReader::channel();
    let (audio_writer, audio_reader) = webm_demux::StreamingByteReader::channel();

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

    let video_demuxer =
        webm_demux::SymphoniaDemuxer::from_stream(video_reader, "webm", "youtube-video")
            .context("Не удалось открыть streaming video WebM")?;
    let audio_demuxer =
        webm_demux::SymphoniaDemuxer::from_stream(audio_reader, "webm", "youtube-audio")
            .context("Не удалось открыть streaming audio WebM")?;

    let demuxer = webm_demux::DualStreamDemuxer::new(video_demuxer, audio_demuxer)
        .context("Не удалось объединить streaming video/audio demuxer-ы")?;

    Ok(Box::new(demuxer))
}

/// Получает metadata и выбранные direct URLs через `yt-dlp`.
fn resolve_youtube_metadata(video_url: &str) -> Result<YtDlpMetadata> {
    // Выбираем selector из env или используем тестовую policy по умолчанию.
    let format_selector = youtube_format_selector();

    // `--simulate --dump-single-json` не скачивает media, но применяет format selection.
    let command_output = Command::new("yt-dlp")
        .arg("--quiet")
        .arg("--no-warnings")
        .arg("--simulate")
        .arg("--dump-single-json")
        .arg("--no-playlist")
        .arg("--format")
        .arg(&format_selector)
        .arg(video_url)
        .output()
        .context("Не удалось запустить yt-dlp для получения streaming metadata")?;

    // Любая ошибка selection/access должна стать понятной ошибкой UI.
    ensure_yt_dlp_success(command_output.status, &command_output.stderr)?;

    // JSON должен быть валидным UTF-8.
    let stdout_text = String::from_utf8(command_output.stdout)
        .context("yt-dlp вернул metadata stdout не в UTF-8")?;

    // Парсим typed JSON, чтобы не ходить по magic string paths руками.
    serde_json::from_str(&stdout_text).context("Не удалось разобрать JSON metadata от yt-dlp")
}

/// Выбирает прямые video/audio descriptors из metadata.
fn select_direct_media_streams(metadata: &YtDlpMetadata) -> Result<YoutubeDirectStreams> {
    let requested_formats = metadata
        .requested_downloads
        .as_ref()
        .and_then(|downloads| downloads.first())
        .and_then(|download| download.requested_formats.as_ref())
        .or(metadata.requested_formats.as_ref())
        .context("yt-dlp metadata не содержит requested_formats для streaming")?;

    // Video stream имеет настоящий vcodec и не имеет audio codec.
    let video_format = requested_formats
        .iter()
        .find(|format| {
            format
                .vcodec
                .as_deref()
                .is_some_and(|codec| codec != "none")
                && format.acodec.as_deref().unwrap_or("none") == "none"
        })
        .cloned()
        .context("yt-dlp не вернул video-only stream URL")?;

    // Audio stream имеет настоящий acodec и не имеет video codec.
    let audio_format = requested_formats
        .iter()
        .find(|format| {
            format
                .acodec
                .as_deref()
                .is_some_and(|codec| codec != "none")
                && format.vcodec.as_deref().unwrap_or("none") == "none"
        })
        .cloned()
        .context("yt-dlp не вернул audio-only stream URL")?;

    let service_media_id = metadata.id.clone();
    let duration = duration_from_seconds(metadata.duration);
    let live = metadata_is_live(metadata);

    Ok(YoutubeDirectStreams {
        title: metadata.title.clone(),
        service_media_id: service_media_id.clone(),
        format_id: metadata.format_id.clone(),
        height: metadata.height,
        fps: metadata.fps,
        vcodec: metadata.vcodec.clone(),
        acodec: metadata.acodec.clone(),
        duration,
        live,
        video: direct_stream_from_format(
            YoutubeStreamKind::Video,
            video_format,
            service_media_id.clone(),
            duration,
            live,
        ),
        audio: direct_stream_from_format(
            YoutubeStreamKind::Audio,
            audio_format,
            service_media_id,
            duration,
            live,
        ),
    })
}

/// Преобразует JSON format в runtime stream descriptor.
fn direct_stream_from_format(
    kind: YoutubeStreamKind,
    format: YtDlpFormat,
    service_media_id: Option<String>,
    media_duration: Option<Duration>,
    live: bool,
) -> YoutubeDirectStreamDescriptor {
    let size = format
        .filesize
        .or(format.filesize_approx)
        .map(|bytes| format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0))
        .unwrap_or_else(|| "unknown size".to_string());

    let description = format!(
        "{} format={} ext={} vcodec={} acodec={} height={} fps={} size={}",
        kind.as_str(),
        format.format_id.as_deref().unwrap_or("unknown"),
        format.ext.as_deref().unwrap_or("unknown"),
        format.vcodec.as_deref().unwrap_or("unknown"),
        format.acodec.as_deref().unwrap_or("unknown"),
        format
            .height
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        format
            .fps
            .map(|value| format!("{value:.0}"))
            .unwrap_or_else(|| "none".to_string()),
        size,
    );
    let duration = duration_from_seconds(format.duration).or(media_duration);
    let headers = format
        .http_headers
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| HttpHeader::new(name, value))
        .collect();

    YoutubeDirectStreamDescriptor {
        kind,
        url: format.url,
        headers,
        format_id: format.format_id,
        service_media_id,
        validators: SourceValidators::default(),
        duration,
        live,
        description,
    }
}

/// Контекст refresh-а direct URL, ограниченный одним stream-ом.
#[derive(Clone)]
struct RefreshContext {
    /// Исходный URL страницы/ролика, который понимает yt-dlp.
    original_video_url: String,

    /// Какой stream нужно достать из свежей пары descriptors.
    stream_kind: YoutubeStreamKind,

    /// Resolver, который умеет заново получить direct URLs.
    resolver: Arc<dyn YoutubeDirectStreamResolver>,
}

/// HTTP Range source с одноразовым refresh-ом direct URL через service layer.
struct YoutubeRefreshingRangeSource {
    /// Текущий direct stream descriptor.
    descriptor: YoutubeDirectStreamDescriptor,

    /// Настройки HTTP/cache layer из пользовательского config.
    source_config: SourceRuntimeConfig,

    /// Активный neutral HTTP Range source.
    inner: HttpRangeSource,

    /// Контекст, через который можно один раз получить свежий direct URL.
    refresh_context: RefreshContext,

    /// Гарантия bounded retry: refresh выполняется максимум один раз.
    refresh_attempted: bool,
}

impl YoutubeRefreshingRangeSource {
    /// Открывает Range source; если initial direct URL уже истёк, refresh выполняется один раз.
    fn open(
        descriptor: YoutubeDirectStreamDescriptor,
        source_config: SourceRuntimeConfig,
        refresh_context: RefreshContext,
    ) -> Result<Self> {
        match open_http_range_source(&descriptor, source_config.clone()) {
            Ok(inner) => Ok(Self {
                descriptor,
                source_config,
                inner,
                refresh_context,
                refresh_attempted: false,
            }),
            Err(error) if direct_url_may_be_expired(&error) => {
                let refreshed_descriptor = refresh_descriptor(&refresh_context)
                    .context("Не удалось обновить истёкший YouTube direct URL")?;
                let inner = open_http_range_source(&refreshed_descriptor, source_config.clone())
                    .context("Не удалось открыть обновлённый YouTube HTTP Range source")?;
                Ok(Self {
                    descriptor: refreshed_descriptor,
                    source_config,
                    inner,
                    refresh_context,
                    refresh_attempted: true,
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Повторяет текущую read-позицию после успешного refresh-а direct URL.
    fn refresh_once_at_position(&mut self, position: u64) -> bool {
        if self.refresh_attempted {
            return false;
        }

        self.refresh_attempted = true;
        let refreshed_descriptor = match refresh_descriptor(&self.refresh_context) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                tracing::warn!(error = %error, "YouTube direct URL refresh failed");
                return false;
            }
        };
        let mut refreshed_source =
            match open_http_range_source(&refreshed_descriptor, self.source_config.clone()) {
                Ok(source) => source,
                Err(error) => {
                    tracing::warn!(error = %error, "Updated YouTube HTTP Range source open failed");
                    return false;
                }
            };

        if let Err(error) = refreshed_source.seek(position) {
            tracing::warn!(error = %error, position, "Updated YouTube source seek failed");
            return false;
        }

        self.descriptor = refreshed_descriptor;
        self.inner = refreshed_source;
        true
    }
}

impl ByteSource for YoutubeRefreshingRangeSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        let retry_position = self.inner.position();
        match self.inner.read(output, cancellation) {
            Ok(bytes_read) => Ok(bytes_read),
            Err(error) if direct_url_may_be_expired(&error) => {
                if self.refresh_once_at_position(retry_position) {
                    self.inner.read(output, cancellation)
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.inner.seek(offset)
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn seekability(&self) -> Seekability {
        if self.descriptor.live {
            return Seekability::NotSeekable {
                reason: source_core::NotSeekableReason::Unknown,
            };
        }

        self.inner.seekability()
    }

    fn validators(&self) -> SourceValidators {
        self.inner.validators()
    }

    fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.inner.fingerprint()
    }
}

/// Открывает нейтральный HTTP Range source из service descriptor-а.
fn open_http_range_source(
    descriptor: &YoutubeDirectStreamDescriptor,
    source_config: SourceRuntimeConfig,
) -> SourceResult<HttpRangeSource> {
    HttpRangeSource::open(HttpRangeSourceConfig::new(
        descriptor.url.clone(),
        descriptor.headers.clone(),
        source_config,
    ))
}

/// Возвращает свежий descriptor нужного stream kind-а.
fn refresh_descriptor(refresh_context: &RefreshContext) -> Result<YoutubeDirectStreamDescriptor> {
    let refreshed_streams = refresh_context
        .resolver
        .resolve_direct_streams(&refresh_context.original_video_url)?;

    Ok(match refresh_context.stream_kind {
        YoutubeStreamKind::Video => refreshed_streams.video,
        YoutubeStreamKind::Audio => refreshed_streams.audio,
    })
}

/// Определяет HTTP failure, похожий на истёкший direct URL.
fn direct_url_may_be_expired(error: &SourceError) -> bool {
    match error {
        SourceError::HttpStatus { status, .. } => matches!(
            *status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ),
        SourceError::HttpRequest { .. } => true,
        _ => false,
    }
}

/// Конвертирует секунды yt-dlp в `Duration`, отбрасывая некорректные значения.
fn duration_from_seconds(seconds: Option<f64>) -> Option<Duration> {
    let seconds = seconds?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }

    Some(Duration::from_secs_f64(seconds))
}

/// Определяет live media без превращения бывших live VOD в live.
fn metadata_is_live(metadata: &YtDlpMetadata) -> bool {
    metadata.is_live.unwrap_or(false)
        || matches!(
            metadata.live_status.as_deref(),
            Some("is_live" | "is_upcoming")
        )
}

/// Запускает потоковую загрузку одного direct media URL.
fn spawn_http_fetcher(
    thread_name: &'static str,
    stream: YoutubeDirectStreamDescriptor,
    source_config: SourceRuntimeConfig,
    writer: webm_demux::StreamingByteWriter,
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
    writer: &webm_demux::StreamingByteWriter,
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

/// Возвращает selector `yt-dlp` для выбора тестового YouTube формата.
fn youtube_format_selector() -> String {
    // Env override нужен для ручных тестов новых кодеков/разрешений без перекомпиляции.
    if let Ok(configured_selector) = std::env::var(FORMAT_SELECTOR_ENV) {
        // Пустая строка почти всегда ошибка конфигурации, поэтому не используем её.
        if !configured_selector.trim().is_empty() {
            return configured_selector;
        }
    }

    // По умолчанию используем ограниченный VP9/Opus WebM selector для текущего pipeline.
    DEFAULT_FORMAT_SELECTOR.to_string()
}

/// Преобразует неуспешный exit code `yt-dlp` в читаемую ошибку.
fn ensure_yt_dlp_success(status: ExitStatus, stderr_bytes: &[u8]) -> Result<()> {
    // Нулевой exit code означает, что `yt-dlp` считает selection успешным.
    if status.success() {
        return Ok(());
    }

    // stderr сохраняем максимально информативным, но не паникуем на битом UTF-8.
    let stderr_text = String::from_utf8_lossy(stderr_bytes);

    anyhow::bail!(
        "yt-dlp не смог выбрать поддерживаемый SDR VP9/Opus WebM поток (4K60, 4K30, 1080p60 или 1080p): {}",
        stderr_text.trim()
    );
}

/// Формирует описание выбранного streaming формата.
fn build_streaming_description(direct_streams: &YoutubeDirectStreams) -> String {
    let title = direct_streams.title.as_deref().unwrap_or("YouTube video");
    let video_id = direct_streams
        .service_media_id
        .as_deref()
        .unwrap_or("unknown id");
    let format_id = direct_streams
        .format_id
        .as_deref()
        .unwrap_or("unknown format");
    let height = direct_streams
        .height
        .map(|value| format!("{value}p"))
        .unwrap_or_else(|| "unknown height".to_string());
    let fps = direct_streams
        .fps
        .map(|value| format!("{value:.0}fps"))
        .unwrap_or_else(|| "unknown fps".to_string());
    let vcodec = direct_streams
        .vcodec
        .as_deref()
        .unwrap_or("unknown video codec");
    let acodec = direct_streams
        .acodec
        .as_deref()
        .unwrap_or("unknown audio codec");
    let duration = direct_streams
        .duration
        .map(|value| format!("{}s", value.as_secs()))
        .unwrap_or_else(|| "unknown duration".to_string());
    let playback_kind = if direct_streams.live { "live" } else { "vod" };

    format!(
        "{title} [{video_id}] {playback_kind} {format_id} - {height} {fps}, {vcodec} + {acodec}, {duration}; {}; {}",
        direct_streams.video.description, direct_streams.audio.description
    )
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

    use media_core::TimelineNotSeekableReason;
    use source_core::ByteSource;
    use webm_demux::DemuxSeekability;

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

    fn test_source_config() -> SourceRuntimeConfig {
        SourceRuntimeConfig::from_network_config(&NetworkConfig {
            memory_cache_mb: 1,
            read_ahead_mb: 1,
            connect_timeout_ms: 1_000,
            read_timeout_ms: 1_000,
        })
        .expect("source config")
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

    fn test_webm_bytes() -> Arc<Vec<u8>> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/test.webm");
        Arc::new(std::fs::read(path).expect("test webm bytes"))
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
                        height: Some(1080),
                        fps: Some(60.0),
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
                        height: None,
                        fps: None,
                        filesize: None,
                        filesize_approx: Some(5),
                        duration: Some(42.0),
                        http_headers: None,
                    },
                ]),
            }]),
            requested_formats: None,
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
        let media = test_webm_bytes();
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
            resolver,
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
        let media = test_webm_bytes();
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
            resolver,
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
        let media = test_webm_bytes();
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
            resolver,
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
