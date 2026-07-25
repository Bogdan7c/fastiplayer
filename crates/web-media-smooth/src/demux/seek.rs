//! Pure P5 fragment-anchor planning без HTTP и parser duplication.

use std::time::Duration;

use media_core::{DemuxSeekRequest, DemuxSeekResult, MediaTime};
use smooth_streaming_manifest_core::SmoothChunkTimeline;

use crate::source::SmoothSelectedSourceFactory;

use super::error::SmoothVodSeekError;

/// Immutable pair of component anchors и player-facing preview.
pub(super) struct SmoothSeekPlan {
    pub(super) video_fragment_index: usize,
    pub(super) audio_fragment_index: usize,
    pub(super) result: DemuxSeekResult,
}

impl SmoothSeekPlan {
    /// Выводит RAP video anchor и audio fragment at/before одного target-а.
    pub(super) fn for_request(
        source_factory: &SmoothSelectedSourceFactory,
        request: DemuxSeekRequest,
        duration: Duration,
    ) -> Result<Self, SmoothVodSeekError> {
        if request.timestamp > duration {
            return Err(SmoothVodSeekError::TargetOutsideDuration);
        }
        let manifest = source_factory.manifest();
        let video_stream = manifest
            .streams()
            .get(source_factory.video_selection().stream_ordinal.get())
            .ok_or(SmoothVodSeekError::StreamMissing)?;
        let audio_stream = manifest
            .streams()
            .get(source_factory.audio_selection().stream_ordinal.get())
            .ok_or(SmoothVodSeekError::StreamMissing)?;
        let video_fragment_index =
            fragment_at_or_before(video_stream.timeline(), request.timestamp)?;
        let audio_fragment_index =
            fragment_at_or_before(audio_stream.timeline(), request.timestamp)?;
        let video_fragment = video_stream
            .timeline()
            .fragment_at(video_fragment_index)
            .map_err(SmoothVodSeekError::Timeline)?;
        let actual_position = smooth_ticks_to_duration(
            video_fragment.start().ticks(),
            video_fragment.start().timescale().get(),
        );
        Ok(Self {
            video_fragment_index,
            audio_fragment_index,
            result: DemuxSeekResult {
                requested_position: MediaTime::from_duration(request.timestamp),
                actual_position: MediaTime::from_duration(actual_position),
                actual_track_timestamp: None,
            },
        })
    }
}

/// Binary-search-ит последний validated fragment start не позже target.
fn fragment_at_or_before(
    timeline: &SmoothChunkTimeline,
    target: Duration,
) -> Result<usize, SmoothVodSeekError> {
    let target_ticks = duration_to_ticks_floor(target, timeline.timescale().get())?;
    let mut lower = 0_usize;
    let mut upper = timeline.fragment_count();
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        let fragment = timeline
            .fragment_at(middle)
            .map_err(SmoothVodSeekError::Timeline)?;
        if fragment.start().ticks() <= target_ticks {
            lower = middle.saturating_add(1);
        } else {
            upper = middle;
        }
    }
    Ok(lower.saturating_sub(1))
}

/// Floor-конверсия runtime Duration в unsigned manifest ticks.
fn duration_to_ticks_floor(
    duration: Duration,
    timescale_ticks_per_second: u64,
) -> Result<u64, SmoothVodSeekError> {
    let total_nanoseconds = u128::from(duration.as_secs())
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(u128::from(duration.subsec_nanos())))
        .ok_or(SmoothVodSeekError::TargetTickOverflow)?;
    let ticks = total_nanoseconds
        .checked_mul(u128::from(timescale_ticks_per_second))
        .ok_or(SmoothVodSeekError::TargetTickOverflow)?
        / 1_000_000_000;
    u64::try_from(ticks).map_err(|_| SmoothVodSeekError::TargetTickOverflow)
}

/// Floor-конверсия validated manifest ticks в runtime Duration.
pub(super) fn smooth_ticks_to_duration(ticks: u64, timescale_ticks_per_second: u64) -> Duration {
    let timescale = timescale_ticks_per_second;
    let seconds = ticks / timescale;
    let remainder_ticks = ticks % timescale;
    let nanoseconds = u128::from(remainder_ticks) * 1_000_000_000 / u128::from(timescale);
    Duration::new(
        seconds,
        u32::try_from(nanoseconds).expect("fractional nanoseconds меньше одной секунды"),
    )
}
