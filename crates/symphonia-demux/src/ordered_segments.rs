//! Bounded adapter neutral ordered segments -> forward-only Symphonia byte stream.
//!
//! Этот модуль валидирует только transport lifecycle и sequence. Container bytes
//! по-прежнему разбирает единственный зарегистрированный Symphonia format reader.

use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use demux_api::{
    OrderedSegment, OrderedSegmentKind, OrderedSegmentReadError, OrderedSegmentSource,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, MediaMetadata, TimelineNotSeekableReason, TrackInfo,
};
use source_core::CancellationToken;

use crate::{DemuxError, SymphoniaDemuxer};

/// Typed нарушение container-neutral finite ordered lifecycle-а.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrderedSegmentLifecycleError {
    /// Source завершился до обязательного initialization segment-а.
    #[error("ordered media source завершился до initialization segment")]
    MissingInitializationSegment,
    /// Media segment нельзя интерпретировать без первого initialization segment-а.
    #[error("ordered media segment {sequence} получен до initialization segment")]
    MediaBeforeInitialization {
        /// Transport-owned sequence ошибочного media segment-а.
        sequence: u64,
    },
    /// Finite profile не поддерживает смену initialization state/discontinuity.
    #[error("ordered media source повторно получил initialization segment {sequence}")]
    RepeatedInitializationSegment {
        /// Transport-owned sequence повторного initialization segment-а.
        sequence: u64,
    },
    /// Media sequence обязан строго возрастать; gaps допустимы, duplicate/decrease — нет.
    #[error(
        "ordered media sequence не возрастает: previous={previous_sequence}, current={current_sequence}"
    )]
    NonIncreasingSequence {
        /// Последний принятый init/media sequence.
        previous_sequence: u64,
        /// Duplicate или decreasing media sequence.
        current_sequence: u64,
    },
    /// Пустой segment не является initialization либо media segment profile-а.
    #[error("ordered media segment {sequence} с ролью {kind:?} не содержит bytes")]
    EmptySegment {
        /// Transport-owned sequence пустого segment-а.
        sequence: u64,
        /// Объявленная transport role пустого segment-а.
        kind: OrderedSegmentKind,
    },
}

/// Forward-only reader с mutex только для требуемого Symphonia `Sync` bound-а.
pub(crate) struct OrderedSegmentReader {
    /// Единственный mutable owner source/lifecycle/current-segment state-а.
    state: Mutex<OrderedSegmentReaderState>,
}

/// Наблюдатель сохраняет первую typed adapter failure, которую eager probe может скрыть.
#[derive(Clone, Default)]
pub(crate) struct OrderedSegmentFailureObserver {
    /// Shared slot не владеет source bytes и не меняет bounded buffering.
    failure: Arc<Mutex<Option<OrderedSegmentFailure>>>,
}

/// Cloneable exact failure, пригодная для восстановления public error boundary.
#[derive(Clone)]
enum OrderedSegmentFailure {
    /// Нарушение init/media lifecycle.
    Lifecycle(OrderedSegmentLifecycleError),
    /// Operational failure neutral source-а.
    Source(OrderedSegmentReadError),
}

impl OrderedSegmentFailureObserver {
    /// Запоминает первую ошибку: последующие probe retries не должны её затереть.
    fn record(&self, failure: OrderedSegmentFailure) {
        let mut failure_slot = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure_slot.is_none() {
            *failure_slot = Some(failure);
        }
    }

    /// Возвращает concrete demux error без потери исходного typed значения.
    pub(crate) fn demux_error(&self) -> Option<DemuxError> {
        let failure_slot = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match failure_slot.as_ref()? {
            OrderedSegmentFailure::Lifecycle(error) => Some(error.clone().into()),
            OrderedSegmentFailure::Source(error) => Some(error.clone().into()),
        }
    }
}

/// Runtime boundary finite ordered media поверх existing Symphonia parser-а.
pub(crate) struct OrderedSegmentDemuxer {
    /// Единственный container parser; wrapper владеет только source-level policy.
    inner: SymphoniaDemuxer,
    /// Текущий public track snapshot с ordered-only нормализацией unknown duration.
    tracks: Vec<TrackInfo>,
    /// Container duration с ordered-only нормализацией zero как unknown.
    duration: Option<Duration>,
}

impl OrderedSegmentDemuxer {
    /// Фиксирует начальный track snapshot и non-seekable policy ordered source-а.
    pub(crate) fn new(inner: SymphoniaDemuxer) -> Self {
        let tracks = normalized_ordered_tracks(inner.tracks());
        let duration = inner.duration().filter(|duration| !duration.is_zero());
        Self {
            inner,
            tracks,
            duration,
        }
    }
}

/// Нормализует только ordered-specific представление неизвестной длительности.
fn normalized_ordered_tracks(tracks: &[TrackInfo]) -> Vec<TrackInfo> {
    tracks
        .iter()
        .cloned()
        .map(|mut track| {
            track.duration = track.duration.filter(|duration| !duration.is_zero());
            track
        })
        .collect()
}

impl Demuxer for OrderedSegmentDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.inner.media_metadata()
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        }
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        match self.inner.next_event()? {
            DemuxReadEvent::TracksChanged(track_update) => {
                self.tracks = normalized_ordered_tracks(&track_update.tracks);
                self.duration = track_update.duration.filter(|duration| !duration.is_zero());
                Ok(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
                    self.tracks.clone(),
                    self.duration,
                )))
            }
            event => Ok(event),
        }
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(DemuxError::SeekUnavailable(
            "finite ordered media input не поддерживает seek".to_owned(),
        )
        .into())
    }

    fn seek_with_request(&mut self, _request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        Err(DemuxError::SeekUnavailable(
            "finite ordered media input не поддерживает seek".to_owned(),
        )
        .into())
    }
}

impl OrderedSegmentReader {
    /// Принимает owned source и shared cancellation без чтения наперёд.
    #[cfg(test)]
    pub(crate) fn new(
        source: Box<dyn OrderedSegmentSource>,
        cancellation: CancellationToken,
    ) -> Self {
        Self::new_observed(source, cancellation).0
    }

    /// Создаёт reader и observer для factory open, где Symphonia может скрыть probe I/O error.
    pub(crate) fn new_observed(
        source: Box<dyn OrderedSegmentSource>,
        cancellation: CancellationToken,
    ) -> (Self, OrderedSegmentFailureObserver) {
        let failure_observer = OrderedSegmentFailureObserver::default();
        let reader = Self {
            state: Mutex::new(OrderedSegmentReaderState {
                source,
                cancellation,
                failure_observer: failure_observer.clone(),
                lifecycle: OrderedSegmentLifecycle::AwaitingInitialization,
                current_bytes: Bytes::new(),
                current_offset: 0,
            }),
        };
        (reader, failure_observer)
    }
}

impl Read for OrderedSegmentReader {
    /// Возвращает bytes только одного текущего segment-а за вызов и не склеивает очередь.
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        if destination.is_empty() {
            return Ok(0);
        }
        self.state
            .lock()
            .map_err(|_| io::Error::other("ordered media reader state poisoned"))?
            .read_into(destination)
    }
}

/// Mutable bounded state: source + lifecycle + ровно один retained segment.
struct OrderedSegmentReaderState {
    /// Manifest/network owner остаётся за neutral source implementation.
    source: Box<dyn OrderedSegmentSource>,
    /// Shared cooperative cancellation действует на каждый source read.
    cancellation: CancellationToken,
    /// First-failure observer нужен только для eager probe error recovery.
    failure_observer: OrderedSegmentFailureObserver,
    /// Finite init/media lifecycle без manifest/live semantics.
    lifecycle: OrderedSegmentLifecycle,
    /// Immutable bytes только текущего segment-а.
    current_bytes: Bytes,
    /// Следующий unread byte внутри `current_bytes`.
    current_offset: usize,
}

impl OrderedSegmentReaderState {
    /// Заполняет destination остатком current segment-а либо загружает ровно следующий segment.
    fn read_into(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        self.ensure_active()?;
        if self.current_offset < self.current_bytes.len() {
            return Ok(self.copy_current_bytes(destination));
        }
        if matches!(self.lifecycle, OrderedSegmentLifecycle::Finished) {
            return Ok(0);
        }

        let next_segment = match self.source.next_segment(&self.cancellation) {
            Ok(segment) => segment,
            Err(error) => return Err(self.source_error(error)),
        };
        let Some(segment) = next_segment else {
            return self.finish();
        };
        self.install_segment(segment)?;
        Ok(self.copy_current_bytes(destination))
    }

    /// Cancellation проверяется до выдачи buffered bytes, чтобы stale read не продолжался.
    fn ensure_active(&self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            Err(self.source_error(OrderedSegmentReadError::Cancelled))
        } else {
            Ok(())
        }
    }

    /// Валидирует lifecycle без частичной mutation, затем устанавливает current segment.
    fn install_segment(&mut self, segment: OrderedSegment) -> io::Result<()> {
        let sequence = segment.sequence.get();
        let next_lifecycle = match (self.lifecycle, segment.kind) {
            (
                OrderedSegmentLifecycle::AwaitingInitialization,
                OrderedSegmentKind::Initialization,
            ) => OrderedSegmentLifecycle::ReadingMedia {
                previous_sequence: sequence,
            },
            (OrderedSegmentLifecycle::AwaitingInitialization, OrderedSegmentKind::Media) => {
                return Err(self.lifecycle_error(
                    OrderedSegmentLifecycleError::MediaBeforeInitialization { sequence },
                ));
            }
            (OrderedSegmentLifecycle::ReadingMedia { .. }, OrderedSegmentKind::Initialization) => {
                return Err(self.lifecycle_error(
                    OrderedSegmentLifecycleError::RepeatedInitializationSegment { sequence },
                ));
            }
            (
                OrderedSegmentLifecycle::ReadingMedia { previous_sequence },
                OrderedSegmentKind::Media,
            ) if sequence <= previous_sequence => {
                return Err(self.lifecycle_error(
                    OrderedSegmentLifecycleError::NonIncreasingSequence {
                        previous_sequence,
                        current_sequence: sequence,
                    },
                ));
            }
            (OrderedSegmentLifecycle::ReadingMedia { .. }, OrderedSegmentKind::Media) => {
                OrderedSegmentLifecycle::ReadingMedia {
                    previous_sequence: sequence,
                }
            }
            (OrderedSegmentLifecycle::Finished, _) => {
                return Err(io::Error::other(
                    "ordered media reader получил segment после terminal EOF",
                ));
            }
        };

        if segment.bytes.is_empty() {
            return Err(
                self.lifecycle_error(OrderedSegmentLifecycleError::EmptySegment {
                    sequence,
                    kind: segment.kind,
                }),
            );
        }
        self.lifecycle = next_lifecycle;
        self.current_bytes = segment.bytes;
        self.current_offset = 0;
        Ok(())
    }

    /// Terminal EOF допустим только после единственного initialization segment-а.
    fn finish(&mut self) -> io::Result<usize> {
        match self.lifecycle {
            OrderedSegmentLifecycle::AwaitingInitialization => {
                Err(self
                    .lifecycle_error(OrderedSegmentLifecycleError::MissingInitializationSegment))
            }
            OrderedSegmentLifecycle::ReadingMedia { .. } => {
                self.lifecycle = OrderedSegmentLifecycle::Finished;
                self.current_bytes = Bytes::new();
                self.current_offset = 0;
                Ok(0)
            }
            OrderedSegmentLifecycle::Finished => Ok(0),
        }
    }

    /// Записывает lifecycle failure до преобразования в стандартный `Read` error.
    fn lifecycle_error(&self, error: OrderedSegmentLifecycleError) -> io::Error {
        self.failure_observer
            .record(OrderedSegmentFailure::Lifecycle(error.clone()));
        lifecycle_error(error)
    }

    /// Записывает source failure до преобразования в стандартный `Read` error.
    fn source_error(&self, error: OrderedSegmentReadError) -> io::Error {
        self.failure_observer
            .record(OrderedSegmentFailure::Source(error.clone()));
        segment_read_error(error)
    }

    /// Копирует bounded slice и освобождает retained bytes после полного consumption.
    fn copy_current_bytes(&mut self, destination: &mut [u8]) -> usize {
        let available_bytes = &self.current_bytes[self.current_offset..];
        let copied_bytes = available_bytes.len().min(destination.len());
        destination[..copied_bytes].copy_from_slice(&available_bytes[..copied_bytes]);
        self.current_offset += copied_bytes;
        if self.current_offset == self.current_bytes.len() {
            self.current_bytes = Bytes::new();
            self.current_offset = 0;
        }
        copied_bytes
    }
}

/// Finite lifecycle state не принимает manifest refresh/discontinuity semantics.
#[derive(Clone, Copy)]
enum OrderedSegmentLifecycle {
    /// Первый segment обязан быть Initialization.
    AwaitingInitialization,
    /// Init принят; дальше допустимы только strictly increasing Media sequences.
    ReadingMedia {
        /// Последний принятый init/media sequence.
        previous_sequence: u64,
    },
    /// Source сообщил terminal EOF.
    Finished,
}

/// Сохраняет neutral source error в стандартной `Read` error chain.
fn segment_read_error(error: OrderedSegmentReadError) -> io::Error {
    let kind = match error {
        OrderedSegmentReadError::Cancelled => io::ErrorKind::Interrupted,
        OrderedSegmentReadError::Failed { .. } => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

/// Переносит concrete lifecycle error через стандартный `Read` boundary.
fn lifecycle_error(error: OrderedSegmentLifecycleError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests;
