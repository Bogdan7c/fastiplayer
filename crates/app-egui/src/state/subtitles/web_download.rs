//! Граница загрузки web-субтитров: узкий контракт, от которого зависит владелец субтитров.
//!
//! Владелец (`SubtitleOwner`) не знает ни про yt-dlp, ни про подписанные ссылки, ни про HTTP:
//! он видит только [`WebSubtitleDownloader`] — «скачай дорожку по её стабильному
//! идентификатору». Настоящая реализация — тонкий адаптер над `WebSubtitleFetchHandle` из
//! `media-source-open`; тесты подставляют фальшивую.
//!
//! Контракт сознательно не содержит URL: ссылки остаются внутри `media-source-open`, поэтому
//! они не могут попасть ни в `Debug`, ни в логи приложения.

use std::fmt;
use std::num::NonZeroUsize;

use media_source_open::web_subtitle_fetch::{
    WebSubtitleFetchError, WebSubtitleFetchHandle, WebSubtitleFetchLimits, WebSubtitleFormat,
    WebSubtitleTrackId,
};
use source_core::CancellationToken;

/// Скачанные байты субтитров и формат, в котором они пришли.
///
/// Свой тип (а не `FetchedWebSubtitle`), чтобы контракт можно было сыграть в тестах без
/// доступа к внутренностям `media-source-open`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DownloadedWebSubtitle {
    /// Тело ответа как есть; разбирает `subtitle-formats`.
    pub(crate) bytes: Vec<u8>,
    /// Формат тела.
    pub(crate) format: WebSubtitleFormat,
}

impl fmt::Debug for DownloadedWebSubtitle {
    /// Только размер и формат: текст реплик в логи не идёт.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DownloadedWebSubtitle")
            .field("byte_count", &self.bytes.len())
            .field("format", &self.format)
            .finish()
    }
}

/// Скачивает web-субтитры. Вызов блокирующий: только с фонового потока задачи загрузки.
///
/// Требование к реализации: ошибка `Cancelled` — единственный способ сообщить об отмене
/// через `cancellation`; никаких фоновых повторов реализация не делает (повтор — только
/// новым вызовом по клику пользователя).
pub(crate) trait WebSubtitleDownloader: fmt::Debug + Send + Sync {
    /// Скачивает дорожку `track`, не читая больше `max_bytes` байт.
    fn download(
        &self,
        track: &WebSubtitleTrackId,
        max_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<DownloadedWebSubtitle, WebSubtitleFetchError>;
}

/// Настоящий загрузчик: дескриптор `media-source-open`.
impl WebSubtitleDownloader for WebSubtitleFetchHandle {
    fn download(
        &self,
        track: &WebSubtitleTrackId,
        max_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<DownloadedWebSubtitle, WebSubtitleFetchError> {
        let limits = WebSubtitleFetchLimits::with_max_bytes(max_bytes);
        let fetched = self.fetch(track, limits, cancellation)?;
        let format = fetched.format();
        Ok(DownloadedWebSubtitle {
            bytes: fetched.into_bytes(),
            format,
        })
    }
}
