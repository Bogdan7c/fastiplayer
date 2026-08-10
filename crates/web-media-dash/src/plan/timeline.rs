//! Exact SegmentTemplate timeline owner для static S34 и dynamic S35.
//!
//! Здесь намеренно разделены две координаты:
//! - raw media time `T`, который без изменений подставляется в `$Time$`;
//! - presentation time `(T - PTO) / timescale`, который используется scheduler-ом.
//!
//! Referenced segment принимается только целиком внутри Period. Пересечение
//! границы пока требует sample-level clipping/decode-only preroll, которого нет
//! в neutral demux API, поэтому такой профиль явно отклоняется.

use std::num::NonZeroUsize;
use std::time::Duration;

use dash_mpd_core::{DashSegmentTemplate, DashTimelineEntry};

use super::DashPlanError;

/// Lifecycle intent делает static/dynamic различие явным на callsite.
#[derive(Clone, Copy)]
pub(super) enum DashTimelinePlanningIntent {
    /// VOD обязан непрерывно покрыть Period от начала до конца.
    StaticCompletePeriod,
    /// Live snapshot может удалить head и ещё не объявить tail.
    DynamicSnapshot,
}

/// Declared Period lifecycle не смешивается с operational snapshot horizon.
#[derive(Clone, Copy)]
pub(super) enum DashPeriodTimelineBound {
    /// Static либо завершённый Period имеет exact верхнюю границу.
    Finite(Duration),
    /// Последний dynamic Period продолжается за пределами текущего MPD snapshot-а.
    OpenEnded,
}

/// Один reference одновременно хранит raw URL time и presentation placement.
pub(super) struct PlannedTemplateSegment {
    /// `$Number$` template value.
    pub number: u64,
    /// Raw `$Time$` template value.
    pub raw_start_time: u64,
    /// Start относительно Period после единственного вычитания PTO.
    pub presentation_start: Duration,
    /// Duration между двумя одинаково quantized absolute boundaries.
    pub duration: Duration,
}

/// Полный результат timeline planning для URL и packet timestamp mapping.
pub(super) struct PlannedTemplateTimeline {
    /// Ordered contiguous references текущего MPD snapshot-а.
    pub segments: Vec<PlannedTemplateSegment>,
    /// PTO как exact neutral duration для component demuxer-а.
    pub media_time_origin: Duration,
    /// Конец последнего объявленного segment-а относительно Period start.
    pub snapshot_duration: Duration,
}

/// Строит checked timeline без floating-point и approximate clipping.
pub(super) fn plan_template_timeline(
    template: &DashSegmentTemplate,
    period_bound: DashPeriodTimelineBound,
    maximum_segments: NonZeroUsize,
    intent: DashTimelinePlanningIntent,
) -> Result<PlannedTemplateTimeline, DashPlanError> {
    let period_start_raw = template.presentation_time_offset;
    let period_duration_units = match period_bound {
        DashPeriodTimelineBound::Finite(period_duration) => {
            Some(duration_to_units(period_duration, template.timescale)?)
        }
        DashPeriodTimelineBound::OpenEnded => None,
    };
    let period_end_raw = period_duration_units
        .map(|duration_units| {
            period_start_raw
                .checked_add(duration_units)
                .ok_or(DashPlanError::TimelineOverflow)
        })
        .transpose()?;
    let raw_segments = match template.duration {
        Some(segment_duration) => {
            let finite_duration_units = period_duration_units
                .ok_or(DashPlanError::OpenEndedPeriodRequiresExplicitTimeline)?;
            plan_uniform_segments(
                template.start_number,
                period_start_raw,
                finite_duration_units,
                segment_duration,
                maximum_segments,
            )?
        }
        None => expand_explicit_timeline(
            &template.timeline,
            template.start_number,
            period_end_raw,
            maximum_segments,
        )?,
    };
    validate_raw_contiguity(&raw_segments)?;
    validate_period_bounds(&raw_segments, period_start_raw, period_end_raw, intent)?;
    let snapshot_end_raw = raw_segments
        .last()
        .and_then(|segment| segment.raw_start_time.checked_add(segment.duration))
        .ok_or(DashPlanError::TimelineOverflow)?;
    let snapshot_duration_units = snapshot_end_raw
        .checked_sub(period_start_raw)
        .ok_or(DashPlanError::SegmentCrossesPeriodBoundary)?;

    let segments = raw_segments
        .into_iter()
        .map(|segment| {
            let presentation_start_units = segment
                .raw_start_time
                .checked_sub(period_start_raw)
                .ok_or(DashPlanError::SegmentCrossesPeriodBoundary)?;
            let presentation_end_units = presentation_start_units
                .checked_add(segment.duration)
                .ok_or(DashPlanError::TimelineOverflow)?;
            let presentation_start =
                units_to_duration(presentation_start_units, template.timescale)?;
            let presentation_end = units_to_duration(presentation_end_units, template.timescale)?;
            let duration = presentation_end
                .checked_sub(presentation_start)
                .filter(|duration| !duration.is_zero())
                .ok_or(DashPlanError::TimelineOverflow)?;
            Ok(PlannedTemplateSegment {
                number: segment.number,
                raw_start_time: segment.raw_start_time,
                presentation_start,
                duration,
            })
        })
        .collect::<Result<Vec<_>, DashPlanError>>()?;
    Ok(PlannedTemplateTimeline {
        segments,
        media_time_origin: units_to_duration(period_start_raw, template.timescale)?,
        snapshot_duration: units_to_duration(snapshot_duration_units, template.timescale)?,
    })
}

/// Простой addressing начинает raw media timeline в PTO и не выдумывает clipped tail.
fn plan_uniform_segments(
    start_number: u64,
    period_start_raw: u64,
    period_duration_units: u64,
    segment_duration: u64,
    maximum_segments: NonZeroUsize,
) -> Result<Vec<RawTemplateSegment>, DashPlanError> {
    if segment_duration == 0 || !period_duration_units.is_multiple_of(segment_duration) {
        return Err(DashPlanError::IncompleteStaticPeriod);
    }
    let segment_count = period_duration_units / segment_duration;
    let segment_count =
        usize::try_from(segment_count).map_err(|_| DashPlanError::SegmentBoundExceeded)?;
    if segment_count == 0 || segment_count > maximum_segments.get() {
        return Err(DashPlanError::SegmentBoundExceeded);
    }
    (0..segment_count)
        .map(|index| {
            let index = u64::try_from(index).map_err(|_| DashPlanError::TimelineOverflow)?;
            Ok(RawTemplateSegment {
                number: start_number
                    .checked_add(index)
                    .ok_or(DashPlanError::TimelineOverflow)?,
                raw_start_time: period_start_raw
                    .checked_add(
                        index
                            .checked_mul(segment_duration)
                            .ok_or(DashPlanError::TimelineOverflow)?,
                    )
                    .ok_or(DashPlanError::TimelineOverflow)?,
                duration: segment_duration,
            })
        })
        .collect()
}

/// Раскрывает SegmentTimeline; `r=-1` требует следующую `S@t` либо finite Period end.
fn expand_explicit_timeline(
    entries: &[DashTimelineEntry],
    start_number: u64,
    period_end_raw: Option<u64>,
    maximum_segments: NonZeroUsize,
) -> Result<Vec<RawTemplateSegment>, DashPlanError> {
    let mut segments = Vec::new();
    let mut continuation_raw = 0_u64;
    for (entry_index, entry) in entries.iter().enumerate() {
        if entry.duration == 0 {
            return Err(DashPlanError::Template(
                dash_mpd_core::DashTemplateError::InvalidSyntax,
            ));
        }
        let entry_start_raw = entry.start_time.unwrap_or(continuation_raw);
        let segment_count =
            segment_count_for_entry(entries, entry_index, entry, entry_start_raw, period_end_raw)?;
        let segment_count_usize =
            usize::try_from(segment_count).map_err(|_| DashPlanError::SegmentBoundExceeded)?;
        if segments.len().saturating_add(segment_count_usize) > maximum_segments.get() {
            return Err(DashPlanError::SegmentBoundExceeded);
        }
        for repeat_index in 0..segment_count {
            let ordinal =
                u64::try_from(segments.len()).map_err(|_| DashPlanError::TimelineOverflow)?;
            let raw_start_time = entry_start_raw
                .checked_add(
                    entry
                        .duration
                        .checked_mul(repeat_index)
                        .ok_or(DashPlanError::TimelineOverflow)?,
                )
                .ok_or(DashPlanError::TimelineOverflow)?;
            segments.push(RawTemplateSegment {
                number: start_number
                    .checked_add(ordinal)
                    .ok_or(DashPlanError::TimelineOverflow)?,
                raw_start_time,
                duration: entry.duration,
            });
        }
        continuation_raw = entry_start_raw
            .checked_add(
                entry
                    .duration
                    .checked_mul(segment_count)
                    .ok_or(DashPlanError::TimelineOverflow)?,
            )
            .ok_or(DashPlanError::TimelineOverflow)?;
    }
    if segments.is_empty() {
        return Err(DashPlanError::InvalidSerializedLifecycle);
    }
    Ok(segments)
}

/// Возвращает число references, считая исходный `S` и все repetitions.
fn segment_count_for_entry(
    entries: &[DashTimelineEntry],
    entry_index: usize,
    entry: &DashTimelineEntry,
    entry_start_raw: u64,
    period_end_raw: Option<u64>,
) -> Result<u64, DashPlanError> {
    if entry.repeat >= 0 {
        return u64::try_from(entry.repeat)
            .ok()
            .and_then(|repeat| repeat.checked_add(1))
            .ok_or(DashPlanError::TimelineOverflow);
    }
    if entry.repeat != -1 {
        return Err(DashPlanError::Template(
            dash_mpd_core::DashTemplateError::InvalidSyntax,
        ));
    }
    let boundary_raw = entries
        .get(entry_index + 1)
        .and_then(|next| next.start_time)
        .or(period_end_raw)
        .ok_or(DashPlanError::OpenEndedTimelineRepeat)?;
    let span = boundary_raw
        .checked_sub(entry_start_raw)
        .ok_or(DashPlanError::TimelineOverlap)?;
    if span == 0 {
        return Err(DashPlanError::TimelineOverlap);
    }
    span.checked_add(entry.duration.saturating_sub(1))
        .and_then(|rounded| rounded.checked_div(entry.duration))
        .filter(|count| *count > 0)
        .ok_or(DashPlanError::TimelineOverflow)
}

/// Raw references внутри Representation обязаны соприкасаться точно.
fn validate_raw_contiguity(segments: &[RawTemplateSegment]) -> Result<(), DashPlanError> {
    for pair in segments.windows(2) {
        let previous_end = pair[0]
            .raw_start_time
            .checked_add(pair[0].duration)
            .ok_or(DashPlanError::TimelineOverflow)?;
        match pair[1].raw_start_time.cmp(&previous_end) {
            std::cmp::Ordering::Less => return Err(DashPlanError::TimelineOverlap),
            std::cmp::Ordering::Greater => return Err(DashPlanError::TimelineGap),
            std::cmp::Ordering::Equal => {}
        }
    }
    Ok(())
}

/// Dynamic разрешает sliding head/tail, но не boundary-crossing reference.
fn validate_period_bounds(
    segments: &[RawTemplateSegment],
    period_start_raw: u64,
    period_end_raw: Option<u64>,
    intent: DashTimelinePlanningIntent,
) -> Result<(), DashPlanError> {
    let first_start = segments
        .first()
        .map(|segment| segment.raw_start_time)
        .ok_or(DashPlanError::InvalidSerializedLifecycle)?;
    let last_end = segments
        .last()
        .and_then(|segment| segment.raw_start_time.checked_add(segment.duration))
        .ok_or(DashPlanError::TimelineOverflow)?;
    if first_start < period_start_raw
        || period_end_raw.is_some_and(|period_end| last_end > period_end)
    {
        return Err(DashPlanError::SegmentCrossesPeriodBoundary);
    }
    if matches!(intent, DashTimelinePlanningIntent::StaticCompletePeriod) {
        let period_end_raw =
            period_end_raw.ok_or(DashPlanError::OpenEndedPeriodRequiresExplicitTimeline)?;
        if first_start != period_start_raw || last_end != period_end_raw {
            return Err(DashPlanError::IncompleteStaticPeriod);
        }
    }
    Ok(())
}

/// Внутренний raw reference до преобразования в presentation time.
struct RawTemplateSegment {
    number: u64,
    raw_start_time: u64,
    duration: u64,
}

/// Требует exact integral duration в addressing timescale.
pub(super) fn duration_to_units(duration: Duration, timescale: u64) -> Result<u64, DashPlanError> {
    let nanoseconds = duration.as_nanos();
    let scaled = nanoseconds
        .checked_mul(u128::from(timescale))
        .ok_or(DashPlanError::TimelineOverflow)?;
    if scaled % 1_000_000_000 != 0 {
        return Err(DashPlanError::NonIntegralPeriodTimescale);
    }
    u64::try_from(scaled / 1_000_000_000).map_err(|_| DashPlanError::TimelineOverflow)
}

/// Квантует absolute tick boundary в ближайшую наносекунду, ties округляя вверх.
///
/// Segment duration нельзя квантувать отдельно: caller обязан вычесть две
/// boundary, полученные этим же правилом, чтобы не накопить drift/gap.
pub(super) fn units_to_duration(units: u64, timescale: u64) -> Result<Duration, DashPlanError> {
    let nanoseconds = u128::from(units)
        .checked_mul(1_000_000_000)
        .ok_or(DashPlanError::TimelineOverflow)?;
    let rounding_offset = u128::from(timescale / 2);
    let rounded_nanoseconds = nanoseconds
        .checked_add(rounding_offset)
        .ok_or(DashPlanError::TimelineOverflow)?
        / u128::from(timescale);
    let nanoseconds =
        u64::try_from(rounded_nanoseconds).map_err(|_| DashPlanError::TimelineOverflow)?;
    Ok(Duration::from_nanos(nanoseconds))
}

#[cfg(test)]
mod tests {
    use dash_mpd_core::{DashSegmentTemplate, DashTemplateString, DashTimelineEntry};

    use super::{
        DashPeriodTimelineBound, DashPlanError, DashTimelinePlanningIntent, plan_template_timeline,
    };

    /// Собирает explicit template без XML/parser шума.
    fn template(
        presentation_time_offset: u64,
        timeline: Vec<DashTimelineEntry>,
    ) -> DashSegmentTemplate {
        DashSegmentTemplate {
            timescale: 1,
            start_number: 7,
            presentation_time_offset,
            media: DashTemplateString::parse("segment-$Time$.m4s".to_owned())
                .expect("test template is valid"),
            initialization: None,
            duration: None,
            timeline: timeline.into_boxed_slice(),
            availability_time_offset_nanoseconds: None,
            availability_time_complete: None,
        }
    }

    #[test]
    fn nonzero_pto_keeps_raw_time_and_subtracts_origin_once() {
        let planned = plan_template_timeline(
            &template(
                100,
                vec![DashTimelineEntry {
                    start_time: Some(100),
                    duration: 5,
                    repeat: 1,
                }],
            ),
            DashPeriodTimelineBound::Finite(std::time::Duration::from_secs(10)),
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::StaticCompletePeriod,
        )
        .expect("fully-contained PTO timeline must plan");

        assert_eq!(
            planned.media_time_origin,
            std::time::Duration::from_secs(100)
        );
        assert_eq!(planned.segments[0].raw_start_time, 100);
        assert_eq!(
            planned.segments[0].presentation_start,
            std::time::Duration::ZERO
        );
        assert_eq!(planned.segments[1].raw_start_time, 105);
        assert_eq!(
            planned.segments[1].presentation_start,
            std::time::Duration::from_secs(5)
        );
    }

    #[test]
    fn r_minus_one_uses_pto_plus_period_duration_as_raw_boundary() {
        let planned = plan_template_timeline(
            &template(
                100,
                vec![DashTimelineEntry {
                    start_time: Some(100),
                    duration: 2,
                    repeat: -1,
                }],
            ),
            DashPeriodTimelineBound::Finite(std::time::Duration::from_secs(6)),
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::StaticCompletePeriod,
        )
        .expect("raw PTO boundary must bound repeat");

        assert_eq!(
            planned
                .segments
                .iter()
                .map(|segment| segment.raw_start_time)
                .collect::<Vec<_>>(),
            vec![100, 102, 104]
        );
    }

    #[test]
    fn dynamic_snapshot_allows_exact_sliding_head_and_tail() {
        let planned = plan_template_timeline(
            &template(
                100,
                vec![DashTimelineEntry {
                    start_time: Some(104),
                    duration: 2,
                    repeat: 1,
                }],
            ),
            DashPeriodTimelineBound::Finite(std::time::Duration::from_secs(10)),
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        )
        .expect("dynamic snapshot may expose a strict subset");

        assert_eq!(
            planned.segments[0].presentation_start,
            std::time::Duration::from_secs(4)
        );
        assert_eq!(planned.segments.len(), 2);
    }

    #[test]
    fn gaps_and_overlaps_fail_with_distinct_typed_errors() {
        let gap = plan_template_timeline(
            &template(
                0,
                vec![
                    DashTimelineEntry {
                        start_time: Some(0),
                        duration: 2,
                        repeat: 0,
                    },
                    DashTimelineEntry {
                        start_time: Some(3),
                        duration: 2,
                        repeat: 0,
                    },
                ],
            ),
            DashPeriodTimelineBound::Finite(std::time::Duration::from_secs(5)),
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        );
        let overlap = plan_template_timeline(
            &template(
                0,
                vec![
                    DashTimelineEntry {
                        start_time: Some(0),
                        duration: 3,
                        repeat: 0,
                    },
                    DashTimelineEntry {
                        start_time: Some(2),
                        duration: 2,
                        repeat: 0,
                    },
                ],
            ),
            DashPeriodTimelineBound::Finite(std::time::Duration::from_secs(5)),
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        );

        assert!(matches!(gap, Err(DashPlanError::TimelineGap)));
        assert!(matches!(overlap, Err(DashPlanError::TimelineOverlap)));
    }

    #[test]
    fn period_boundary_crossing_is_never_clipped() {
        let before_start = plan_template_timeline(
            &template(
                100,
                vec![DashTimelineEntry {
                    start_time: Some(98),
                    duration: 4,
                    repeat: 0,
                }],
            ),
            DashPeriodTimelineBound::Finite(std::time::Duration::from_secs(10)),
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        );
        let after_end = plan_template_timeline(
            &template(
                100,
                vec![DashTimelineEntry {
                    start_time: Some(108),
                    duration: 4,
                    repeat: 0,
                }],
            ),
            DashPeriodTimelineBound::Finite(std::time::Duration::from_secs(10)),
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        );

        assert!(matches!(
            before_start,
            Err(DashPlanError::SegmentCrossesPeriodBoundary)
        ));
        assert!(matches!(
            after_end,
            Err(DashPlanError::SegmentCrossesPeriodBoundary)
        ));
    }

    #[test]
    fn open_period_uses_explicit_snapshot_end_without_inventing_declared_duration() {
        let planned = plan_template_timeline(
            &template(
                0,
                vec![DashTimelineEntry {
                    start_time: Some(10),
                    duration: 2,
                    repeat: 2,
                }],
            ),
            DashPeriodTimelineBound::OpenEnded,
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        )
        .expect("bounded explicit snapshot должен планироваться для open Period");

        assert_eq!(planned.segments.len(), 3);
        assert_eq!(
            planned.snapshot_duration,
            std::time::Duration::from_secs(16)
        );
    }

    #[test]
    fn sample_clock_boundaries_quantize_without_segment_drift_or_gap() {
        let mut audio_template = template(
            0,
            vec![
                DashTimelineEntry {
                    start_time: Some(0),
                    duration: 96_256,
                    repeat: 2,
                },
                DashTimelineEntry {
                    start_time: None,
                    duration: 95_232,
                    repeat: 0,
                },
            ],
        );
        audio_template.timescale = 48_000;
        let planned = plan_template_timeline(
            &audio_template,
            DashPeriodTimelineBound::OpenEnded,
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        )
        .expect("AAC sample-clock timeline должен детерминированно quantize-иться");

        for adjacent_segments in planned.segments.windows(2) {
            assert_eq!(
                adjacent_segments[0]
                    .presentation_start
                    .checked_add(adjacent_segments[0].duration)
                    .expect("test timeline end"),
                adjacent_segments[1].presentation_start
            );
        }
        let last_segment = planned.segments.last().expect("non-empty timeline");
        assert_eq!(
            last_segment
                .presentation_start
                .checked_add(last_segment.duration)
                .expect("test timeline end"),
            planned.snapshot_duration
        );
    }

    #[test]
    fn unbounded_repeat_in_open_period_is_a_typed_exclusion() {
        let result = plan_template_timeline(
            &template(
                0,
                vec![DashTimelineEntry {
                    start_time: Some(10),
                    duration: 2,
                    repeat: -1,
                }],
            ),
            DashPeriodTimelineBound::OpenEnded,
            std::num::NonZeroUsize::new(8).expect("non-zero"),
            DashTimelinePlanningIntent::DynamicSnapshot,
        );

        assert!(matches!(
            result,
            Err(DashPlanError::OpenEndedTimelineRepeat)
        ));
    }
}
