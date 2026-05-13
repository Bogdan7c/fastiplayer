use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use source_core::{HttpHeader, SourceValidators};

/// Минимальная metadata по выбранному YouTube формату.
#[derive(Debug, Deserialize)]
pub(crate) struct YtDlpMetadata {
    /// Заголовок ролика для логов.
    pub(crate) title: Option<String>,

    /// YouTube id для логов.
    pub(crate) id: Option<String>,

    /// Итоговый выбранный combined format.
    pub(crate) format_id: Option<String>,

    /// Высота выбранного video stream.
    pub(crate) height: Option<u32>,

    /// FPS выбранного video stream.
    pub(crate) fps: Option<f64>,

    /// Video codec выбранного stream.
    pub(crate) vcodec: Option<String>,

    /// Audio codec выбранного stream.
    pub(crate) acodec: Option<String>,

    /// Длительность VOD в секундах, если extractor её знает.
    pub(crate) duration: Option<f64>,

    /// Флаг live-трансляции от extractor-а.
    pub(crate) is_live: Option<bool>,

    /// Текстовый live status от yt-dlp для новых extractor-ов.
    pub(crate) live_status: Option<String>,

    /// Подробности выбранных adaptive streams после format selection.
    pub(crate) requested_downloads: Option<Vec<YtDlpRequestedDownload>>,

    /// Fallback-поле: некоторые версии yt-dlp кладут выбранные streams сюда.
    pub(crate) requested_formats: Option<Vec<YtDlpFormat>>,
}

/// Один download candidate из `requested_downloads`.
#[derive(Debug, Deserialize)]
pub(crate) struct YtDlpRequestedDownload {
    /// Составные adaptive streams: обычно video-only + audio-only.
    pub(crate) requested_formats: Option<Vec<YtDlpFormat>>,
}

/// Один конкретный media stream от `yt-dlp`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct YtDlpFormat {
    /// Прямой media URL.
    pub(crate) url: String,

    /// Идентификатор формата, например `315` или `251`.
    pub(crate) format_id: Option<String>,

    /// Расширение/container.
    pub(crate) ext: Option<String>,

    /// Video codec или `none`.
    pub(crate) vcodec: Option<String>,

    /// Audio codec или `none`.
    pub(crate) acodec: Option<String>,

    /// Высота video stream.
    pub(crate) height: Option<u32>,

    /// FPS video stream.
    pub(crate) fps: Option<f64>,

    /// Размер stream, если известен.
    pub(crate) filesize: Option<u64>,

    /// Приблизительный размер stream, если точный неизвестен.
    pub(crate) filesize_approx: Option<u64>,

    /// Длительность конкретного adaptive stream, если yt-dlp её сообщает.
    pub(crate) duration: Option<f64>,

    /// HTTP headers, которые YouTube ожидает для этого URL.
    pub(crate) http_headers: Option<BTreeMap<String, String>>,
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
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
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
