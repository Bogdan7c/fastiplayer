//! Bounded sniff чтение и точное восстановление input для winning factory.

use std::collections::VecDeque;
use std::io::{self, Cursor, Read};
use std::time::Instant;

use source_core::{
    ByteSource, CancellationToken, Seekability, SourceError, SourceFingerprint, SourceResult,
    SourceValidators,
};

use crate::{
    DemuxByteSource, DemuxByteStream, DemuxInput, DemuxProbeRejection, DemuxSniffBudget,
    OrderedSegment, OrderedSegmentReadError, OrderedSegmentSource,
};

use super::{DemuxOpenError, ensure_active};

/// Снимает bounded prefix и возвращает input с исходной последовательностью bytes.
pub(super) fn sniff_and_restore_input(
    input: DemuxInput,
    budget: DemuxSniffBudget,
    cancellation: &CancellationToken,
) -> Result<(DemuxInput, Vec<u8>), DemuxOpenError> {
    let started_at = Instant::now();
    match input {
        DemuxInput::ByteSource(mut source) => {
            let original_position = source.position();
            let sniffed_bytes =
                read_bounded_byte_source(source.inner_mut(), budget, cancellation, started_at)?;
            if source.seekability().is_seekable() {
                source
                    .seek(original_position)
                    .map_err(source_error_to_probe)?;
                Ok((DemuxInput::ByteSource(source), sniffed_bytes))
            } else {
                let replay_source =
                    ReplayByteSource::new(source, original_position, &sniffed_bytes);
                Ok((
                    DemuxInput::byte_source(Box::new(replay_source)),
                    sniffed_bytes,
                ))
            }
        }
        DemuxInput::ByteStream(mut reader) => {
            let sniffed_bytes =
                read_bounded_reader(reader.as_mut(), budget, cancellation, started_at)?;
            let replay_reader = ReplayByteStream::new(reader, &sniffed_bytes);
            Ok((
                DemuxInput::byte_stream(Box::new(replay_reader)),
                sniffed_bytes,
            ))
        }
        DemuxInput::OrderedSegments(mut source) => {
            let (sniffed_bytes, replay_segments) =
                read_bounded_segments(source.as_mut(), budget, cancellation, started_at)?;
            let replay_source = ReplayOrderedSegmentSource::new(source, replay_segments);
            Ok((
                DemuxInput::ordered_segments(Box::new(replay_source)),
                sniffed_bytes,
            ))
        }
    }
}

/// Читает prefix из `ByteSource` с cooperative checks между blocking calls.
fn read_bounded_byte_source(
    source: &mut dyn ByteSource,
    budget: DemuxSniffBudget,
    cancellation: &CancellationToken,
    started_at: Instant,
) -> Result<Vec<u8>, DemuxOpenError> {
    let mut sniffed_bytes = vec![0_u8; budget.max_bytes()];
    let mut filled = 0;
    while filled < sniffed_bytes.len() {
        check_sniff_progress(cancellation, budget, started_at)?;
        let bytes_read = source
            .read(&mut sniffed_bytes[filled..], cancellation)
            .map_err(source_error_to_probe)?;
        if bytes_read == 0 {
            break;
        }
        filled += bytes_read;
    }
    sniffed_bytes.truncate(filled);
    Ok(sniffed_bytes)
}

/// Читает prefix из sequential reader-а с теми же explicit bounds.
fn read_bounded_reader(
    reader: &mut dyn DemuxByteStream,
    budget: DemuxSniffBudget,
    cancellation: &CancellationToken,
    started_at: Instant,
) -> Result<Vec<u8>, DemuxOpenError> {
    let mut sniffed_bytes = vec![0_u8; budget.max_bytes()];
    let mut filled = 0;
    while filled < sniffed_bytes.len() {
        check_sniff_progress(cancellation, budget, started_at)?;
        let bytes_read = reader
            .read(&mut sniffed_bytes[filled..])
            .map_err(io_error_to_probe)?;
        if bytes_read == 0 {
            break;
        }
        filled += bytes_read;
    }
    sniffed_bytes.truncate(filled);
    Ok(sniffed_bytes)
}

/// Читает не больше segment и byte bounds, сохраняя exact replay boundaries.
fn read_bounded_segments(
    source: &mut dyn OrderedSegmentSource,
    budget: DemuxSniffBudget,
    cancellation: &CancellationToken,
    started_at: Instant,
) -> Result<(Vec<u8>, VecDeque<OrderedSegment>), DemuxOpenError> {
    let mut sniffed_bytes = Vec::with_capacity(budget.max_bytes());
    let mut replay_segments = VecDeque::new();
    for _ in 0..budget.max_segments() {
        if sniffed_bytes.len() >= budget.max_bytes() {
            break;
        }
        check_sniff_progress(cancellation, budget, started_at)?;
        let Some(segment) = source
            .next_segment(cancellation)
            .map_err(segment_error_to_probe)?
        else {
            break;
        };
        if segment.bytes.len() > budget.max_bytes() {
            return Err(DemuxOpenError::ProbeRejected(
                DemuxProbeRejection::SegmentExceedsByteBudget {
                    segment_bytes: segment.bytes.len(),
                    max_bytes: budget.max_bytes(),
                },
            ));
        }
        let remaining_bytes = budget.max_bytes() - sniffed_bytes.len();
        let sample_len = segment.bytes.len().min(remaining_bytes);
        sniffed_bytes.extend_from_slice(&segment.bytes[..sample_len]);
        replay_segments.push_back(segment);
    }
    Ok((sniffed_bytes, replay_segments))
}

/// Проверяет cancellation/deadline в каждой доступной cooperative точке.
fn check_sniff_progress(
    cancellation: &CancellationToken,
    budget: DemuxSniffBudget,
    started_at: Instant,
) -> Result<(), DemuxOpenError> {
    ensure_active(cancellation)?;
    if started_at.elapsed() > budget.max_duration() {
        return Err(DemuxOpenError::ProbeRejected(
            DemuxProbeRejection::DeadlineExceeded {
                max_duration: budget.max_duration(),
            },
        ));
    }
    Ok(())
}

/// Сохраняет typed source cancellation, остальные safe errors переводит в probe layer.
fn source_error_to_probe(error: SourceError) -> DemuxOpenError {
    let rejection = match error {
        SourceError::Cancelled => DemuxProbeRejection::Cancelled,
        other_error => DemuxProbeRejection::InputFailure {
            reason: other_error.to_string(),
        },
    };
    DemuxOpenError::ProbeRejected(rejection)
}

/// Классифицирует обычный sequential I/O без знания concrete transport-а.
fn io_error_to_probe(error: io::Error) -> DemuxOpenError {
    let rejection = if error.kind() == io::ErrorKind::Interrupted {
        DemuxProbeRejection::Cancelled
    } else {
        DemuxProbeRejection::InputFailure {
            reason: error.to_string(),
        }
    };
    DemuxOpenError::ProbeRejected(rejection)
}

/// Классифицирует ordered source failure без утечки provider state.
fn segment_error_to_probe(error: OrderedSegmentReadError) -> DemuxOpenError {
    let rejection = match error {
        OrderedSegmentReadError::Cancelled => DemuxProbeRejection::Cancelled,
        OrderedSegmentReadError::Failed { reason } => DemuxProbeRejection::InputFailure { reason },
    };
    DemuxOpenError::ProbeRejected(rejection)
}

/// Non-seekable source wrapper возвращает уже sniffed bytes ровно один раз.
struct ReplayByteSource {
    /// Prefix cursor хранит только bounded sniff allocation.
    prefix: Cursor<Vec<u8>>,
    /// Original source уже стоит сразу после prefix.
    inner: DemuxByteSource,
    /// Logical position, видимая demux adapter-у во время replay.
    logical_position: u64,
}

impl ReplayByteSource {
    /// Создаёт wrapper без повторного чтения source-а.
    fn new(inner: DemuxByteSource, original_position: u64, prefix: &[u8]) -> Self {
        Self {
            prefix: Cursor::new(prefix.to_vec()),
            inner,
            logical_position: original_position,
        }
    }
}

impl ByteSource for ReplayByteSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let prefix_bytes = self
            .prefix
            .read(output)
            .map_err(|source| SourceError::LocalIo {
                context: "replay demux sniff prefix",
                source,
            })?;
        let bytes_read = if prefix_bytes > 0 {
            prefix_bytes
        } else {
            self.inner.read(output, cancellation)?
        };
        self.logical_position = self.logical_position.saturating_add(bytes_read as u64);
        Ok(bytes_read)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.inner.seek(offset)
    }

    fn position(&self) -> u64 {
        self.logical_position
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

/// Sequential reader wrapper replay-ит prefix перед original reader-ом.
struct ReplayByteStream {
    /// Bounded prefix cursor.
    prefix: Cursor<Vec<u8>>,
    /// Original reader после sniff cursor.
    inner: Box<dyn DemuxByteStream>,
}

impl ReplayByteStream {
    /// Сохраняет exact prefix для будущего concrete factory.
    fn new(inner: Box<dyn DemuxByteStream>, prefix: &[u8]) -> Self {
        Self {
            prefix: Cursor::new(prefix.to_vec()),
            inner,
        }
    }
}

impl Read for ReplayByteStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let prefix_bytes = self.prefix.read(output)?;
        if prefix_bytes > 0 {
            Ok(prefix_bytes)
        } else {
            self.inner.read(output)
        }
    }
}

/// Ordered source wrapper сохраняет segment boundaries после bounded sniff.
struct ReplayOrderedSegmentSource {
    /// Уже прочитанные exact segments.
    replay_segments: VecDeque<OrderedSegment>,
    /// Original source после последнего sniffed segment-а.
    inner: Box<dyn OrderedSegmentSource>,
}

impl ReplayOrderedSegmentSource {
    /// Передаёт ownership prefix queue wrapper-у.
    fn new(
        inner: Box<dyn OrderedSegmentSource>,
        replay_segments: VecDeque<OrderedSegment>,
    ) -> Self {
        Self {
            replay_segments,
            inner,
        }
    }
}

impl OrderedSegmentSource for ReplayOrderedSegmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        if let Some(segment) = self.replay_segments.pop_front() {
            Ok(Some(segment))
        } else {
            self.inner.next_segment(cancellation)
        }
    }
}
