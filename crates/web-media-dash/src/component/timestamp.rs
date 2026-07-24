//! Private преобразование timestamp-ов между media, Period и global presentation timelines.

use std::time::Duration;

use anyhow::Result;
use media_core::{DemuxSeekResult, MediaTime};

use crate::plan::{DashComponentPeriodPlan, DashPeriodInputPlan, DashTimestampMapping};
use crate::request::DashSerializedFragmentKind;

/// Выбирает единое timestamp-преобразование для open, seek и Period transition.
pub(super) fn timestamp_mapping_for_open(
    period: &DashComponentPeriodPlan,
    first_media_index: usize,
) -> Result<(Duration, Option<Duration>)> {
    match period.timestamp_mapping {
        DashTimestampMapping::NormalizeAtFirstPacket => {
            let resource_offset = ordered_resource_start(period, first_media_index)?;
            let timeline_start = period
                .timeline_start
                .checked_add(resource_offset)
                .ok_or_else(|| anyhow::anyhow!("DASH Period/resource timestamp overflow"))?;
            Ok((timeline_start, None))
        }
        DashTimestampMapping::MediaTimeOrigin(media_time_origin) => {
            Ok((period.timeline_start, Some(media_time_origin)))
        }
    }
}

/// Вычитает media origin ровно один раз и затем добавляет global Period start.
pub(super) fn globalize_packet_timestamp(
    timestamp: Duration,
    media_time_origin: Duration,
    period_timeline_start: Duration,
    timestamp_name: &'static str,
) -> Result<Duration> {
    timestamp
        .checked_sub(media_time_origin)
        .ok_or_else(|| {
            anyhow::anyhow!("DASH packet {timestamp_name} предшествует proven media origin")
        })?
        .checked_add(period_timeline_start)
        .ok_or_else(|| anyhow::anyhow!("DASH global packet {timestamp_name} overflow"))
}

/// Переводит inner SegmentBase seek result в global presentation time.
pub(super) fn globalize_seek_result(
    inner: DemuxSeekResult,
    requested: Duration,
    period_start: Duration,
) -> Result<DemuxSeekResult> {
    let actual = inner
        .actual_position
        .as_duration()
        .checked_add(period_start)
        .ok_or_else(|| anyhow::anyhow!("DASH seek result timestamp overflow"))?;
    Ok(DemuxSeekResult {
        requested_position: MediaTime::from_duration(requested),
        actual_position: MediaTime::from_duration(actual),
        actual_track_timestamp: inner.actual_track_timestamp,
    })
}

/// Возвращает selected ordered resource start либо zero для Range/initial open.
fn ordered_resource_start(
    period: &DashComponentPeriodPlan,
    first_media_index: usize,
) -> Result<Duration> {
    match &period.input {
        DashPeriodInputPlan::Ordered { resources, .. } => resources
            .iter()
            .filter(|resource| resource.kind == DashSerializedFragmentKind::Media)
            .nth(first_media_index)
            .and_then(|resource| resource.timeline_start)
            .ok_or_else(|| anyhow::anyhow!("DASH media fragment index отсутствует")),
        DashPeriodInputPlan::Range { .. } => Ok(Duration::ZERO),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::globalize_packet_timestamp;

    #[test]
    fn nonzero_pto_is_subtracted_once_for_pts_and_dts_across_periods() {
        let first_period_pts = globalize_packet_timestamp(
            Duration::from_secs(105),
            Duration::from_secs(100),
            Duration::ZERO,
            "PTS",
        )
        .expect("first Period timestamp must map");
        let second_period_dts = globalize_packet_timestamp(
            Duration::from_secs(205),
            Duration::from_secs(200),
            Duration::from_secs(30),
            "DTS",
        )
        .expect("second Period timestamp must map");

        assert_eq!(first_period_pts, Duration::from_secs(5));
        assert_eq!(second_period_dts, Duration::from_secs(35));
    }

    #[test]
    fn timestamp_before_pto_fails_instead_of_saturating_to_period_start() {
        assert!(
            globalize_packet_timestamp(
                Duration::from_secs(99),
                Duration::from_secs(100),
                Duration::ZERO,
                "PTS",
            )
            .is_err()
        );
    }
}
