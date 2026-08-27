use std::time::Duration;

use video_core::DecodePacket;

use crate::ffi::frame::FrameTimestamps;

use super::NO_TIMESTAMP;

/// PTS resolver policy: best_effort -> pts -> interpolation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FramePtsResolver {
    /// Last known stream time base.
    time_base: Option<StreamTimeBase>,

    /// Next timestamp predicted from the previous frame duration.
    next_interpolated_pts: Option<Duration>,

    /// Last non-zero frame duration seen in frame metadata.
    last_frame_duration: Option<Duration>,
}

impl FramePtsResolver {
    pub(super) fn observe_accepted_packet(&mut self, packet: &DecodePacket) {
        if let Some(packet_time_base) = packet_track_time_base(packet) {
            self.time_base = Some(packet_time_base);
        }

        if self.next_interpolated_pts.is_none() {
            self.next_interpolated_pts = Some(packet.pts);
        }
    }

    pub(super) fn resolve_frame_pts(
        &mut self,
        timestamps: FrameTimestamps,
        packet_pts_seed: Option<Duration>,
    ) -> Duration {
        let explicit_pts = self
            .timestamp_units_to_duration(timestamps.best_effort_timestamp)
            .or_else(|| self.timestamp_units_to_duration(timestamps.pts));
        let frame_duration = self.frame_duration(timestamps.duration);
        let resolved_pts = explicit_pts
            .or(self.next_interpolated_pts)
            .or(packet_pts_seed)
            .unwrap_or(Duration::ZERO);

        if let Some(frame_duration) = frame_duration {
            self.last_frame_duration = Some(frame_duration);
        }

        if let Some(duration_step) = frame_duration.or(self.last_frame_duration) {
            self.next_interpolated_pts = Some(resolved_pts.saturating_add(duration_step));
        } else {
            self.next_interpolated_pts = Some(resolved_pts);
        }

        resolved_pts
    }

    fn timestamp_units_to_duration(self, units: i64) -> Option<Duration> {
        if units == NO_TIMESTAMP {
            return None;
        }

        let time_base = self.time_base?;

        Some(units_to_duration_saturating(units, time_base))
    }

    fn frame_duration(self, units: i64) -> Option<Duration> {
        if units <= 0 {
            return None;
        }

        let time_base = self.time_base?;

        Some(units_to_duration_saturating(units, time_base))
    }
}

/// Compact copy of media-core time base fields, avoiding a new public dependency leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StreamTimeBase {
    /// Numerator from container track time base.
    pub(super) numer: u32,

    /// Denominator from container track time base.
    pub(super) denom: u32,
}

impl StreamTimeBase {
    fn new(numer: u32, denom: u32) -> Self {
        Self { numer, denom }
    }
}

/// Выбирает container time base из raw PTS, а при его отсутствии — из raw DTS.
pub(super) fn packet_track_time_base(packet: &DecodePacket) -> Option<StreamTimeBase> {
    packet
        .track_pts
        .filter(|track_pts| track_pts.track_id == packet.track_id)
        .or_else(|| {
            packet
                .track_dts
                .filter(|track_dts| track_dts.track_id == packet.track_id)
        })
        .map(|track_timestamp| {
            StreamTimeBase::new(
                track_timestamp.time_base.numer,
                track_timestamp.time_base.denom,
            )
        })
}

/// Возвращает raw PTS units только для согласованных track owner и time base.
#[cfg(feature = "ffmpeg")]
pub(super) fn packet_track_pts_units(
    packet: &DecodePacket,
    packet_time_base: StreamTimeBase,
) -> Option<i64> {
    packet
        .track_pts
        .filter(|timestamp| timestamp.track_id == packet.track_id)
        .filter(|timestamp| {
            timestamp.time_base.numer == packet_time_base.numer
                && timestamp.time_base.denom == packet_time_base.denom
        })
        .map(|timestamp| timestamp.units.get())
}

/// Возвращает raw DTS units только для согласованных track owner и time base.
#[cfg(feature = "ffmpeg")]
pub(super) fn packet_track_dts_units(
    packet: &DecodePacket,
    packet_time_base: StreamTimeBase,
) -> Option<i64> {
    packet
        .track_dts
        .filter(|timestamp| timestamp.track_id == packet.track_id)
        .filter(|timestamp| {
            timestamp.time_base.numer == packet_time_base.numer
                && timestamp.time_base.denom == packet_time_base.denom
        })
        .map(|timestamp| timestamp.units.get())
}

fn units_to_duration_saturating(units: i64, time_base: StreamTimeBase) -> Duration {
    if units <= 0 || time_base.denom == 0 {
        return Duration::ZERO;
    }

    let total_nanoseconds = (units as u128)
        .saturating_mul(u128::from(time_base.numer))
        .saturating_mul(1_000_000_000)
        / u128::from(time_base.denom);
    let clamped_nanoseconds = total_nanoseconds.min(u128::from(u64::MAX));

    Duration::from_nanos(clamped_nanoseconds as u64)
}

#[cfg(feature = "ffmpeg")]
pub(super) fn duration_to_units_saturating(duration: Duration, time_base: StreamTimeBase) -> i64 {
    if time_base.numer == 0 {
        return 0;
    }

    let duration_nanoseconds = u128::from(duration.as_secs())
        .saturating_mul(1_000_000_000)
        .saturating_add(u128::from(duration.subsec_nanos()));
    let units = duration_nanoseconds.saturating_mul(u128::from(time_base.denom))
        / u128::from(time_base.numer)
        / 1_000_000_000;

    if units > i64::MAX as u128 {
        i64::MAX
    } else {
        units as i64
    }
}
