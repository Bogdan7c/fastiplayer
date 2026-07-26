//! Нейтральные bounded-модели F4M без URL fetching и quality policy.

use std::num::NonZeroUsize;
use std::time::Duration;

/// Поддерживаемые F4M namespace URI.
pub(crate) const F4M_NAMESPACES: &[&str] =
    &["http://ns.adobe.com/f4m/1.0", "http://ns.adobe.com/f4m/2.0"];

/// Явно ограничивает размер F4M domain model-а после S04X parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct F4mManifestLimits {
    /// Максимальное число media rows одного документа.
    maximum_media_entries: NonZeroUsize,
    /// Максимальное число bootstrapInfo rows одного документа.
    maximum_bootstrap_entries: NonZeroUsize,
    /// Максимальный размер decoded base64 bootstrap-а.
    maximum_bootstrap_bytes: NonZeroUsize,
    /// Максимальная длина одного domain string-а.
    maximum_string_bytes: NonZeroUsize,
}

impl F4mManifestLimits {
    /// Создаёт policy только из явных caller-owned bounds.
    #[must_use]
    pub const fn new(
        maximum_media_entries: NonZeroUsize,
        maximum_bootstrap_entries: NonZeroUsize,
        maximum_bootstrap_bytes: NonZeroUsize,
        maximum_string_bytes: NonZeroUsize,
    ) -> Self {
        Self {
            maximum_media_entries,
            maximum_bootstrap_entries,
            maximum_bootstrap_bytes,
            maximum_string_bytes,
        }
    }

    /// Возвращает media-row limit.
    #[must_use]
    pub const fn maximum_media_entries(self) -> NonZeroUsize {
        self.maximum_media_entries
    }

    /// Возвращает bootstrap-row limit.
    #[must_use]
    pub const fn maximum_bootstrap_entries(self) -> NonZeroUsize {
        self.maximum_bootstrap_entries
    }

    /// Возвращает decoded bootstrap byte limit.
    #[must_use]
    pub const fn maximum_bootstrap_bytes(self) -> NonZeroUsize {
        self.maximum_bootstrap_bytes
    }

    /// Возвращает длину одного string field.
    #[must_use]
    pub const fn maximum_string_bytes(self) -> NonZeroUsize {
        self.maximum_string_bytes
    }
}

/// VOD/live intent, заявленный F4M manifest-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F4mStreamType {
    /// Конечная запись, разрешённая S38 base/VOD profile.
    Vod,
    /// Динамическая presentation, пока не входящая в S38 base card.
    Live,
    /// Поле отсутствовало: runtime обязан подтвердить VOD другим evidence.
    Unspecified,
}

/// Источник bootstrapInfo: inline base64 или отдельный URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum F4mBootstrapSource {
    /// Уже bounded decoded bootstrap bytes.
    Inline(Box<[u8]>),
    /// Relative/absolute locator, который разрешает transport owner.
    Url(String),
}

/// Один manifest-level bootstrapInfo row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct F4mBootstrapInfo {
    /// Optional id, используемый media.bootstrapInfoId.
    id: Option<String>,
    /// Bootstrap bytes или deferred URL.
    source: F4mBootstrapSource,
}

impl F4mBootstrapInfo {
    /// Возвращает optional bootstrap id.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Возвращает source без раскрытия network state.
    #[must_use]
    pub const fn source(&self) -> &F4mBootstrapSource {
        &self.source
    }
}

/// Один F4M media row; `href` означает следующий уровень hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct F4mMediaEntry {
    /// Concrete quality media locator, если row уже stream-level.
    url: Option<String>,
    /// Child F4M locator, если row является set-level hierarchy edge.
    href: Option<String>,
    /// Service bitrate для deterministic quality ordering.
    bitrate: Option<u64>,
    /// Advertised video width.
    width: Option<u32>,
    /// Advertised video height.
    height: Option<u32>,
    /// Manifest bootstrap id, если задан.
    bootstrap_info_id: Option<String>,
}

/// Безопасная причина, по которой одна `<media>` row не вошла в parsed inventory.
///
/// Значение намеренно не содержит attribute text: URL и query остаются внутри
/// provider-а, а caller получает достаточно evidence для bounded diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F4mMediaEntryRejection {
    /// `url`/`href` отсутствует, пуст или задан одновременно.
    InvalidLocatorShape,
    /// Bitrate не является допустимым `u64`.
    InvalidBitrate,
    /// Width не является допустимым `u32`.
    InvalidWidth,
    /// Height не является допустимым `u32`.
    InvalidHeight,
    /// Один bounded string attribute превысил caller-owned limit.
    StringTooLong,
}

impl F4mMediaEntry {
    /// Возвращает concrete media URL.
    #[must_use]
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// Возвращает hierarchy child URL.
    #[must_use]
    pub fn href(&self) -> Option<&str> {
        self.href.as_deref()
    }

    /// Возвращает optional bitrate.
    #[must_use]
    pub const fn bitrate(&self) -> Option<u64> {
        self.bitrate
    }

    /// Возвращает optional width.
    #[must_use]
    pub const fn width(&self) -> Option<u32> {
        self.width
    }

    /// Возвращает optional height.
    #[must_use]
    pub const fn height(&self) -> Option<u32> {
        self.height
    }

    /// Возвращает bootstrap id выбранной media row.
    #[must_use]
    pub fn bootstrap_info_id(&self) -> Option<&str> {
        self.bootstrap_info_id.as_deref()
    }
}

/// Parsed F4M document с сохранённым hierarchy и manifest-local metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct F4mManifest {
    /// Advertised F4M stream type.
    stream_type: F4mStreamType,
    /// Optional manifest duration.
    duration: Option<Duration>,
    /// Optional baseURL text.
    base_url: Option<String>,
    /// Direct media rows или hierarchy edges.
    media: Box<[F4mMediaEntry]>,
    /// Malformed sibling rows, изолированные без раскрытия attribute values.
    rejected_media: Box<[F4mMediaEntryRejection]>,
    /// Manifest-level bootstrap definitions.
    bootstrap_info: Box<[F4mBootstrapInfo]>,
}

impl F4mManifest {
    /// Возвращает stream/live intent.
    #[must_use]
    pub const fn stream_type(&self) -> F4mStreamType {
        self.stream_type
    }

    /// Возвращает optional duration.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Возвращает manifest base URL.
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// Возвращает media rows.
    #[must_use]
    pub fn media(&self) -> &[F4mMediaEntry] {
        &self.media
    }

    /// Возвращает безопасные причины изоляции malformed sibling rows.
    #[must_use]
    pub fn rejected_media(&self) -> &[F4mMediaEntryRejection] {
        &self.rejected_media
    }

    /// Возвращает bootstrap definitions.
    #[must_use]
    pub fn bootstrap_info(&self) -> &[F4mBootstrapInfo] {
        &self.bootstrap_info
    }

    /// Создаёт внутреннюю model после parser-side validation.
    pub(crate) fn new(
        stream_type: F4mStreamType,
        duration: Option<Duration>,
        base_url: Option<String>,
        media: Vec<F4mMediaEntry>,
        rejected_media: Vec<F4mMediaEntryRejection>,
        bootstrap_info: Vec<F4mBootstrapInfo>,
    ) -> Self {
        Self {
            stream_type,
            duration,
            base_url,
            media: media.into_boxed_slice(),
            rejected_media: rejected_media.into_boxed_slice(),
            bootstrap_info: bootstrap_info.into_boxed_slice(),
        }
    }
}

/// Вспомогательная constructor boundary parser-а для media row.
pub(crate) fn media_entry(
    url: Option<String>,
    href: Option<String>,
    bitrate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    bootstrap_info_id: Option<String>,
) -> F4mMediaEntry {
    F4mMediaEntry {
        url,
        href,
        bitrate,
        width,
        height,
        bootstrap_info_id,
    }
}

/// Внутренний constructor bootstrap row.
pub(crate) fn bootstrap_info(id: Option<String>, source: F4mBootstrapSource) -> F4mBootstrapInfo {
    F4mBootstrapInfo { id, source }
}
