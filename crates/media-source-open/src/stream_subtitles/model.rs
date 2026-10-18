//! Нейтральная модель дорожек субтитров потокового источника.
//!
//! Модель не знает ни про HLS, ни про DASH: её заполняет конкретный транспорт, а читают
//! лента субтитров и приложение. Адресов и секретов здесь нет, `Debug` безопасен.

use std::fmt;
use std::sync::Arc;

use super::transport::StreamSubtitleTransport;

/// Идентификатор дорожки внутри одного [`StreamSubtitleSource`].
///
/// Не является постоянной идентичностью: после переоткрытия источника выдаётся заново.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamSubtitleTrackId(u32);

impl StreamSubtitleTrackId {
    /// Создаёт идентификатор из порядкового номера дорожки у транспорта.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Порядковый номер дорожки.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Безопасное описание дорожки: только метаданные для меню и автовыбора.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSubtitleTrackInfo {
    /// Идентификатор для запуска ленты.
    pub id: StreamSubtitleTrackId,
    /// Тег языка из манифеста (как есть), если задан.
    pub language: Option<Box<str>>,
    /// Название дорожки для меню.
    pub name: Box<str>,
    /// Дорожка «только форсированные» (иностранная речь, надписи).
    pub forced: bool,
    /// Манифест помечает дорожку выбранной по умолчанию.
    pub default: bool,
    /// Манифест разрешает автоматический выбор дорожки.
    pub autoselect: bool,
}

/// Источник субтитров потока: список дорожек и транспорт, который их отдаёт.
///
/// Дёшево клонируется (общий `Arc`). `Debug` печатает только метаданные дорожек.
#[derive(Clone)]
pub struct StreamSubtitleSource {
    transport: Arc<dyn StreamSubtitleTransport>,
    tracks: Arc<[StreamSubtitleTrackInfo]>,
}

impl StreamSubtitleSource {
    /// Собирает источник из транспорта; `None`, если у потока нет ни одной дорожки.
    #[must_use]
    pub fn new(transport: Arc<dyn StreamSubtitleTransport>) -> Option<Self> {
        let tracks: Arc<[StreamSubtitleTrackInfo]> = transport.tracks().into();
        if tracks.is_empty() {
            return None;
        }
        Some(Self { transport, tracks })
    }

    /// Дорожки в порядке манифеста.
    #[must_use]
    pub fn tracks(&self) -> &[StreamSubtitleTrackInfo] {
        &self.tracks
    }

    /// Описание дорожки по идентификатору.
    #[must_use]
    pub fn track(&self, id: StreamSubtitleTrackId) -> Option<&StreamSubtitleTrackInfo> {
        self.tracks.iter().find(|track| track.id == id)
    }

    /// Транспорт для ленты (граница внутри crate-а: приложение им не пользуется).
    pub(crate) fn transport(&self) -> &Arc<dyn StreamSubtitleTransport> {
        &self.transport
    }
}

impl PartialEq for StreamSubtitleSource {
    /// Два источника равны, если это один и тот же открытый транспорт.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.transport, &other.transport)
    }
}

impl Eq for StreamSubtitleSource {}

impl fmt::Debug for StreamSubtitleSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamSubtitleSource")
            .field("tracks", &self.tracks)
            .finish_non_exhaustive()
    }
}
