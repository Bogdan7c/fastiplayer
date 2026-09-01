//! Resource, BaseURL, byte-range и serialized alignment construction.

use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use dash_mpd_core::{
    DashBaseUrl, DashInitialization, DashRepresentation, DashSegmentBase, DashSegmentList,
    DashSegmentTemplate, DashTemplateContext, IndexRange,
};
use source_core::{HttpBoundedByteRange, HttpRequestTarget};

use crate::request::{DashSerializedComponent, DashSerializedFragmentKind};

use super::lifecycle::validate_period_alignment;
use super::timeline::{
    DashPeriodTimelineBound, DashTimelinePlanningIntent, plan_template_timeline, units_to_duration,
};
use super::{
    DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPeriodLifecycle,
    DashPlanError, DashPlannedResource, DashTimestampMapping,
};

/// Раскрывает finite SegmentTemplate duration/Timeline.
pub(super) fn plan_template(
    representation: &DashRepresentation,
    template: &DashSegmentTemplate,
    base: &HttpRequestTarget,
    period_bound: DashPeriodTimelineBound,
    maximum_segments: NonZeroUsize,
    intent: DashTimelinePlanningIntent,
) -> Result<(Vec<DashPlannedResource>, Duration, Duration), DashPlanError> {
    let timeline = plan_template_timeline(template, period_bound, maximum_segments, intent)?;
    let snapshot_duration = timeline.snapshot_duration;
    let points = timeline.segments;
    let first_point = points
        .first()
        .ok_or(DashPlanError::InvalidSerializedLifecycle)?;
    let mut resources = Vec::with_capacity(points.len().saturating_add(1));
    if let Some(initialization) = &template.initialization {
        let reference = initialization.expand(DashTemplateContext {
            representation_id: &representation.id,
            bandwidth: representation.bandwidth,
            number: first_point.number,
            time: first_point.raw_start_time,
        })?;
        resources.push(DashPlannedResource {
            kind: DashSerializedFragmentKind::Initialization,
            target: resolve_reference(base, &reference)?,
            byte_range: None,
            timeline_start: None,
            duration: None,
        });
    }
    for point in points {
        let reference = template.media.expand(DashTemplateContext {
            representation_id: &representation.id,
            bandwidth: representation.bandwidth,
            number: point.number,
            time: point.raw_start_time,
        })?;
        resources.push(DashPlannedResource {
            kind: DashSerializedFragmentKind::Media,
            target: resolve_reference(base, &reference)?,
            byte_range: None,
            timeline_start: Some(point.presentation_start),
            duration: Some(point.duration),
        });
    }
    Ok((resources, timeline.media_time_origin, snapshot_duration))
}

/// Строит explicit SegmentList resources в manifest order.
pub(super) fn plan_list(
    list: &DashSegmentList,
    base: &HttpRequestTarget,
    period_duration: Duration,
    maximum_segments: NonZeroUsize,
) -> Result<Vec<DashPlannedResource>, DashPlanError> {
    if list.segments.len() > maximum_segments.get() {
        return Err(DashPlanError::SegmentBoundExceeded);
    }
    let segment_duration = list
        .duration
        .ok_or(DashPlanError::MissingSegmentListDuration)?;
    let mut resources = Vec::with_capacity(list.segments.len().saturating_add(1));
    if let Some(initialization) = &list.initialization {
        resources.push(plan_initialization(initialization, base)?);
    }
    for (index, segment) in list.segments.iter().enumerate() {
        if segment.index.is_some() || segment.index_range.is_some() {
            return Err(DashPlanError::ExternalSegmentIndexUnsupported);
        }
        let index = u64::try_from(index).map_err(|_| DashPlanError::TimelineOverflow)?;
        let start_units = index
            .checked_mul(segment_duration)
            .ok_or(DashPlanError::TimelineOverflow)?;
        let end_units = start_units
            .checked_add(segment_duration)
            .ok_or(DashPlanError::TimelineOverflow)?;
        let timeline_start = units_to_duration(start_units, list.timescale)?;
        let timeline_end = units_to_duration(end_units, list.timescale)?;
        let duration = timeline_end
            .checked_sub(timeline_start)
            .filter(|duration| !duration.is_zero())
            .ok_or(DashPlanError::TimelineOverflow)?;
        resources.push(DashPlannedResource {
            kind: DashSerializedFragmentKind::Media,
            target: resolve_reference(base, segment.media.as_str())?,
            byte_range: segment
                .media_range
                .map(index_range_to_bounded)
                .transpose()?,
            timeline_start: Some(timeline_start),
            duration: Some(duration),
        });
    }
    let planned_duration = units_to_duration(
        u64::try_from(list.segments.len())
            .map_err(|_| DashPlanError::TimelineOverflow)?
            .checked_mul(segment_duration)
            .ok_or(DashPlanError::TimelineOverflow)?,
        list.timescale,
    )?;
    if planned_duration != period_duration {
        return Err(DashPlanError::ComponentAlignmentMismatch);
    }
    Ok(resources)
}

/// Строит initialization target/range.
fn plan_initialization(
    initialization: &DashInitialization,
    base: &HttpRequestTarget,
) -> Result<DashPlannedResource, DashPlanError> {
    let target = initialization.source_url.as_ref().map_or_else(
        || Ok(base.clone()),
        |reference| resolve_reference(base, reference.as_str()),
    )?;
    Ok(DashPlannedResource {
        kind: DashSerializedFragmentKind::Initialization,
        target,
        byte_range: initialization
            .byte_range
            .map(index_range_to_bounded)
            .transpose()?,
        timeline_start: None,
        duration: None,
    })
}

/// SegmentBase разрешает только init range внутри того же representation resource.
pub(super) fn validate_segment_base(segment_base: &DashSegmentBase) -> Result<(), DashPlanError> {
    if segment_base
        .initialization
        .as_ref()
        .and_then(|initialization| initialization.source_url.as_ref())
        .is_some()
    {
        return Err(DashPlanError::ExternalSegmentBaseInitializationUnsupported);
    }
    Ok(())
}

/// Возвращает zero-based initialization prefix, пригодный только для catalog demux proof.
pub(super) fn segment_base_catalog_probe_content_length(
    segment_base: &DashSegmentBase,
) -> Option<NonZeroU64> {
    let initialization_range = segment_base
        .initialization
        .as_ref()
        .and_then(|initialization| initialization.byte_range)?;
    if initialization_range.start() != 0 {
        return None;
    }
    initialization_range
        .end()
        .checked_add(1)
        .and_then(NonZeroU64::new)
}

/// Строит single-period component из concrete fragments.
pub(super) fn build_serialized_component(
    component: &DashSerializedComponent,
    maximum_segments: NonZeroUsize,
) -> Result<DashComponentPlan, DashPlanError> {
    let media_count = component
        .fragments
        .iter()
        .filter(|fragment| fragment.kind == DashSerializedFragmentKind::Media)
        .count();
    if media_count == 0 || media_count > maximum_segments.get() {
        return Err(DashPlanError::SegmentBoundExceeded);
    }
    let mut saw_initialization = false;
    let mut saw_media = false;
    let mut timeline_start = Duration::ZERO;
    let mut resources = Vec::with_capacity(component.fragments.len());
    for fragment in &component.fragments {
        match fragment.kind {
            DashSerializedFragmentKind::Initialization
                if !saw_initialization && !saw_media && fragment.duration.is_none() =>
            {
                saw_initialization = true;
                resources.push(DashPlannedResource {
                    kind: fragment.kind,
                    target: fragment
                        .target
                        .resolve()
                        .map_err(|_| DashPlanError::Target)?,
                    byte_range: fragment.byte_range,
                    timeline_start: None,
                    duration: None,
                });
            }
            DashSerializedFragmentKind::Media => {
                saw_media = true;
                let duration = fragment
                    .duration
                    .filter(|duration| !duration.is_zero())
                    .ok_or(DashPlanError::InvalidSerializedDuration)?;
                resources.push(DashPlannedResource {
                    kind: fragment.kind,
                    target: fragment
                        .target
                        .resolve()
                        .map_err(|_| DashPlanError::Target)?,
                    byte_range: fragment.byte_range,
                    timeline_start: Some(timeline_start),
                    duration: Some(duration),
                });
                timeline_start = timeline_start
                    .checked_add(duration)
                    .ok_or(DashPlanError::TimelineOverflow)?;
            }
            DashSerializedFragmentKind::Initialization => {
                return Err(DashPlanError::InvalidSerializedLifecycle);
            }
        }
    }
    if !saw_initialization || !saw_media {
        return Err(DashPlanError::InvalidSerializedLifecycle);
    }
    Ok(DashComponentPlan {
        media_kind: component.media_kind,
        periods: vec![DashComponentPeriodPlan {
            container: component.container,
            timeline_start: Duration::ZERO,
            declared_lifecycle: DashPeriodLifecycle::Finite(timeline_start),
            duration: timeline_start,
            timestamp_mapping: DashTimestampMapping::NormalizeAtFirstPacket,
            input: DashPeriodInputPlan::Ordered {
                resources,
                query_application: component.query_application,
            },
        }],
        duration: timeline_start,
    })
}

/// Проверяет count/start/duration каждого serialized media fragment-а.
pub(super) fn validate_resource_alignment(
    video: &DashComponentPlan,
    audio: &DashComponentPlan,
) -> Result<(), DashPlanError> {
    validate_period_alignment(video, audio)?;
    let video_resources = ordered_media_resources(&video.periods[0]);
    let audio_resources = ordered_media_resources(&audio.periods[0]);
    if video_resources.len() != audio_resources.len()
        || video_resources
            .iter()
            .zip(audio_resources)
            .any(|(video, audio)| {
                video.timeline_start != audio.timeline_start || video.duration != audio.duration
            })
    {
        return Err(DashPlanError::ComponentAlignmentMismatch);
    }
    Ok(())
}

/// Возвращает media resources ordered period-а без изменения manifest order.
fn ordered_media_resources(period: &DashComponentPeriodPlan) -> Vec<&DashPlannedResource> {
    match &period.input {
        DashPeriodInputPlan::Ordered { resources, .. } => resources
            .iter()
            .filter(|resource| resource.kind == DashSerializedFragmentKind::Media)
            .collect(),
        DashPeriodInputPlan::Range { .. } => Vec::new(),
    }
}

/// Применяет optional BaseURL одного lexical уровня.
pub(super) fn resolve_optional_base(
    base: &HttpRequestTarget,
    nested: Option<&DashBaseUrl>,
) -> Result<HttpRequestTarget, DashPlanError> {
    nested.map_or_else(
        || Ok(base.clone()),
        |nested| resolve_reference(base, nested.reference().as_str()),
    )
}

/// Разрешает reference через единственный source-core WHATWG boundary.
fn resolve_reference(
    base: &HttpRequestTarget,
    reference: &str,
) -> Result<HttpRequestTarget, DashPlanError> {
    base.resolve_reference(reference)
        .map_err(|_| DashPlanError::Target)
}

/// Переводит inclusive MPD range в `(start,length)` с checked arithmetic.
fn index_range_to_bounded(range: IndexRange) -> Result<HttpBoundedByteRange, DashPlanError> {
    let length = range
        .end()
        .checked_sub(range.start())
        .and_then(|difference| difference.checked_add(1))
        .and_then(|length| usize::try_from(length).ok())
        .and_then(NonZeroUsize::new)
        .ok_or(DashPlanError::InvalidByteRange)?;
    HttpBoundedByteRange::new(range.start(), length).map_err(|_| DashPlanError::InvalidByteRange)
}
