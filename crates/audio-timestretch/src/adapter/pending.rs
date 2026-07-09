//! Ordered pending-output accounting probe backend-а.

use std::collections::VecDeque;

use audio_core::{
    AudioTempoFrameCount, AudioTempoOutputSegmentSpan, AudioTempoOutputSegmentSpans,
    AudioTempoPcmFormat, AudioTempoSegment,
};

use super::TimestretchTempoError;

/// Ordered actual pending output concrete probe backend-а.
#[derive(Debug, Clone, Copy)]
pub(super) struct TimestretchPendingOutputSpan {
    segment: AudioTempoSegment,
    frame_count: u64,
}

/// Добавляет scheduled pending span и объединяет соседний тот же segment.
pub(super) fn push_timestretch_pending_span(
    queue: &mut VecDeque<TimestretchPendingOutputSpan>,
    segment: AudioTempoSegment,
    frame_count: u64,
) -> Result<(), TimestretchTempoError> {
    if frame_count == 0 {
        return Ok(());
    }
    if let Some(last_span) = queue.back_mut()
        && last_span.segment == segment
    {
        last_span.frame_count =
            last_span
                .frame_count
                .checked_add(frame_count)
                .ok_or_else(|| TimestretchTempoError::BoundaryInvariant {
                    message: "pending segment frame count overflow".to_owned(),
                })?;
        return Ok(());
    }
    queue.push_back(TimestretchPendingOutputSpan {
        segment,
        frame_count,
    });
    Ok(())
}

/// Вынимает produced frames из начала ordered pending queue.
pub(super) fn drain_timestretch_pending_spans(
    queue: &mut VecDeque<TimestretchPendingOutputSpan>,
    frame_count: u64,
    pcm_format: AudioTempoPcmFormat,
) -> Result<AudioTempoOutputSegmentSpans, TimestretchTempoError> {
    let mut remaining_frames = frame_count;
    let mut produced_spans = AudioTempoOutputSegmentSpans::default();
    while remaining_frames > 0 {
        let Some(front_span) = queue.front_mut() else {
            return Err(TimestretchTempoError::BoundaryInvariant {
                message: format!(
                    "pending segment queue underflow: {remaining_frames} frames are missing"
                ),
            });
        };
        let produced_frames = remaining_frames.min(front_span.frame_count);
        produced_spans.push(AudioTempoOutputSegmentSpan::new(
            pcm_format,
            front_span.segment,
            AudioTempoFrameCount::new(produced_frames),
        ));
        front_span.frame_count -= produced_frames;
        remaining_frames -= produced_frames;
        if front_span.frame_count == 0 {
            queue.pop_front();
        }
    }
    Ok(produced_spans)
}

/// Копирует pending queue в inline neutral collection без мутации backend state.
pub(super) fn timestretch_pending_spans_view(
    queue: &VecDeque<TimestretchPendingOutputSpan>,
    pcm_format: AudioTempoPcmFormat,
) -> AudioTempoOutputSegmentSpans {
    let mut pending_spans = AudioTempoOutputSegmentSpans::default();
    for span in queue {
        pending_spans.push(AudioTempoOutputSegmentSpan::new(
            pcm_format,
            span.segment,
            AudioTempoFrameCount::new(span.frame_count),
        ));
    }
    pending_spans
}

/// Считает actual pending frames без saturating arithmetic.
pub(super) fn sum_timestretch_pending_frames(
    queue: &VecDeque<TimestretchPendingOutputSpan>,
) -> Result<u64, TimestretchTempoError> {
    queue.iter().try_fold(0u64, |total, span| {
        total.checked_add(span.frame_count).ok_or_else(|| {
            TimestretchTempoError::BoundaryInvariant {
                message: "pending segment total overflow".to_owned(),
            }
        })
    })
}
