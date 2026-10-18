//! Безопасные (без секретов) типы границы загрузки субтитров.

use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

use service_ytdlp::{YtDlpSubtitleFormat, YtDlpSubtitleTrack};
use thiserror::Error;
use web_media_core::{LanguageTag, SubtitleKind};

/// Стабильный идентификатор дорожки: вид + язык.
///
/// Не зависит от конкретного извлечения, поэтому переживает повторный запрос
/// к yt-dlp и годится как ключ «выбор дорожки для этого видео».
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WebSubtitleTrackId {
    /// Вид дорожки.
    kind: SubtitleKind,
    /// Язык дорожки (для автоперевода — целевой).
    language: LanguageTag,
}

impl WebSubtitleTrackId {
    /// Собирает идентификатор из вида и языка.
    #[must_use]
    pub fn new(kind: SubtitleKind, language: LanguageTag) -> Self {
        Self { kind, language }
    }

    /// Вид дорожки.
    #[must_use]
    pub const fn kind(&self) -> SubtitleKind {
        self.kind
    }

    /// Язык дорожки.
    #[must_use]
    pub const fn language(&self) -> &LanguageTag {
        &self.language
    }
}

/// Формат субтитров, в котором вернулись байты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebSubtitleFormat {
    /// YouTube srv3 (XML).
    Srv3,
    /// WebVTT.
    WebVtt,
}

impl From<YtDlpSubtitleFormat> for WebSubtitleFormat {
    fn from(format: YtDlpSubtitleFormat) -> Self {
        match format {
            YtDlpSubtitleFormat::Srv3 => Self::Srv3,
            YtDlpSubtitleFormat::WebVtt => Self::WebVtt,
        }
    }
}

/// Безопасное описание дорожки для меню и памяти выбора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSubtitleTrack {
    /// Стабильный идентификатор.
    id: WebSubtitleTrackId,
    /// Название от источника (например, «Russian»).
    display_name: Option<String>,
    /// Форматы по убыванию предпочтения.
    formats: Vec<WebSubtitleFormat>,
}

impl WebSubtitleTrack {
    /// Проецирует сервисную дорожку без ссылок. `None`, если у дорожки нет языка.
    pub(crate) fn from_service_track(track: &YtDlpSubtitleTrack) -> Option<Self> {
        let language = track.language()?.clone();
        Some(Self {
            id: WebSubtitleTrackId::new(track.kind(), language),
            display_name: track
                .descriptor()
                .display_name()
                .map(|name| name.as_str().to_owned()),
            formats: track
                .sources()
                .iter()
                .map(|source| source.format().into())
                .collect(),
        })
    }

    /// Стабильный идентификатор дорожки.
    #[must_use]
    pub const fn id(&self) -> &WebSubtitleTrackId {
        &self.id
    }

    /// Вид дорожки.
    #[must_use]
    pub const fn kind(&self) -> SubtitleKind {
        self.id.kind
    }

    /// Язык дорожки.
    #[must_use]
    pub const fn language(&self) -> &LanguageTag {
        &self.id.language
    }

    /// Название от источника.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// Доступные форматы, предпочтительный первым.
    #[must_use]
    pub fn formats(&self) -> &[WebSubtitleFormat] {
        &self.formats
    }
}

/// Лимиты одной загрузки; значения приходят из конфигурации вызывающего.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSubtitleFetchLimits {
    /// Максимальный размер тела ответа в байтах.
    max_bytes: NonZeroUsize,
}

impl WebSubtitleFetchLimits {
    /// Задаёт максимальный размер тела ответа.
    #[must_use]
    pub const fn with_max_bytes(max_bytes: NonZeroUsize) -> Self {
        Self { max_bytes }
    }

    /// Максимальный размер тела ответа.
    #[must_use]
    pub const fn max_bytes(self) -> NonZeroUsize {
        self.max_bytes
    }
}

/// Скачанные субтитры: сырые байты и формат, в котором они пришли.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchedWebSubtitle {
    /// Тело ответа как есть, разбирать будет `subtitle-formats`.
    bytes: Vec<u8>,
    /// Формат тела.
    format: WebSubtitleFormat,
}

impl FetchedWebSubtitle {
    pub(super) const fn new(bytes: Vec<u8>, format: WebSubtitleFormat) -> Self {
        Self { bytes, format }
    }

    /// Тело ответа.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Передаёт тело владельцу разбора.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Формат тела.
    #[must_use]
    pub const fn format(&self) -> WebSubtitleFormat {
        self.format
    }
}

impl fmt::Debug for FetchedWebSubtitle {
    /// Только размер и формат: содержимое реплик в логи не идёт.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FetchedWebSubtitle")
            .field("byte_count", &self.bytes.len())
            .field("format", &self.format)
            .finish()
    }
}

/// Почему субтитры не загрузились. Ни один вариант не содержит URL или query.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WebSubtitleFetchError {
    /// Такой дорожки нет в списке этого видео.
    #[error("subtitle track is not available for this video")]
    UnknownTrack,
    /// Сервер просит подождать (HTTP 429). Типично для автоперевода YouTube.
    #[error("subtitle server is rate limiting requests")]
    RateLimited {
        /// Подсказка сервера, сколько ждать, если он её дал.
        retry_after: Option<Duration>,
    },
    /// Ни в одном из форматов дорожки на сервере нет.
    #[error("subtitle file was not found")]
    NotFound,
    /// Ссылка протухла, и повторное извлечение не помогло.
    #[error("subtitle link expired")]
    LinkExpired,
    /// Не удалось заново получить ссылки у yt-dlp.
    #[error("subtitle links could not be refreshed")]
    RefreshFailed,
    /// Нет связи с сервером.
    #[error("subtitle server is unreachable")]
    NetworkUnavailable,
    /// Сервер не ответил вовремя.
    #[error("subtitle server timed out")]
    Timeout,
    /// Сервер ответил неожиданным HTTP-статусом (не 403/404/410/429).
    #[error("subtitle server answered with HTTP {status}")]
    UnexpectedStatus {
        /// Числовой статус ответа.
        status: u16,
    },
    /// Файл больше разрешённого лимита.
    #[error("subtitle file exceeds the {limit_bytes} byte limit")]
    TooLarge {
        /// Лимит, который был превышен.
        limit_bytes: usize,
    },
    /// Ответ получен, но это не субтитры нужного формата.
    #[error("subtitle response is not a valid subtitle file")]
    InvalidResponse,
    /// Загрузку отменил вызывающий.
    #[error("subtitle download was cancelled")]
    Cancelled,
}
