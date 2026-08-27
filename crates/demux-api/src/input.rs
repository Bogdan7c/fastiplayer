use std::fmt;
use std::io::Read;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use bytes::Bytes;
use media_core::{DemuxRetryHint, PacketPresentationWindow};
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
    /// Pull-based ordered resources с отдельными chunk и resource EOF boundaries.
    OrderedResourceStream,
}

impl DemuxInputCapability {
    /// Возвращает внутренний bit только для compact capability set-а.
    const fn bit(self) -> u8 {
        match self {
            Self::SeekableBytes => 1 << 0,
            Self::StreamingBytes => 1 << 1,
            Self::OrderedSegments => 1 << 2,
            Self::OrderedResourceStream => 1 << 3,
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

/// Явный lifecycle-маркер на границе transport -> demux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderedSegmentDiscontinuity {
    /// Segment продолжает текущую decoder/timestamp generation.
    #[default]
    Continuous,
    /// Перед segment-ом начинается новая decoder/timestamp generation.
    StartsNewTimeline,
}

/// Один immutable segment с явной ролью и порядком.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedSegment {
    /// Monotonic transport/session sequence.
    pub sequence: OrderedSegmentSequence,
    /// Роль bytes в segmented container stream.
    pub kind: OrderedSegmentKind,
    /// Явно сообщает demuxer-у о смене timeline/config generation.
    pub discontinuity: OrderedSegmentDiscontinuity,
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

/// Provenance одного ресурса в pull-based ordered byte stream.
///
/// Metadata отделена от body chunks, поэтому временная граница transport chunk-а
/// не может случайно стать container resource boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderedResourceMetadata {
    /// Monotonic transport/session sequence.
    pub sequence: OrderedSegmentSequence,
    /// Роль ресурса в container byte sequence.
    pub kind: OrderedSegmentKind,
    /// Явный lifecycle marker перед первым byte ресурса.
    pub discontinuity: OrderedSegmentDiscontinuity,
}

/// Результат одного bounded pull из ordered resource stream-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderedResourceReadOutcome {
    /// Начинается новый resource; следующий непустой `Data` принадлежит ему.
    Begin(OrderedResourceMetadata),
    /// Непустой immutable body chunk размером не больше caller-provided bound-а.
    Data(Bytes),
    /// Текущий resource полностью и успешно закончен.
    EndResource,
    /// После последнего `EndResource` новых ресурсов больше не будет.
    EndOfInput,
}

/// Typed failure pull-based ordered resource source-а.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrderedResourceReadError {
    /// Caller отменил чтение через shared token; это не resource EOF.
    #[error("чтение ordered resource stream отменено")]
    Cancelled,
    /// Committed owner прервал physical body read ради transactional replacement.
    ///
    /// Это не cancellation, EOF или malformed input: parser обязан unwind-нуть текущий
    /// read, а source owner решает commit replacement либо byte-zero rollback reopen.
    #[error("активное чтение ordered resource прервано с возможностью restart")]
    RestartableReadInterrupted,
    /// Source завершил pull operational error-ом; это не resource EOF.
    #[error("ошибка чтения ordered resource stream: {reason}")]
    Failed {
        /// Secret-safe bounded transport reason.
        reason: String,
    },
}

/// Neutral marker сохраняет typed interruption через container-specific error chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("restartable ordered resource read interrupted")]
pub struct OrderedResourceRestartableReadInterrupted;

/// Neutral pull-based ordered resource stream.
///
/// Source обязан соблюдать lifecycle `Begin -> Data* -> EndResource` и затем
/// либо начинать следующий resource, либо вернуть `EndOfInput`. Пустой `Data`
/// запрещён, а непустой chunk не может превышать `maximum_chunk_bytes`.
pub trait OrderedResourceStreamSource: Send {
    /// Возвращает ровно один lifecycle/body outcome с явным backpressure bound-ом.
    fn next_event(
        &mut self,
        maximum_chunk_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<OrderedResourceReadOutcome, OrderedResourceReadError>;
}

/// Отдельный ordered segment contract с обязательным presentation-window intent.
///
/// Существующий [`OrderedSegment`] намеренно не расширяется: старые finite
/// container пути не получают скрытый новый field или новую семантику.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentationWindowOrderedSegment {
    /// Initialization bytes, которые не несут packet presentation window.
    Initialization {
        /// Monotonic transport/session sequence.
        sequence: OrderedSegmentSequence,
        /// Явный lifecycle marker.
        discontinuity: OrderedSegmentDiscontinuity,
        /// Exact initialization bytes.
        bytes: Bytes,
    },
    /// Media bytes с обязательным bounded либо explicit unbounded intent.
    Media {
        /// Monotonic transport/session sequence.
        sequence: OrderedSegmentSequence,
        /// Явный lifecycle marker.
        discontinuity: OrderedSegmentDiscontinuity,
        /// Exact media fragment bytes.
        bytes: Bytes,
        /// Authoritative presentation intent для каждого packet-а fragment-а.
        presentation_window: PacketPresentationWindow,
    },
}

impl PresentationWindowOrderedSegment {
    /// Возвращает sequence независимо от variant-а.
    #[must_use]
    pub const fn sequence(&self) -> OrderedSegmentSequence {
        match self {
            Self::Initialization { sequence, .. } | Self::Media { sequence, .. } => *sequence,
        }
    }

    /// Возвращает discontinuity независимо от variant-а.
    #[must_use]
    pub const fn discontinuity(&self) -> OrderedSegmentDiscontinuity {
        match self {
            Self::Initialization { discontinuity, .. } | Self::Media { discontinuity, .. } => {
                *discontinuity
            }
        }
    }

    /// Возвращает exact immutable bytes независимо от variant-а.
    #[must_use]
    pub const fn bytes(&self) -> &Bytes {
        match self {
            Self::Initialization { bytes, .. } | Self::Media { bytes, .. } => bytes,
        }
    }
}

/// Nonblocking результат одного чтения window-aware ordered source-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentationWindowOrderedSegmentReadOutcome {
    /// Следующий segment готов.
    Segment(PresentationWindowOrderedSegment),
    /// Source остаётся живым, но следующий segment ещё не готов.
    TemporarilyUnavailable(DemuxRetryHint),
    /// Source терминально завершён.
    EndOfStream,
}

/// Neutral window-aware ordered source; provider/manifest state остаётся снаружи.
pub trait PresentationWindowOrderedSegmentSource: Send {
    /// Читает ровно один outcome с cooperative cancellation.
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<PresentationWindowOrderedSegmentReadOutcome, OrderedSegmentReadError>;
}

/// Owned input, который registry передаёт ровно одному выбранному factory.
pub enum DemuxInput {
    /// `source-core` byte source; runtime seekability определяется самим source-ом.
    ByteSource(DemuxByteSource),
    /// Последовательный reader без byte seek.
    ByteStream(Box<dyn DemuxByteStream>),
    /// Явно разделённая ordered segment sequence.
    OrderedSegments(Box<dyn OrderedSegmentSource>),
    /// Ordered resources с pull-based bounded body chunks и отдельным resource EOF.
    OrderedResourceStream(Box<dyn OrderedResourceStreamSource>),
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

    /// Стирает concrete pull-based ordered resource provider type.
    #[must_use]
    pub fn ordered_resource_stream(source: Box<dyn OrderedResourceStreamSource>) -> Self {
        Self::OrderedResourceStream(source)
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
            Self::OrderedResourceStream(_) => DemuxInputCapability::OrderedResourceStream,
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

        let ordered_stream =
            DemuxInputCapabilities::only(DemuxInputCapability::OrderedResourceStream);
        assert!(!ordered_stream.intersects(seekable));
        assert!(
            both.with(DemuxInputCapability::OrderedResourceStream)
                .contains(DemuxInputCapability::OrderedResourceStream)
        );
    }
}
