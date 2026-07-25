//! Window-aware finite audio source wrapper.

use demux_api::{
    OrderedSegmentDiscontinuity, OrderedSegmentReadError, OrderedSegmentSequence,
    PresentationWindowOrderedSegment, PresentationWindowOrderedSegmentReadOutcome,
    PresentationWindowOrderedSegmentSource,
};
use source_core::CancellationToken;

use super::cursor::{
    SmoothCursorItem, SmoothCursorMedia, SmoothCursorReadError, SmoothFragmentCursor,
};

/// Lazy selected audio source с authoritative F3A packet windows.
pub struct SmoothAudioFragmentSource {
    pub(super) cursor: SmoothFragmentCursor,
}

impl std::fmt::Debug for SmoothAudioFragmentSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmoothAudioFragmentSource")
            .finish_non_exhaustive()
    }
}

impl PresentationWindowOrderedSegmentSource for SmoothAudioFragmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<PresentationWindowOrderedSegmentReadOutcome, OrderedSegmentReadError> {
        match self.cursor.next(cancellation).map_err(map_cursor_error)? {
            SmoothCursorItem::Initialization { sequence, bytes } => {
                Ok(PresentationWindowOrderedSegmentReadOutcome::Segment(
                    PresentationWindowOrderedSegment::Initialization {
                        sequence: OrderedSegmentSequence::new(sequence),
                        discontinuity: OrderedSegmentDiscontinuity::Continuous,
                        bytes,
                    },
                ))
            }
            SmoothCursorItem::Media { sequence, media } => {
                let SmoothCursorMedia::Audio {
                    bytes,
                    presentation_window,
                } = media
                else {
                    return Err(OrderedSegmentReadError::Failed {
                        reason: "smooth audio source received wrong media axis".to_owned(),
                    });
                };
                Ok(PresentationWindowOrderedSegmentReadOutcome::Segment(
                    PresentationWindowOrderedSegment::Media {
                        sequence: OrderedSegmentSequence::new(sequence),
                        discontinuity: OrderedSegmentDiscontinuity::Continuous,
                        bytes,
                        presentation_window,
                    },
                ))
            }
            SmoothCursorItem::EndOfStream => {
                Ok(PresentationWindowOrderedSegmentReadOutcome::EndOfStream)
            }
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
