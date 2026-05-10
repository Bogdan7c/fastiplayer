//! Временный YouTube resolver на базе `yt-dlp`.
//!
//! Этот crate является service boundary: он знает про YouTube/yt-dlp format
//! selection и HTTP headers, но не знает про UI, renderer или внутренний state
//! player-а. `app-egui` получает отсюда уже готовый streaming demuxer.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;

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

    /// HTTP headers, которые YouTube ожидает для этого URL.
    http_headers: Option<BTreeMap<String, String>>,
}

/// Direct media stream, готовый к HTTP fetching.
#[derive(Debug, Clone)]
struct DirectMediaStream {
    /// Прямой media URL.
    url: String,

    /// HTTP headers из `yt-dlp`.
    http_headers: BTreeMap<String, String>,

    /// Описание stream для логов.
    description: String,
}

/// Результат подготовки YouTube ролика к streaming playback.
pub struct YoutubeStreamingMedia {
    /// Demuxer, который уже читает из HTTP-backed streaming sources.
    pub demuxer: Box<dyn webm_demux::Demuxer>,

    /// Человекочитаемое описание выбранного YouTube формата.
    pub description: String,
}

/// Проверяет, похож ли CLI-аргумент на URL, который должен обрабатывать YouTube resolver.
#[must_use]
pub fn is_probably_url(argument: &str) -> bool {
    // Явно поддерживаем только web URL, чтобы локальные пути с двоеточиями не ломали CLI.
    argument.starts_with("https://") || argument.starts_with("http://")
}

/// Открывает YouTube URL как streaming demuxer без предварительного скачивания файла.
pub fn open_streaming_media(video_url: &str) -> Result<YoutubeStreamingMedia> {
    // Получаем direct URLs и metadata, но не скачиваем media через yt-dlp.
    let metadata = resolve_youtube_metadata(video_url)?;

    // Выбираем video/audio stream из yt-dlp requested formats.
    let (video_stream, audio_stream) = select_direct_media_streams(&metadata)?;

    // Создаём bounded streaming channels для video и audio.
    let (video_writer, video_reader) = webm_demux::StreamingByteReader::channel();
    let (audio_writer, audio_reader) = webm_demux::StreamingByteReader::channel();

    // Запускаем HTTP fetchers до открытия demuxer-ов, чтобы probe мог дождаться первых bytes.
    spawn_http_fetcher("youtube-video", video_stream.clone(), video_writer)?;
    spawn_http_fetcher("youtube-audio", audio_stream.clone(), audio_writer)?;

    // Открываем отдельные WebM demuxer-ы поверх blocking streaming readers.
    let video_demuxer =
        webm_demux::SymphoniaDemuxer::from_stream(video_reader, "webm", "youtube-video")
            .context("Не удалось открыть streaming video WebM")?;
    let audio_demuxer =
        webm_demux::SymphoniaDemuxer::from_stream(audio_reader, "webm", "youtube-audio")
            .context("Не удалось открыть streaming audio WebM")?;

    // Объединяем video-only и audio-only WebM в один Demuxer для текущего app loop.
    let demuxer = webm_demux::DualStreamDemuxer::new(video_demuxer, audio_demuxer)
        .context("Не удалось объединить streaming video/audio demuxer-ы")?;

    Ok(YoutubeStreamingMedia {
        demuxer: Box::new(demuxer),
        description: build_streaming_description(&metadata, &video_stream, &audio_stream),
    })
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

/// Выбирает прямые video/audio URLs из metadata.
fn select_direct_media_streams(
    metadata: &YtDlpMetadata,
) -> Result<(DirectMediaStream, DirectMediaStream)> {
    // Берём `requested_downloads[0].requested_formats`, если оно есть.
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

    Ok((
        direct_stream_from_format("video", video_format),
        direct_stream_from_format("audio", audio_format),
    ))
}

/// Преобразует JSON format в runtime stream descriptor.
fn direct_stream_from_format(kind: &str, format: YtDlpFormat) -> DirectMediaStream {
    // Размер полезен в логах; для streaming он не обязателен.
    let size = format
        .filesize
        .or(format.filesize_approx)
        .map(|bytes| format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0))
        .unwrap_or_else(|| "unknown size".to_string());

    // Описание оставляем компактным, чтобы не засорять tracing.
    let description = format!(
        "{} format={} ext={} vcodec={} acodec={} height={} fps={} size={}",
        kind,
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

    DirectMediaStream {
        url: format.url,
        http_headers: format.http_headers.unwrap_or_default(),
        description,
    }
}

/// Запускает потоковую загрузку одного direct media URL.
fn spawn_http_fetcher(
    thread_name: &'static str,
    stream: DirectMediaStream,
    writer: webm_demux::StreamingByteWriter,
) -> Result<()> {
    // Отдельный OS thread достаточно прост для MVP и не требует async runtime.
    thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            if let Err(fetch_error) = fetch_stream_to_writer(&stream, &writer) {
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
    stream: &DirectMediaStream,
    writer: &webm_demux::StreamingByteWriter,
) -> Result<()> {
    tracing::info!(description = %stream.description, "HTTP streaming fetch started");

    // Client создаётся внутри thread, чтобы не тащить lifetime/ownership наружу.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .context("Не удалось создать reqwest blocking client")?;

    // Добавляем headers, которые yt-dlp получил от YouTube extractor.
    let headers = build_header_map(&stream.http_headers)?;

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

/// Конвертирует headers из yt-dlp JSON в reqwest HeaderMap.
fn build_header_map(headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
    let mut header_map = HeaderMap::new();

    for (name, value) in headers {
        // Header name/value валидируем явно, чтобы ошибки metadata были понятными.
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("Некорректный HTTP header name от yt-dlp: {name}"))?;
        let header_value = HeaderValue::from_str(value)
            .with_context(|| format!("Некорректное значение HTTP header от yt-dlp: {name}"))?;
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
fn build_streaming_description(
    metadata: &YtDlpMetadata,
    video_stream: &DirectMediaStream,
    audio_stream: &DirectMediaStream,
) -> String {
    // Название и id помогают сопоставить логи с конкретным YouTube URL.
    let title = metadata.title.as_deref().unwrap_or("YouTube video");
    let video_id = metadata.id.as_deref().unwrap_or("unknown id");

    // Итоговый формат показывает, какой pair выбрал yt-dlp.
    let format_id = metadata.format_id.as_deref().unwrap_or("unknown format");
    let height = metadata
        .height
        .map(|value| format!("{value}p"))
        .unwrap_or_else(|| "unknown height".to_string());
    let fps = metadata
        .fps
        .map(|value| format!("{value:.0}fps"))
        .unwrap_or_else(|| "unknown fps".to_string());
    let vcodec = metadata.vcodec.as_deref().unwrap_or("unknown video codec");
    let acodec = metadata.acodec.as_deref().unwrap_or("unknown audio codec");

    format!(
        "{title} [{video_id}] streaming {format_id} - {height} {fps}, {vcodec} + {acodec}; {}; {}",
        video_stream.description, audio_stream.description
    )
}
