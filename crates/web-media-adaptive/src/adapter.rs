//! Узкий compatibility adapter existing finite ordered demux factories.

use std::thread;
use std::time::{Duration, Instant};

use demux_api::{
    OrderedSegment, OrderedSegmentReadError, OrderedSegmentSource, ProgressiveDemuxBufferLimits,
    ProgressiveDemuxStartupError, ProgressiveDemuxer,
};
use media_core::{DemuxRetryHint, Demuxer};
use source_core::CancellationToken;

use crate::{AdaptiveOrderedSegmentSource, SegmentPoll};

const CANCELLATION_OBSERVATION_INTERVAL: Duration = Duration::from_millis(25);
const MAX_SAFE_REASON_BYTES: usize = 256;

/// Blocking facade, которую разрешено читать только внутри demux worker-а.
pub struct BlockingOrderedSegmentAdapter {
    source: AdaptiveOrderedSegmentSource,
}

impl BlockingOrderedSegmentAdapter {
    /// Забирает единоличное владение nonblocking adaptive source-ом.
    #[must_use]
    pub const fn new(source: AdaptiveOrderedSegmentSource) -> Self {
        Self { source }
    }

    /// Запускает registry sniff/open и parser reads за player-owner boundary.
    ///
    /// Initial segment prefetch выполняется тем же worker-ом. Поэтому registry
    /// никогда не получает fake EOF, а player-facing demuxer до готовности
    /// возвращает существующий `DemuxReadEvent::TemporarilyUnavailable`.
    pub fn open_deferred<F>(
        source: AdaptiveOrderedSegmentSource,
        cancellation: CancellationToken,
        limits: ProgressiveDemuxBufferLimits,
        retry_hint: DemuxRetryHint,
        open_inner: F,
    ) -> Result<ProgressiveDemuxer, ProgressiveDemuxStartupError>
    where
        F: FnOnce(Box<dyn OrderedSegmentSource>) -> anyhow::Result<Box<dyn Demuxer + Send>>
            + Send
            + 'static,
    {
        ProgressiveDemuxer::new_deferred(
            move || open_inner(Box::new(Self::new(source))),
            cancellation,
            limits,
            retry_hint,
        )
    }
}

impl OrderedSegmentSource for BlockingOrderedSegmentAdapter {
    /// Ждёт readiness только на выделенном demux worker-е.
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        loop {
            if cancellation.is_cancelled() {
                return Err(OrderedSegmentReadError::Cancelled);
            }
            match self.source.poll_next(Instant::now()) {
                SegmentPoll::Segment(segment) => return Ok(Some(segment)),
                SegmentPoll::EndOfStream => return Ok(None),
                SegmentPoll::Cancelled => {
                    return Err(OrderedSegmentReadError::Cancelled);
                }
                SegmentPoll::Failed(error) => {
                    return Err(OrderedSegmentReadError::Failed {
                        reason: bounded_reason(&error.to_string()),
                    });
                }
                SegmentPoll::TemporarilyUnavailable { retry_after } => {
                    thread::park_timeout(retry_after.min(CANCELLATION_OBSERVATION_INTERVAL));
                }
            }
        }
    }
}

fn bounded_reason(reason: &str) -> String {
    let mut boundary = reason.len().min(MAX_SAFE_REASON_BYTES);
    while !reason.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    reason[..boundary].to_owned()
}
