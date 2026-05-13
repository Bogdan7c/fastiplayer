use std::time::Duration;

use anyhow::{Context, Result};
use rustiplayer_config::YoutubeConfig;
use source_core::{HttpHeader, SourceValidators};

use crate::dto::{
    YoutubeDirectStreamDescriptor, YoutubeDirectStreams, YoutubeStreamKind, YtDlpFormat,
    YtDlpMetadata,
};
use crate::process::{YtDlpProcessConfig, resolve_youtube_metadata};

/// Resolver direct stream descriptors для production и тестового refresh path.
pub(crate) trait YoutubeDirectStreamResolver: Send + Sync {
    /// Возвращает свежую пару direct stream descriptors для исходного YouTube URL.
    fn resolve_direct_streams(&self, video_url: &str) -> Result<YoutubeDirectStreams>;
}

/// Production resolver на базе `yt-dlp`.
pub(crate) struct YtDlpDirectStreamResolver {
    /// Process policy для всех metadata refresh-ов этого resolver-а.
    process_config: YtDlpProcessConfig,
}

impl YtDlpDirectStreamResolver {
    /// Создаёт production resolver из пользовательского YouTube config.
    pub(crate) fn from_youtube_config(youtube_config: &YoutubeConfig) -> Result<Self> {
        Ok(Self {
            process_config: YtDlpProcessConfig::from_youtube_config(youtube_config)?,
        })
    }
}

impl YoutubeDirectStreamResolver for YtDlpDirectStreamResolver {
    fn resolve_direct_streams(&self, video_url: &str) -> Result<YoutubeDirectStreams> {
        resolve_youtube_direct_streams_with_process_config(video_url, &self.process_config)
    }
}

/// Получает normalized descriptors через `yt-dlp`, не открывая media bytes.
pub fn resolve_youtube_direct_streams(video_url: &str) -> Result<YoutubeDirectStreams> {
    let process_config = YtDlpProcessConfig::from_youtube_config(&YoutubeConfig::default())?;

    resolve_youtube_direct_streams_with_process_config(video_url, &process_config)
}

/// Получает normalized descriptors через `yt-dlp` с уже валидированной process policy.
fn resolve_youtube_direct_streams_with_process_config(
    video_url: &str,
    process_config: &YtDlpProcessConfig,
) -> Result<YoutubeDirectStreams> {
    let metadata = resolve_youtube_metadata(video_url, process_config)?;

    select_direct_media_streams(&metadata)
}

/// Выбирает прямые video/audio descriptors из metadata.
pub(crate) fn select_direct_media_streams(
    metadata: &YtDlpMetadata,
) -> Result<YoutubeDirectStreams> {
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

/// Формирует описание выбранного streaming формата.
pub(crate) fn build_streaming_description(direct_streams: &YoutubeDirectStreams) -> String {
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
