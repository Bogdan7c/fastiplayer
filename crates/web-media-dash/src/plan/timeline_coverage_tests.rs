//! Тесты допуска static Period: priming-gap в начале и overshoot в конце.

use std::num::NonZeroUsize;
use std::time::Duration;

use dash_mpd_core::{DashSegmentTemplate, DashTemplateString, DashTimelineEntry};

use super::DashPlanError;
use super::timeline::{
    DashPeriodTimelineBound, DashTimelinePlanningIntent, PlannedTemplateTimeline,
    plan_template_timeline,
};

/// Форма ffmpeg audio timeline: timescale 48000, priming 10832, шаг 273408.
const AUDIO_TIMESCALE: u64 = 48_000;
const AUDIO_PRIMING: u64 = 10_832;
const AUDIO_SEGMENT: u64 = 273_408;

fn timeline_template(timescale: u64, entries: Vec<DashTimelineEntry>) -> DashSegmentTemplate {
    DashSegmentTemplate {
        timescale,
        start_number: 1,
        presentation_time_offset: 0,
        media: DashTemplateString::parse("segment-$Time$.m4s".to_owned())
            .expect("test template is valid"),
        initialization: None,
        duration: None,
        timeline: entries.into_boxed_slice(),
        availability_time_offset_nanoseconds: None,
        availability_time_complete: None,
    }
}

fn entry(start_time: Option<u64>, duration: u64, repeat: i64) -> DashTimelineEntry {
    DashTimelineEntry {
        start_time,
        duration,
        repeat,
    }
}

fn plan(
    timescale: u64,
    entries: Vec<DashTimelineEntry>,
    period_seconds: u64,
    intent: DashTimelinePlanningIntent,
) -> Result<PlannedTemplateTimeline, DashPlanError> {
    plan_template_timeline(
        &timeline_template(timescale, entries),
        DashPeriodTimelineBound::Finite(Duration::from_secs(period_seconds)),
        NonZeroUsize::new(64).expect("non-zero"),
        intent,
    )
}

const STATIC: DashTimelinePlanningIntent = DashTimelinePlanningIntent::StaticCompletePeriod;

/// Конец последнего запланированного сегмента.
fn planned_end(planned: &PlannedTemplateTimeline) -> Duration {
    let last = planned.segments.last().expect("non-empty timeline");
    last.presentation_start + last.duration
}

#[test]
fn leading_gap_within_one_segment_keeps_real_offset_and_period_duration() {
    // Period 10 с: timeline стартует на 1 с (gap 1 с <= сегмент 3 с) и кончается ровно на 10 с.
    let planned = plan(1, vec![entry(Some(1), 3, 2)], 10, STATIC)
        .expect("leading gap <= one segment must be accepted");

    assert_eq!(
        planned.segments[0].presentation_start,
        Duration::from_secs(1)
    );
    assert_eq!(planned.snapshot_duration, Duration::from_secs(10));
    assert_eq!(planned_end(&planned), Duration::from_secs(10));
}

#[test]
fn trailing_overshoot_is_clipped_to_period_end() {
    // 0..12 при Period 10 с: последний сегмент (8..12) выступает на 2 с.
    let planned = plan(1, vec![entry(Some(0), 4, 2)], 10, STATIC)
        .expect("overshoot < last segment must be accepted");

    assert_eq!(planned.snapshot_duration, Duration::from_secs(10));
    let last = planned.segments.last().expect("non-empty");
    assert_eq!(last.duration, Duration::from_secs(2));
    // Не последние сегменты не режутся.
    assert_eq!(planned.segments[0].duration, Duration::from_secs(4));
    // Raw time в URL остаётся нетронутым.
    assert_eq!(last.raw_start_time, 8);
}

#[test]
fn ffmpeg_audio_priming_timeline_matches_period_duration() {
    // Точная форма из ffmpeg isoff-live MPD: priming 10832, итог 5_771_856 против Period 5_760_000.
    let planned = plan(
        AUDIO_TIMESCALE,
        vec![
            entry(Some(AUDIO_PRIMING), AUDIO_SEGMENT, 19),
            entry(None, 273_408, 0),
            entry(None, 19_456, 0),
        ],
        120,
        STATIC,
    )
    .expect("ffmpeg audio timeline must be accepted");

    assert_eq!(
        planned.segments[0].presentation_start,
        Duration::from_nanos(225_666_667)
    );
    assert_eq!(planned.snapshot_duration, Duration::from_secs(120));
    assert_eq!(planned_end(&planned), Duration::from_secs(120));
}

#[test]
fn leading_gap_larger_than_one_segment_is_rejected() {
    let result = plan(1, vec![entry(Some(4), 3, 2)], 10, STATIC);
    assert!(matches!(result, Err(DashPlanError::IncompleteStaticPeriod)));
}

#[test]
fn timeline_ending_before_period_end_is_rejected() {
    let result = plan(1, vec![entry(Some(0), 3, 2)], 10, STATIC);
    assert!(matches!(result, Err(DashPlanError::IncompleteStaticPeriod)));
}

#[test]
fn trailing_segments_wholly_past_period_end_are_dropped_from_plan() {
    // 0..15 при Period 10 с: сегмент 10..15 целиком за Period (выступ 5 с == сегмент).
    let planned = plan(1, vec![entry(Some(0), 5, 2)], 10, STATIC)
        .expect("overshoot of one longest segment must be accepted");

    assert_eq!(planned.segments.len(), 2);
    assert_eq!(planned_end(&planned), Duration::from_secs(10));
    assert_eq!(planned.snapshot_duration, Duration::from_secs(10));
}

#[test]
fn overshoot_beyond_one_segment_is_rejected() {
    // 0..20 при Period 10 с: выступ 10 с > самого длинного сегмента 4 с.
    let result = plan(1, vec![entry(Some(0), 4, 4)], 10, STATIC);
    assert!(matches!(
        result,
        Err(DashPlanError::SegmentCrossesPeriodBoundary)
    ));
}

#[test]
fn timeline_starting_before_period_is_rejected() {
    let mut template = timeline_template(1, vec![entry(Some(98), 4, 2)]);
    template.presentation_time_offset = 100;
    let result = plan_template_timeline(
        &template,
        DashPeriodTimelineBound::Finite(Duration::from_secs(10)),
        NonZeroUsize::new(8).expect("non-zero"),
        STATIC,
    );
    assert!(matches!(
        result,
        Err(DashPlanError::SegmentCrossesPeriodBoundary)
    ));
}

#[test]
fn dynamic_snapshot_still_rejects_overshoot() {
    let result = plan(
        1,
        vec![entry(Some(0), 4, 2)],
        10,
        DashTimelinePlanningIntent::DynamicSnapshot,
    );
    assert!(matches!(
        result,
        Err(DashPlanError::SegmentCrossesPeriodBoundary)
    ));
}
