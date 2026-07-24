//! Pure finite DASH planning: BaseURL, addressing, clocks и component alignment.

use std::num::NonZeroUsize;
use std::time::Duration;

use dash_mpd_core::{
    DashAddressing, DashBaseUrl, DashContainer, DashInitialization, DashMediaKind, DashMpd,
    DashPeriod, DashRepresentation, DashSegmentBase, DashSegmentList, DashSegmentTemplate,
    DashTemplateContext, DashTemplateError, IndexRange,
};
use source_core::{HttpBoundedByteRange, HttpRequestTarget};
use thiserror::Error;
use web_media_adaptive::AdaptiveResourceQueryApplication;

use crate::request::{
    DashSerializedComponent, DashSerializedFragmentKind, DashSerializedPresentation,
};
use crate::selection::{
    DashPresentationSelection, DashRepresentationSelectionError, SelectedDashRepresentation,
    select_representation,
};

mod timeline;
use timeline::{DashTimelinePlanningIntent, plan_template_timeline, units_to_duration};

/// Один ordered init/media resource и его explicit presentation interval.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DashPlannedResource {
    /// Role для existing ordered demux boundary.
    pub kind: DashSerializedFragmentKind,
    /// Secret-safe validated target.
    pub target: HttpRequestTarget,
    /// Optional exact Range.
    pub byte_range: Option<HttpBoundedByteRange>,
    /// Global component-relative start для media fragment-а.
    pub timeline_start: Option<Duration>,
    /// Media fragment duration.
    pub duration: Option<Duration>,
}

/// Input одного Period.
#[derive(Clone)]
pub(crate) enum DashPeriodInputPlan {
    /// Init + finite ordered media resources.
    Ordered {
        /// Exact resources.
        resources: Vec<DashPlannedResource>,
        /// Query projection для resource requests.
        query_application: AdaptiveResourceQueryApplication,
    },
    /// Single seekable HTTP representation; existing container owner читает sidx.
    Range {
        /// Effective representation resource.
        target: HttpRequestTarget,
        /// Query projection для every Range read.
        query_application: AdaptiveResourceQueryApplication,
    },
}

/// Один component Period с global timeline placement.
#[derive(Clone)]
pub(crate) struct DashComponentPeriodPlan {
    /// Proven container.
    pub container: DashContainer,
    /// Global Period start.
    pub timeline_start: Duration,
    /// Exact finite Period duration.
    pub duration: Duration,
    /// Правило перевода container timestamps в global presentation timeline.
    pub timestamp_mapping: DashTimestampMapping,
    /// Addressing-specific demux input.
    pub input: DashPeriodInputPlan,
}

/// Private timestamp boundary не позволяет component demuxer-у угадывать origin.
#[derive(Clone, Copy)]
pub(crate) enum DashTimestampMapping {
    /// Serialized/SegmentList input сохраняет proven S34 normalization от первого packet-а.
    NormalizeAtFirstPacket,
    /// Explicit template timestamps остаются на media timeline и вычитают ровно PTO.
    MediaTimeOrigin(Duration),
}

/// Immutable component plan.
#[derive(Clone)]
pub(crate) struct DashComponentPlan {
    /// Required component track layout.
    pub media_kind: DashMediaKind,
    /// Ordered contiguous Periods.
    pub periods: Vec<DashComponentPeriodPlan>,
    /// Exact public presentation duration.
    pub duration: Duration,
}

/// Planned muxed/single либо aligned separate presentation.
#[derive(Clone)]
pub(crate) enum DashPresentationPlan {
    /// Один component.
    Single(DashComponentPlan),
    /// Exact period/fragment-aligned pair.
    Separate {
        /// Video component.
        video: DashComponentPlan,
        /// Audio component.
        audio: DashComponentPlan,
    },
}

/// Secret-safe planning/profile failure.
#[derive(Debug, Error)]
pub enum DashPlanError {
    /// Required Representation selection failed.
    #[error("DASH Representation selection failed: {0}")]
    Selection(#[from] DashRepresentationSelectionError),
    /// BaseURL/reference не образует valid HTTP(S) target.
    #[error("DASH resource target resolution failed")]
    Target,
    /// Template expansion нарушила S34A bounded contract.
    #[error("DASH SegmentTemplate expansion failed: {0}")]
    Template(#[from] DashTemplateError),
    /// Addressing создаёт больше caller-owned resource cap-а.
    #[error("DASH addressing exceeds caller-owned segment bound")]
    SegmentBoundExceeded,
    /// Period duration нельзя точно представить в addressing timescale.
    #[error("DASH Period duration is not integral in addressing timescale")]
    NonIntegralPeriodTimescale,
    /// SegmentList без exact duration не даёт finite seek timeline.
    #[error("DASH SegmentList requires an exact uniform duration")]
    MissingSegmentListDuration,
    /// External SegmentList index нельзя безопасно интерпретировать ordered demux-ом.
    #[error("DASH external SegmentList index is unsupported")]
    ExternalSegmentIndexUnsupported,
    /// External SegmentBase initialization нельзя совместить с byte offsets resource-а.
    #[error("DASH external SegmentBase initialization is unsupported")]
    ExternalSegmentBaseInitializationUnsupported,
    /// Inclusive byte range не помещается в neutral bounded Range contract.
    #[error("DASH byte range is invalid or exceeds platform bounds")]
    InvalidByteRange,
    /// Serialized component нарушает init/media lifecycle.
    #[error("serialized DASH fragments have an invalid init/media lifecycle")]
    InvalidSerializedLifecycle,
    /// Serialized media fragment не имеет positive duration.
    #[error("serialized DASH media fragment requires a positive duration")]
    InvalidSerializedDuration,
    /// Serialized component evidence не соответствует explicit layout.
    #[error("serialized DASH component layout does not match required evidence")]
    SerializedLayoutMismatch,
    /// Separate components имеют разные fragment boundaries.
    #[error("separate DASH components are not exactly aligned")]
    ComponentAlignmentMismatch,
    /// Raw SegmentTimeline содержит gap между соседними references.
    #[error("DASH SegmentTimeline contains a gap")]
    TimelineGap,
    /// Raw SegmentTimeline содержит overlap между соседними references.
    #[error("DASH SegmentTimeline contains an overlap")]
    TimelineOverlap,
    /// Segment reference пересекает Period boundary и требует unsupported sample clipping.
    #[error("DASH segment reference crosses a Period boundary")]
    SegmentCrossesPeriodBoundary,
    /// Static presentation не покрывает Period точно и непрерывно.
    #[error("static DASH SegmentTemplate does not cover the complete Period")]
    IncompleteStaticPeriod,
    /// Checked duration/timestamp arithmetic overflow.
    #[error("DASH timeline arithmetic overflow")]
    TimelineOverflow,
}

/// Строит MPD presentation plan, повторяя exact selection для каждого Period.
pub(crate) fn build_manifest_plan(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    selection: &DashPresentationSelection,
    maximum_segments: NonZeroUsize,
) -> Result<DashPresentationPlan, DashPlanError> {
    build_manifest_plan_with_intent(
        mpd,
        manifest_base,
        selection,
        maximum_segments,
        DashTimelinePlanningIntent::StaticCompletePeriod,
    )
}

/// Строит dynamic snapshot, где sliding head/tail допустимы, но gaps/overlaps запрещены.
pub(crate) fn build_dynamic_manifest_plan(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    selection: &DashPresentationSelection,
    maximum_segments: NonZeroUsize,
) -> Result<DashPresentationPlan, DashPlanError> {
    build_manifest_plan_with_intent(
        mpd,
        manifest_base,
        selection,
        maximum_segments,
        DashTimelinePlanningIntent::DynamicSnapshot,
    )
}

/// Общий S34/S35 planner различает lifecycle через intent, а не positional bool.
fn build_manifest_plan_with_intent(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    selection: &DashPresentationSelection,
    maximum_segments: NonZeroUsize,
    intent: DashTimelinePlanningIntent,
) -> Result<DashPresentationPlan, DashPlanError> {
    match selection {
        DashPresentationSelection::Single { main } => {
            let component =
                build_manifest_component(mpd, manifest_base, main, maximum_segments, intent)?;
            Ok(DashPresentationPlan::Single(component))
        }
        DashPresentationSelection::Separate { video, audio } => {
            let video =
                build_manifest_component(mpd, manifest_base, video, maximum_segments, intent)?;
            let audio =
                build_manifest_component(mpd, manifest_base, audio, maximum_segments, intent)?;
            validate_period_alignment(&video, &audio)?;
            Ok(DashPresentationPlan::Separate { video, audio })
        }
    }
}

/// Строит authoritative serialized single-period plan.
pub(crate) fn build_serialized_plan(
    presentation: &DashSerializedPresentation,
    selection: &DashPresentationSelection,
    maximum_segments: NonZeroUsize,
) -> Result<DashPresentationPlan, DashPlanError> {
    match (presentation, selection) {
        (
            DashSerializedPresentation::Single(component),
            DashPresentationSelection::Single { main },
        ) if component.container == main.container && component.media_kind == main.media_kind => {
            Ok(DashPresentationPlan::Single(build_serialized_component(
                component,
                maximum_segments,
            )?))
        }
        (
            DashSerializedPresentation::Separate { video, audio },
            DashPresentationSelection::Separate {
                video: video_evidence,
                audio: audio_evidence,
            },
        ) if video.container == video_evidence.container
            && audio.container == audio_evidence.container
            && video.media_kind == video_evidence.media_kind
            && audio.media_kind == audio_evidence.media_kind =>
        {
            let video = build_serialized_component(video, maximum_segments)?;
            let audio = build_serialized_component(audio, maximum_segments)?;
            validate_resource_alignment(&video, &audio)?;
            Ok(DashPresentationPlan::Separate { video, audio })
        }
        _ => Err(DashPlanError::SerializedLayoutMismatch),
    }
}

/// Строит один component через все bounded Period-ы.
fn build_manifest_component(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    evidence: &crate::DashRepresentationEvidence,
    maximum_segments: NonZeroUsize,
    intent: DashTimelinePlanningIntent,
) -> Result<DashComponentPlan, DashPlanError> {
    let mpd_base = resolve_optional_base(manifest_base, mpd.base_url.as_ref())?;
    let mut periods = Vec::with_capacity(mpd.periods.len());
    for period in &mpd.periods {
        let selected = select_representation(period, evidence)?;
        periods.push(build_manifest_period(
            period,
            selected,
            &mpd_base,
            maximum_segments,
            intent,
        )?);
    }
    Ok(DashComponentPlan {
        media_kind: evidence.media_kind,
        periods,
        duration: Duration::from_millis(mpd.media_presentation_duration_milliseconds),
    })
}

/// Применяет lexical BaseURL chain и строит addressing одного Period.
fn build_manifest_period(
    period: &DashPeriod,
    selected: SelectedDashRepresentation<'_>,
    mpd_base: &HttpRequestTarget,
    maximum_segments: NonZeroUsize,
    intent: DashTimelinePlanningIntent,
) -> Result<DashComponentPeriodPlan, DashPlanError> {
    let period_base = resolve_optional_base(mpd_base, period.base_url.as_ref())?;
    let adaptation_base =
        resolve_optional_base(&period_base, selected.adaptation.base_url.as_ref())?;
    let representation_base =
        resolve_optional_base(&adaptation_base, selected.representation.base_url.as_ref())?;
    let duration = Duration::from_millis(period.duration_milliseconds);
    let (input, timestamp_mapping) = match &selected.representation.addressing {
        DashAddressing::Template(template) => {
            let (resources, media_time_origin) = plan_template(
                selected.representation,
                template,
                &representation_base,
                duration,
                maximum_segments,
                intent,
            )?;
            (
                DashPeriodInputPlan::Ordered {
                    resources,
                    query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
                },
                DashTimestampMapping::MediaTimeOrigin(media_time_origin),
            )
        }
        DashAddressing::List(list) => (
            DashPeriodInputPlan::Ordered {
                resources: plan_list(list, &representation_base, duration, maximum_segments)?,
                query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
            },
            DashTimestampMapping::NormalizeAtFirstPacket,
        ),
        DashAddressing::Base(segment_base) => {
            validate_segment_base(segment_base)?;
            (
                DashPeriodInputPlan::Range {
                    target: representation_base,
                    query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
                },
                DashTimestampMapping::MediaTimeOrigin(Duration::ZERO),
            )
        }
        DashAddressing::SingleResource => (
            DashPeriodInputPlan::Range {
                target: representation_base,
                query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
            },
            DashTimestampMapping::MediaTimeOrigin(Duration::ZERO),
        ),
    };
    Ok(DashComponentPeriodPlan {
        container: selected.representation.container,
        timeline_start: Duration::from_millis(period.start_milliseconds),
        duration,
        timestamp_mapping,
        input,
    })
}

/// Раскрывает finite SegmentTemplate duration/Timeline.
fn plan_template(
    representation: &DashRepresentation,
    template: &DashSegmentTemplate,
    base: &HttpRequestTarget,
    period_duration: Duration,
    maximum_segments: NonZeroUsize,
    intent: DashTimelinePlanningIntent,
) -> Result<(Vec<DashPlannedResource>, Duration), DashPlanError> {
    let timeline = plan_template_timeline(template, period_duration, maximum_segments, intent)?;
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
            duration: Some(units_to_duration(point.duration, template.timescale)?),
        });
    }
    Ok((resources, timeline.media_time_origin))
}

/// Строит explicit SegmentList resources.
fn plan_list(
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
        resources.push(DashPlannedResource {
            kind: DashSerializedFragmentKind::Media,
            target: resolve_reference(base, segment.media.as_str())?,
            byte_range: segment
                .media_range
                .map(index_range_to_bounded)
                .transpose()?,
            timeline_start: Some(units_to_duration(start_units, list.timescale)?),
            duration: Some(units_to_duration(segment_duration, list.timescale)?),
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
fn validate_segment_base(segment_base: &DashSegmentBase) -> Result<(), DashPlanError> {
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

/// Строит single-period component из concrete fragments.
fn build_serialized_component(
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

/// Проверяет exact Period boundaries pair-а.
fn validate_period_alignment(
    video: &DashComponentPlan,
    audio: &DashComponentPlan,
) -> Result<(), DashPlanError> {
    if video.periods.len() != audio.periods.len()
        || video.duration != audio.duration
        || video
            .periods
            .iter()
            .zip(&audio.periods)
            .any(|(video, audio)| {
                video.timeline_start != audio.timeline_start || video.duration != audio.duration
            })
    {
        return Err(DashPlanError::ComponentAlignmentMismatch);
    }
    Ok(())
}

/// Проверяет count/start/duration каждого serialized media fragment-а.
fn validate_resource_alignment(
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

/// Возвращает media resources ordered period-а.
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
fn resolve_optional_base(
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
