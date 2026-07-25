//! Ordinary finite video OrderedSegmentSource wrapper.

use demux_api::{
    OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSequence, OrderedSegmentSource,
};
use source_core::CancellationToken;

use super::cursor::{
    SmoothCursorItem, SmoothCursorMedia, SmoothCursorReadError, SmoothFragmentCursor,
};

/// Lazy selected video source без presentation-window field.
pub struct SmoothVideoFragmentSource {
    pub(super) cursor: SmoothFragmentCursor,
}

impl SmoothVideoFragmentSource {
    /// Возвращает общий source cancellation для worker boundary.
    pub(crate) fn cancellation(&self) -> &CancellationToken {
        self.cursor.cancellation()
    }
}

#[cfg(test)]
impl SmoothVideoFragmentSource {
    /// Test-only intent boundary для повторного EOS без доступа к cursor storage.
    pub(super) fn end_after_initialization_for_test(&mut self) {
        self.cursor.end_after_initialization_for_test();
    }
}

impl std::fmt::Debug for SmoothVideoFragmentSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmoothVideoFragmentSource")
            .finish_non_exhaustive()
    }
}

impl OrderedSegmentSource for SmoothVideoFragmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        match self.cursor.next(cancellation).map_err(map_cursor_error)? {
            SmoothCursorItem::Initialization { sequence, bytes } => Ok(Some(OrderedSegment {
                sequence: OrderedSegmentSequence::new(sequence),
                kind: OrderedSegmentKind::Initialization,
                discontinuity: OrderedSegmentDiscontinuity::Continuous,
                bytes,
            })),
            SmoothCursorItem::Media { sequence, media } => {
                let SmoothCursorMedia::Video { bytes } = media else {
                    return Err(OrderedSegmentReadError::Failed {
                        reason: "smooth video source received wrong media axis".to_owned(),
                    });
                };
                Ok(Some(OrderedSegment {
                    sequence: OrderedSegmentSequence::new(sequence),
                    kind: OrderedSegmentKind::Media,
                    discontinuity: OrderedSegmentDiscontinuity::Continuous,
                    bytes,
                }))
            }
            SmoothCursorItem::EndOfStream => Ok(None),
        }
    }
}

fn map_cursor_error(error: SmoothCursorReadError) -> OrderedSegmentReadError {
    match error {
        SmoothCursorReadError::Cancelled => OrderedSegmentReadError::Cancelled,
        SmoothCursorReadError::Failed(failure) => OrderedSegmentReadError::Failed {
            reason: failure.reason().to_owned(),
        },
    }
}
