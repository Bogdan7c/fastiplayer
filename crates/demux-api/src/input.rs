use std::fmt;
use std::io::Read;
use std::sync::Mutex;

use bytes::Bytes;
use source_core::{
    ByteSource, CancellationToken, Seekability, SourceFingerprint, SourceResult, SourceValidators,
    StreamingByteSource,
};

/// Ровно одна форма input, которую demux factory умеет открыть.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DemuxInputCapability {
    /// Reopenable/random-access byte source с честным absolute seek.
    SeekableBytes,
    /// Последовательный byte stream без обещания byte seek.
    StreamingBytes,
    /// Последовательность явно отделённых init/media segments.
    OrderedSegments,
}

impl DemuxInputCapability {
    /// Возвращает внутренний bit только для compact capability set-а.
    const fn bit(self) -> u8 {
        match self {
            Self::SeekableBytes => 1 << 0,
            Self::StreamingBytes => 1 << 1,
            Self::OrderedSegments => 1 << 2,
        }
    }
}

/// Compact set input capabilities без зависимости от конкретного source type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DemuxInputCapabilities(u8);

impl DemuxInputCapabilities {
    /// Пустой set полезен только при пошаговой сборке test registration.
    pub const NONE: Self = Self(0);

    /// Создаёт set из одного явно названного capability.
    #[must_use]
    pub const fn only(capability: DemuxInputCapability) -> Self {
        Self(capability.bit())
    }

    /// Добавляет capability и возвращает новый immutable value.
    #[must_use]
    pub const fn with(self, capability: DemuxInputCapability) -> Self {
        Self(self.0 | capability.bit())
    }

    /// Объединяет два immutable capability sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Возвращает только input shapes, объявленные обеими сторонами boundary.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Проверяет наличие хотя бы одной общей input shape.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Проверяет поддержку exact input shape.
    #[must_use]
    pub const fn contains(self, capability: DemuxInputCapability) -> bool {
        self.0 & capability.bit() != 0
    }

    /// Проверяет, объявил ли factory хотя бы одну форму input.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Owned type-erased `ByteSource`, который остаётся совместим с generic adapters.
pub struct DemuxByteSource {
    /// Конкретный local/http/prefetch source остаётся скрыт за neutral trait.
    inner: Box<dyn ByteSource>,
}

impl DemuxByteSource {
    /// Стирает concrete source type без изменения его cursor/lifecycle.
    #[must_use]
    pub fn new(source: Box<dyn ByteSource>) -> Self {
        Self { inner: source }
    }

    /// Возвращает mutable inner boundary для bounded sniff wrapper-а.
    pub(crate) fn inner_mut(&mut self) -> &mut dyn ByteSource {
        self.inner.as_mut()
    }
}

impl fmt::Debug for DemuxByteSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DemuxByteSource")
            .field("seekability", &self.seekability())
            .field("content_length", &self.content_length())
            .finish_non_exhaustive()
    }
}

impl ByteSource for DemuxByteSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        self.inner.read(output, cancellation)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.inner.seek(offset)
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn seekability(&self) -> Seekability {
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

/// Object-safe streaming reader alias для registry/factory handoff.
pub trait DemuxByteStream: Read + Send + Sync {}

impl<Reader> DemuxByteStream for Reader where Reader: Read + Send + Sync {}

/// Адаптер cancellation-aware streaming source-а к blocking container reader-у.
///
/// Blocking допустим только на progressive demux worker-е. Player owner никогда
/// не вызывает этот reader напрямую и получает neutral readiness events.
struct StreamingSourceByteReader {
    /// Mutex даёт требуемый Symphonia `Sync`, сохраняя единственного mutable reader owner-а.
    source: Mutex<Box<dyn StreamingByteSource>>,
    /// Shared token прерывает network read при drop/supersede.
    cancellation: CancellationToken,
}

impl StreamingSourceByteReader {
    /// Связывает source с exact lifecycle cancellation token-ом.
    fn new(source: Box<dyn StreamingByteSource>, cancellation: CancellationToken) -> Self {
        Self {
            source: Mutex::new(source),
            cancellation,
        }
    }
}

impl Read for StreamingSourceByteReader {
    /// Делегирует blocking read только cancellation-aware source primitive-у.
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let mut source = self
            .source
            .lock()
            .map_err(|_| std::io::Error::other("streaming source mutex poisoned"))?;
        source
            .read(output, &self.cancellation)
            .map_err(std::io::Error::other)
    }
}

/// Monotonic sequence identity одного ordered segment-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderedSegmentSequence(u64);

impl OrderedSegmentSequence {
    /// Создаёт sequence из transport-owned monotonic value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает wire/session sequence без интерпретации container layer-ом.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Роль segment-а в container byte sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderedSegmentKind {
    /// Initialization/header segment, который должен предшествовать media segment-ам.
    Initialization,
    /// Обычный media segment.
    Media,
}

/// Один immutable segment с явной ролью и порядком.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedSegment {
    /// Monotonic transport/session sequence.
    pub sequence: OrderedSegmentSequence,
    /// Роль bytes в segmented container stream.
    pub kind: OrderedSegmentKind,
    /// Exact segment bytes без container parsing на transport boundary.
    pub bytes: Bytes,
}

/// Typed read failure ordered segment source-а.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrderedSegmentReadError {
    /// Caller отменил чтение через shared token.
    #[error("чтение ordered segment отменено")]
    Cancelled,
    /// Source завершился operational error-ом с bounded safe причиной.
    #[error("ошибка чтения ordered segment: {reason}")]
    Failed {
        /// Secret-safe bounded transport reason.
        reason: String,
    },
}

/// Neutral ordered segment input; manifest/network state остаётся у provider-а.
pub trait OrderedSegmentSource: Send {
    /// Возвращает следующий segment или terminal EOF.
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError>;
}

/// Owned input, который registry передаёт ровно одному выбранному factory.
pub enum DemuxInput {
    /// `source-core` byte source; runtime seekability определяется самим source-ом.
    ByteSource(DemuxByteSource),
    /// Последовательный reader без byte seek.
    ByteStream(Box<dyn DemuxByteStream>),
    /// Явно разделённая ordered segment sequence.
    OrderedSegments(Box<dyn OrderedSegmentSource>),
}

impl DemuxInput {
    /// Стирает concrete byte source type.
    #[must_use]
    pub fn byte_source(source: Box<dyn ByteSource>) -> Self {
        Self::ByteSource(DemuxByteSource::new(source))
    }

    /// Стирает concrete streaming reader type.
    #[must_use]
    pub fn byte_stream(reader: Box<dyn DemuxByteStream>) -> Self {
        Self::ByteStream(reader)
    }

    /// Адаптирует S21T streaming source для concrete blocking factory open/read.
    #[must_use]
    pub fn streaming_source(
        source: Box<dyn StreamingByteSource>,
        cancellation: CancellationToken,
    ) -> Self {
        Self::byte_stream(Box::new(StreamingSourceByteReader::new(
            source,
            cancellation,
        )))
    }

    /// Стирает concrete ordered segment provider type.
    #[must_use]
    pub fn ordered_segments(source: Box<dyn OrderedSegmentSource>) -> Self {
        Self::OrderedSegments(source)
    }

    /// Вычисляет exact capability из runtime input shape/seekability.
    #[must_use]
    pub fn capability(&self) -> DemuxInputCapability {
        match self {
            Self::ByteSource(source) if source.seekability().is_seekable() => {
                DemuxInputCapability::SeekableBytes
            }
            Self::ByteSource(_) | Self::ByteStream(_) => DemuxInputCapability::StreamingBytes,
            Self::OrderedSegments(_) => DemuxInputCapability::OrderedSegments,
        }
    }
}

impl fmt::Debug for DemuxInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DemuxInput")
            .field("capability", &self.capability())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{DemuxInputCapabilities, DemuxInputCapability};

    /// Set operations сохраняют точное пересечение transport/demux shapes.
    #[test]
    fn capability_sets_union_and_intersect_without_guessing() {
        let seekable = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes);
        let streaming = DemuxInputCapabilities::only(DemuxInputCapability::StreamingBytes);
        let both = seekable.union(streaming);

        assert!(both.contains(DemuxInputCapability::SeekableBytes));
        assert!(both.contains(DemuxInputCapability::StreamingBytes));
        assert_eq!(both.intersection(seekable), seekable);
        assert!(both.intersects(streaming));
        assert!(!seekable.intersects(streaming));
    }
}
