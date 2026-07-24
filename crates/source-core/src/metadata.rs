use crate::{CancellationToken, SourceResult};

/// Стабильный opaque fingerprint source-а для source/cache ownership.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceFingerprint(String);

impl SourceFingerprint {
    /// Создаёт fingerprint из уже нормализованной строки.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Возвращает строковое представление fingerprint-а.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// HTTP-style validators, если source может подтвердить стабильность bytes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SourceValidators {
    /// Entity tag из HTTP response.
    pub etag: Option<String>,

    /// Last-Modified из HTTP response.
    pub last_modified: Option<String>,
}

impl std::fmt::Debug for SourceValidators {
    /// Не раскрывает server-provided validator values.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceValidators")
            .field("has_etag", &self.etag.is_some())
            .field("has_last_modified", &self.last_modified.is_some())
            .finish()
    }
}

/// Причина, по которой source нельзя безопасно seek-ать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotSeekableReason {
    /// HTTP server проигнорировал Range или вернул несовместимый status.
    HttpRangeStatus {
        /// Status code, если response был получен.
        status: u16,
    },

    /// Source пока не может доказать seekability.
    Unknown,
}

impl std::fmt::Display for NotSeekableReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HttpRangeStatus { status } => {
                write!(formatter, "HTTP Range probe вернул status {status}")
            }
            Self::Unknown => formatter.write_str("seekability unknown"),
        }
    }
}

/// Seekability source-а после local metadata или HTTP Range probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seekability {
    /// Source поддерживает byte seek.
    Seekable,

    /// Source можно открыть как объект, но seek использовать нельзя.
    NotSeekable {
        /// Нормализованная причина для player/UI diagnostics.
        reason: NotSeekableReason,
    },
}

impl Seekability {
    /// Проверяет, можно ли использовать random byte access.
    #[must_use]
    pub const fn is_seekable(&self) -> bool {
        matches!(self, Self::Seekable)
    }
}

/// Единый contract для local/http byte source-ов.
pub trait ByteSource: Send {
    /// Читает bytes из текущей позиции и сдвигает source cursor.
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize>;

    /// Переставляет source cursor на абсолютный byte offset.
    fn seek(&mut self, offset: u64) -> SourceResult<()>;

    /// Возвращает текущий byte cursor.
    fn position(&self) -> u64;

    /// Возвращает seekability, полученную из metadata/probe.
    fn seekability(&self) -> Seekability;

    /// Возвращает validators source-а, если они известны.
    fn validators(&self) -> SourceValidators;

    /// Возвращает длину source-а в bytes, если она известна.
    fn content_length(&self) -> Option<u64>;

    /// Возвращает fingerprint source-а для source/cache ownership.
    fn fingerprint(&self) -> SourceFingerprint;
}

/// Forward-only cancellation-aware byte stream для non-seekable network sources.
///
/// В отличие от `std::io::Read`, contract принимает cooperative cancellation на
/// каждом чтении. Concrete HTTP provider обязан прерывать blocking I/O либо
/// bounded worker wait при отмене; demux/player layers не должны изобретать
/// второй transport-specific cancellation path.
pub trait StreamingByteSource: Send {
    /// Читает следующую порцию bytes либо возвращает typed source error.
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize>;
}
