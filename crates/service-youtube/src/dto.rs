use std::collections::BTreeMap;
use std::time::Duration;

use codec_core::VideoDecodeRequirement;
use serde::Deserialize;
use source_core::{HttpHeader, SourceValidators};

use crate::YoutubeDirectStreamUrl;

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

    /// Полный manifest formats для capability-aware выбора без service default selector-а.
    pub(crate) formats: Option<Vec<YtDlpFormat>>,
}

/// Один download candidate из `requested_downloads`.
#[derive(Debug, Deserialize)]
pub(crate) struct YtDlpRequestedDownload {
    /// Составные adaptive streams: обычно video-only + audio-only.
    pub(crate) requested_formats: Option<Vec<YtDlpFormat>>,
}

/// Один конкретный media stream от `yt-dlp`.
#[derive(Clone, Deserialize)]
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

    /// Ширина video stream.
    pub(crate) width: Option<u32>,

    /// Высота video stream.
    pub(crate) height: Option<u32>,

    /// FPS video stream.
    pub(crate) fps: Option<f64>,

    /// Общий bitrate stream-а в Kbit/s, если yt-dlp его знает.
    pub(crate) tbr: Option<f64>,

    /// Video bitrate stream-а в Kbit/s, если yt-dlp его знает.
    pub(crate) vbr: Option<f64>,

    /// Audio bitrate stream-а в Kbit/s, если yt-dlp его знает.
    pub(crate) abr: Option<f64>,

    /// Текстовый dynamic range hint от yt-dlp, например `SDR` или `HDR`.
    pub(crate) dynamic_range: Option<String>,

    /// Размер stream, если известен.
    pub(crate) filesize: Option<u64>,

    /// Приблизительный размер stream, если точный неизвестен.
    pub(crate) filesize_approx: Option<u64>,

    /// Длительность конкретного adaptive stream, если yt-dlp её сообщает.
    pub(crate) duration: Option<f64>,

    /// HTTP headers, которые YouTube ожидает для этого URL.
    pub(crate) http_headers: Option<BTreeMap<String, String>>,
}

impl std::fmt::Debug for YtDlpFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("YtDlpFormat")
            .field("url", &"<redacted>")
            .field("format_id", &self.format_id)
            .field("ext", &self.ext)
            .field("vcodec", &self.vcodec)
            .field("acodec", &self.acodec)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("fps", &self.fps)
            .field("dynamic_range", &self.dynamic_range)
            .field(
                "http_headers",
                &self.http_headers.as_ref().map(|headers| headers.keys()),
            )
            .finish_non_exhaustive()
    }
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
    pub url: YoutubeDirectStreamUrl,

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

/// Набор YouTube stream candidates без открытия media bytes.
#[derive(Debug, Clone)]
pub struct YoutubeStreamCandidates {
    /// Заголовок media, если extractor его сообщил.
    pub title: Option<String>,

    /// Opaque id media в сервисе.
    pub service_media_id: Option<String>,

    /// Общая длительность VOD, если известна.
    pub duration: Option<Duration>,

    /// Общий live-флаг media.
    pub live: bool,

    /// Нормализованные service candidates для будущего capability selection.
    pub candidates: Vec<YoutubeStreamCandidate>,
}

/// Один service-level video candidate с direct descriptors для позднего открытия.
#[derive(Debug, Clone)]
pub struct YoutubeStreamCandidate {
    /// Stable stream id внутри одного YouTube media manifest-а.
    pub stream_id: String,

    /// Composite format id пары video/audio, если yt-dlp сообщил format id.
    pub format_id: Option<String>,

    /// Video-only direct stream descriptor.
    pub video: YoutubeDirectStreamDescriptor,

    /// Audio-only direct stream descriptor, выбранный как service companion для video.
    pub audio: Option<YoutubeDirectStreamDescriptor>,

    /// Высота video stream-а из service manifest-а.
    pub height: Option<u32>,

    /// FPS video stream-а из service manifest-а.
    pub fps: Option<f64>,

    /// Raw video codec tag из service manifest-а.
    pub vcodec: Option<String>,

    /// Raw audio codec tag выбранного companion stream-а.
    pub acodec: Option<String>,

    /// Нормализованный dynamic range из отдельного service metadata field-а.
    pub dynamic_range: YoutubeDynamicRange,

    /// Требование к capability-core или typed причина, почему metadata недостаточна.
    pub video_requirement: YoutubeVideoRequirement,

    /// Чем больше значение, тем предпочтительнее stream при равной поддержке.
    pub quality_score: i64,
}

/// Достоверность dynamic range, которую service adapter получил из manifest metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YoutubeDynamicRange {
    /// Manifest явно классифицировал stream как SDR.
    Sdr,

    /// Manifest явно классифицировал stream как HDR/PQ/HLG.
    Hdr,

    /// Поле отсутствует, неизвестно или противоречит typed color metadata.
    Unknown,
}

/// Состояние конвертации service metadata в codec/capability requirement.
#[derive(Debug, Clone)]
pub enum YoutubeVideoRequirement {
    /// Metadata достаточно точна для передачи в capability-core.
    Ready(VideoDecodeRequirement),

    /// Metadata распознана частично или отсутствует; guessing запрещён.
    Insufficient(YoutubeInsufficientVideoMetadata),
}

impl YoutubeVideoRequirement {
    /// Возвращает `true`, если candidate можно передать в capability-core.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    /// Возвращает requirement только для ready candidate-а.
    #[must_use]
    pub const fn as_requirement(&self) -> Option<&VideoDecodeRequirement> {
        match self {
            Self::Ready(requirement) => Some(requirement),
            Self::Insufficient(_) => None,
        }
    }

    /// Возвращает причину insufficiency без разворачивания enum-а вызывающим кодом.
    #[must_use]
    pub fn insufficient_reason(&self) -> Option<&str> {
        match self {
            Self::Ready(_) => None,
            Self::Insufficient(metadata) => Some(metadata.reason.as_str()),
        }
    }
}

/// Диагностика candidate-а, который нельзя безопасно выбрать по capabilities.
#[derive(Debug, Clone)]
pub struct YoutubeInsufficientVideoMetadata {
    /// Человекочитаемая причина, почему service не построил полный requirement.
    pub reason: String,

    /// Частично распознанное требование, если codec/profile уже удалось нормализовать.
    pub partial_requirement: Option<VideoDecodeRequirement>,
}

/// Результат подготовки YouTube ролика к streaming playback.
pub struct YoutubeStreamingMedia {
    /// Demuxer, который уже читает из HTTP-backed streaming sources.
    pub demuxer: Box<dyn symphonia_demux::Demuxer + Send>,

    /// Человекочитаемое описание выбранного YouTube формата.
    pub description: String,

    /// Нормализованные direct stream descriptors, выбранные service layer-ом.
    pub direct_streams: YoutubeDirectStreams,
}

impl YoutubeStreamingMedia {
    /// Возвращает metadata выбранного service snapshot-а без повторного `yt-dlp` I/O.
    #[must_use]
    pub fn playlist_metadata(&self) -> crate::YoutubePlaylistMetadata {
        crate::YoutubePlaylistMetadata::from_extractor(
            self.direct_streams.title.clone(),
            self.direct_streams.duration,
        )
    }
}
