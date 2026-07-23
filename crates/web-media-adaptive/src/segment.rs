//! Explicit nonblocking ordered segment delivery.

use std::collections::VecDeque;
use std::num::{NonZeroU8, NonZeroUsize};
use std::time::{Duration, Instant};

use bytes::Bytes;
use demux_api::{
    OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentSequence,
};
use source_core::{HttpBoundedByteRange, HttpRequestTarget};
use web_media_transport_api::{MediaPresentation, SourceGeneration};

use crate::fetch::{FetchExecutor, FetchJob, FetchOutcome, FetchPurpose};
use crate::{
    AdaptiveHttpContext, AdaptivePresentation, AdaptiveTransportError, ComponentClockMetadata,
};

/// Exact HTTP byte range одного media segment-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentByteRange(HttpBoundedByteRange);

impl SegmentByteRange {
    /// Создаёт checked non-empty range.
    pub fn new(start: u64, length: NonZeroUsize) -> Result<Self, SourceRangeError> {
        HttpBoundedByteRange::new(start, length)
            .map(Self)
            .map_err(|_| SourceRangeError::Overflow)
    }

    const fn into_source_range(self) -> HttpBoundedByteRange {
        self.0
    }
}

/// Ошибка representability byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceRangeError {
    /// Конечный offset не помещается в `u64`.
    #[error("segment byte range переполняет u64")]
    Overflow,
}

/// Protocol parser-owned descriptor без container parsing.
#[derive(Debug, Clone)]
pub struct AdaptiveSegmentDescriptor {
    sequence: OrderedSegmentSequence,
    kind: OrderedSegmentKind,
    discontinuity: OrderedSegmentDiscontinuity,
    target: HttpRequestTarget,
    byte_range: Option<SegmentByteRange>,
}

impl AdaptiveSegmentDescriptor {
    /// Создаёт full-resource segment.
    #[must_use]
    pub const fn full(
        sequence: OrderedSegmentSequence,
        kind: OrderedSegmentKind,
        discontinuity: OrderedSegmentDiscontinuity,
        target: HttpRequestTarget,
    ) -> Self {
        Self {
            sequence,
            kind,
            discontinuity,
            target,
            byte_range: None,
        }
    }

    /// Создаёт exact Range segment.
    #[must_use]
    pub const fn range(
        sequence: OrderedSegmentSequence,
        kind: OrderedSegmentKind,
        discontinuity: OrderedSegmentDiscontinuity,
        target: HttpRequestTarget,
        byte_range: SegmentByteRange,
    ) -> Self {
        Self {
            sequence,
            kind,
            discontinuity,
            target,
            byte_range: Some(byte_range),
        }
    }

    /// Transport sequence для diagnostics/tests.
    #[must_use]
    pub const fn sequence(&self) -> OrderedSegmentSequence {
        self.sequence
    }
}

/// Immutable generation snapshot, подготовленный concrete manifest policy.
#[derive(Debug)]
pub struct AdaptiveSegmentSnapshot {
    generation: SourceGeneration,
    presentation: AdaptivePresentation,
    component_clock: ComponentClockMetadata,
    segments: Vec<AdaptiveSegmentDescriptor>,
    completion: AdaptiveSegmentCompletion,
}

/// Terminal semantics published вместе со snapshot-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveSegmentCompletion {
    /// После текущих descriptors ожидается live refresh/new generation.
    AwaitRefresh,
    /// После текущих descriptors источник имеет explicit terminal end.
    EndAfterSnapshot,
}

impl AdaptiveSegmentSnapshot {
    /// Проверяет strict ordering; resource count проверяется owner config-ом при install.
    pub fn new(
        generation: SourceGeneration,
        presentation: AdaptivePresentation,
        component_clock: ComponentClockMetadata,
        segments: Vec<AdaptiveSegmentDescriptor>,
        completion: AdaptiveSegmentCompletion,
    ) -> Result<Self, AdaptiveSegmentSnapshotError> {
        if segments
            .windows(2)
            .any(|pair| pair[0].sequence.get() >= pair[1].sequence.get())
        {
            return Err(AdaptiveSegmentSnapshotError::NonMonotonicSequence);
        }
        Ok(Self {
            generation,
            presentation,
            component_clock,
            segments,
            completion,
        })
    }
}

/// Invalid protocol-owned snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdaptiveSegmentSnapshotError {
    /// Sequence обязан строго возрастать внутри snapshot-а.
    #[error("adaptive segment snapshot содержит немонотонную sequence")]
    NonMonotonicSequence,
    /// Snapshot превысил caller-owned descriptor bound.
    #[error("adaptive segment snapshot превышает descriptor limit")]
    SegmentLimitExceeded,
    /// Same/older generation не может заменить уже installed snapshot.
    #[error("adaptive segment snapshot generation не продвигает lifecycle")]
    NonAdvancingGeneration,
    /// Manifest nature обязана совпадать с S21T open request.
    #[error("adaptive segment snapshot presentation не совпадает с transport request")]
    PresentationMismatch,
}

/// Explicit player-neutral result одного poll-а.
#[derive(Debug)]
pub enum SegmentPoll {
    /// Следующий ordered segment готов.
    Segment(OrderedSegment),
    /// Segment/live refresh ещё не готов; это не EOF и не error.
    TemporarilyUnavailable {
        /// Earliest useful next poll.
        retry_after: Duration,
    },
    /// VOD/explicit terminal live source завершён.
    EndOfStream,
    /// Typed terminal failure current segment-а.
    Failed(AdaptiveTransportError),
    /// Shared cancellation завершила lifecycle.
    Cancelled,
}

#[derive(Debug, Clone)]
struct ActiveSegment {
    descriptor: AdaptiveSegmentDescriptor,
    attempt: NonZeroU8,
    retry_not_before: Instant,
    job_id: u64,
    submitted: bool,
}

/// Owner bounded descriptors, retry state и ordered nonblocking delivery.
pub struct AdaptiveOrderedSegmentSource {
    context: AdaptiveHttpContext,
    executor: FetchExecutor,
    current_generation: SourceGeneration,
    snapshot_installed: bool,
    presentation: Option<AdaptivePresentation>,
    component_clock: Option<ComponentClockMetadata>,
    pending: VecDeque<AdaptiveSegmentDescriptor>,
    active: Option<ActiveSegment>,
    last_delivered_sequence: Option<OrderedSegmentSequence>,
    terminal_after_pending: bool,
    next_job_id: u64,
}

impl AdaptiveOrderedSegmentSource {
    /// Создаёт пустой source; initial readiness явно temporary-unavailable.
    pub fn new(context: AdaptiveHttpContext) -> Result<Self, AdaptiveTransportError> {
        let executor = FetchExecutor::start(context.clone())?;
        Ok(Self {
            current_generation: context.initial_generation,
            context,
            executor,
            snapshot_installed: false,
            presentation: None,
            component_clock: None,
            pending: VecDeque::new(),
            active: None,
            last_delivered_sequence: None,
            terminal_after_pending: false,
            next_job_id: 1,
        })
    }

    /// Atomically публикует initial/newer manifest generation.
    pub fn install_snapshot(
        &mut self,
        mut snapshot: AdaptiveSegmentSnapshot,
    ) -> Result<(), AdaptiveSegmentSnapshotError> {
        if snapshot.segments.len() > self.context.limits.maximum_snapshot_segments.get() {
            return Err(AdaptiveSegmentSnapshotError::SegmentLimitExceeded);
        }
        let snapshot_presentation = match snapshot.presentation {
            AdaptivePresentation::Vod { .. } => MediaPresentation::Vod,
            AdaptivePresentation::Live { .. } => MediaPresentation::Live,
        };
        if snapshot_presentation != self.context.expected_presentation {
            return Err(AdaptiveSegmentSnapshotError::PresentationMismatch);
        }
        if self.snapshot_installed && snapshot.generation <= self.current_generation {
            return Err(AdaptiveSegmentSnapshotError::NonAdvancingGeneration);
        }
        if snapshot.generation < self.current_generation {
            return Err(AdaptiveSegmentSnapshotError::NonAdvancingGeneration);
        }
        if let Some(last_delivered_sequence) = self.last_delivered_sequence {
            snapshot
                .segments
                .retain(|segment| segment.sequence.get() > last_delivered_sequence.get());
        }
        self.current_generation = snapshot.generation;
        self.snapshot_installed = true;
        self.presentation = Some(snapshot.presentation);
        self.component_clock = Some(snapshot.component_clock);
        self.pending = snapshot.segments.into();
        self.active = None;
        self.terminal_after_pending =
            snapshot.completion == AdaptiveSegmentCompletion::EndAfterSnapshot;
        Ok(())
    }

    /// Текущая VOD/live-edge/DVR metadata.
    #[must_use]
    pub const fn presentation(&self) -> Option<AdaptivePresentation> {
        self.presentation
    }

    /// Component-local clock; соседний audio/video source имеет отдельное значение.
    #[must_use]
    pub const fn component_clock(&self) -> Option<ComponentClockMetadata> {
        self.component_clock
    }

    /// Poll-ит готовность без ожидания network/thread completion.
    pub fn poll_next(&mut self, now: Instant) -> SegmentPoll {
        if self.context.cancellation.is_cancelled() {
            return SegmentPoll::Cancelled;
        }
        match self.executor.try_receive() {
            Ok(Some(outcome)) => {
                if let Some(result) = self.accept_outcome(outcome, now) {
                    return result;
                }
            }
            Ok(None) => {}
            Err(error) => return SegmentPoll::Failed(error),
        }

        if self.active.is_none()
            && let Some(descriptor) = self.pending.pop_front()
        {
            let job_id = self.allocate_job_id();
            self.active = Some(ActiveSegment {
                descriptor,
                attempt: NonZeroU8::MIN,
                retry_not_before: now,
                job_id,
                submitted: false,
            });
        }

        let Some(active) = &mut self.active else {
            return if self.terminal_after_pending {
                SegmentPoll::EndOfStream
            } else {
                SegmentPoll::TemporarilyUnavailable {
                    retry_after: Duration::from_millis(1),
                }
            };
        };
        if now < active.retry_not_before {
            return SegmentPoll::TemporarilyUnavailable {
                retry_after: active.retry_not_before.duration_since(now),
            };
        }
        if !active.submitted {
            let byte_range = active
                .descriptor
                .byte_range
                .map(SegmentByteRange::into_source_range);
            let maximum_body_bytes = byte_range
                .map(HttpBoundedByteRange::length)
                .unwrap_or(self.context.limits.maximum_segment_bytes);
            let job = FetchJob {
                id: active.job_id,
                generation: self.current_generation,
                target: active.descriptor.target.clone(),
                byte_range,
                maximum_body_bytes,
                purpose: FetchPurpose::MediaSegment,
            };
            match self.executor.try_submit(job) {
                Ok(submitted) => active.submitted = submitted,
                Err(error) => return SegmentPoll::Failed(error),
            }
        }
        SegmentPoll::TemporarilyUnavailable {
            retry_after: Duration::from_millis(1),
        }
    }

    fn accept_outcome(&mut self, outcome: FetchOutcome, now: Instant) -> Option<SegmentPoll> {
        let active = self.active.as_mut()?;
        if outcome.id != active.job_id || outcome.generation != self.current_generation {
            return None;
        }
        match outcome.result {
            Ok(success) => {
                let active = self.active.take().expect("active segment");
                self.last_delivered_sequence = Some(active.descriptor.sequence);
                Some(SegmentPoll::Segment(OrderedSegment {
                    sequence: active.descriptor.sequence,
                    kind: active.descriptor.kind,
                    discontinuity: active.descriptor.discontinuity,
                    bytes: Bytes::from(success.bytes),
                }))
            }
            Err(AdaptiveTransportError::Cancelled) => {
                self.active = None;
                Some(SegmentPoll::Cancelled)
            }
            Err(error)
                if error.is_retryable()
                    && active.attempt.get() < self.context.retry.maximum_attempts().get() =>
            {
                let delay = self.context.retry.backoff_after(active.attempt);
                active.attempt = NonZeroU8::new(active.attempt.get() + 1).expect("bounded attempt");
                active.retry_not_before = now + delay;
                active.job_id = self.next_job_id;
                self.next_job_id = self.next_job_id.wrapping_add(1).max(1);
                active.submitted = false;
                Some(SegmentPoll::TemporarilyUnavailable { retry_after: delay })
            }
            Err(error) => {
                self.active = None;
                Some(SegmentPoll::Failed(error))
            }
        }
    }

    fn allocate_job_id(&mut self) -> u64 {
        let allocated = self.next_job_id;
        self.next_job_id = self.next_job_id.wrapping_add(1).max(1);
        allocated
    }
}
