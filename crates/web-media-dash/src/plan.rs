//! Pure finite DASH planning: BaseURL, addressing, clocks и component alignment.

use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use dash_mpd_core::{
    DashAddressing, DashContainer, DashMediaKind, DashMpd, DashPeriod, DashPresentationDuration,
    DashTemplateError,
};
use source_core::{HttpBoundedByteRange, HttpRequestTarget};
use thiserror::Error;
use web_media_adaptive::AdaptiveResourceQueryApplication;

use crate::catalog::{DashLogicalRepresentationSelection, DashRepresentationLaneTimelineMode};
use crate::request::{DashSerializedFragmentKind, DashSerializedPresentation};
use crate::selection::{
    DashPresentationSelection, DashRepresentationSelectionError, SelectedDashRepresentation,
    select_representation,
};

mod continuation;
mod lifecycle;
mod resources;
mod timeline;
pub(crate) use continuation::{DashComponentContinuationPoint, DashPresentationContinuationPoint};
use lifecycle::{component_snapshot_duration, validate_period_alignment};
use resources::{
    build_serialized_component, plan_list, plan_template, resolve_optional_base,
    segment_base_catalog_probe_content_length, validate_resource_alignment, validate_segment_base,
};
use timeline::{DashPeriodTimelineBound, DashTimelinePlanningIntent};

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
        /// Zero-based init prefix для catalog proof; playback всегда видит full resource.
        catalog_probe_content_length: Option<NonZeroU64>,
    },
}

/// Declared Period lifecycle остаётся отдельным от operational live snapshot horizon.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DashPeriodLifecycle {
    /// Завершённый Period имеет exact neutral duration.
    Finite(Duration),
    /// Последний dynamic Period продолжается после текущего snapshot-а.
    OpenEnded,
}

/// Один component Period с global timeline placement.
#[derive(Clone)]
pub(crate) struct DashComponentPeriodPlan {
    /// Proven container.
    pub container: DashContainer,
    /// Global Period start.
    pub timeline_start: Duration,
    /// Declared lifecycle не смешивается с границей текущего live snapshot-а.
    pub declared_lifecycle: DashPeriodLifecycle,
    /// Operational finite horizon текущего plan-а.
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
    /// Open Period требует explicit SegmentTimeline snapshot-а.
    #[error("open DASH Period requires an explicit SegmentTimeline snapshot")]
    OpenEndedPeriodRequiresExplicitTimeline,
    /// Последний `r=-1` open Period-а не имеет доказанной конечной границы.
    #[error("open DASH SegmentTimeline repeat has no finite boundary")]
    OpenEndedTimelineRepeat,
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
        duration: component_snapshot_duration(mpd.media_presentation_duration, &periods)?,
        periods,
    })
}

/// Доказывает совместимость exact logical lanes тем же Period planner-ом, что runtime open.
pub(crate) fn prove_manifest_lane_alignment(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    video_locations: &[(usize, usize)],
    audio_locations: &[(usize, usize)],
    maximum_segments: NonZeroUsize,
    mode: DashRepresentationLaneTimelineMode,
) -> Result<(), DashPlanError> {
    let intent = lane_timeline_intent(mode);
    let video = build_manifest_component_from_locations(
        mpd,
        manifest_base,
        video_locations,
        maximum_segments,
        intent,
        DashMediaKind::Video,
    )?;
    let audio = build_manifest_component_from_locations(
        mpd,
        manifest_base,
        audio_locations,
        maximum_segments,
        intent,
        DashMediaKind::Audio,
    )?;
    validate_period_alignment(&video, &audio)
}

/// Доказывает, что одна logical lane сама образует openable timeline во всех Periods.
pub(crate) fn prove_manifest_lane(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    locations: &[(usize, usize)],
    maximum_segments: NonZeroUsize,
    mode: DashRepresentationLaneTimelineMode,
    media_kind: DashMediaKind,
) -> Result<(), DashPlanError> {
    build_manifest_component_from_locations(
        mpd,
        manifest_base,
        locations,
        maximum_segments,
        lane_timeline_intent(mode),
        media_kind,
    )?;
    Ok(())
}

/// Строит runtime plan из provider-owned exact lane mapping без lossy evidence rematch-а.
pub(crate) fn build_manifest_plan_from_logical_selection(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    selection: &DashLogicalRepresentationSelection,
    maximum_segments: NonZeroUsize,
    mode: DashRepresentationLaneTimelineMode,
) -> Result<DashPresentationPlan, DashPlanError> {
    let intent = lane_timeline_intent(mode);
    match selection {
        DashLogicalRepresentationSelection::Single(lane) => {
            let component = build_manifest_component_from_locations(
                mpd,
                manifest_base,
                &lane.locations,
                maximum_segments,
                intent,
                lane.contract.kind,
            )?;
            Ok(DashPresentationPlan::Single(component))
        }
        DashLogicalRepresentationSelection::Separate { video, audio } => {
            if video.contract.kind != DashMediaKind::Video
                || audio.contract.kind != DashMediaKind::Audio
            {
                return Err(DashRepresentationSelectionError::Absent.into());
            }
            let video = build_manifest_component_from_locations(
                mpd,
                manifest_base,
                &video.locations,
                maximum_segments,
                intent,
                DashMediaKind::Video,
            )?;
            let audio = build_manifest_component_from_locations(
                mpd,
                manifest_base,
                &audio.locations,
                maximum_segments,
                intent,
                DashMediaKind::Audio,
            )?;
            validate_period_alignment(&video, &audio)?;
            Ok(DashPresentationPlan::Separate { video, audio })
        }
    }
}

const fn lane_timeline_intent(
    mode: DashRepresentationLaneTimelineMode,
) -> DashTimelinePlanningIntent {
    match mode {
        DashRepresentationLaneTimelineMode::Static => {
            DashTimelinePlanningIntent::StaticCompletePeriod
        }
        DashRepresentationLaneTimelineMode::Dynamic => DashTimelinePlanningIntent::DynamicSnapshot,
    }
}

/// Exact snapshot-local locations не зависят от lossy caller evidence.
fn build_manifest_component_from_locations(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    locations: &[(usize, usize)],
    maximum_segments: NonZeroUsize,
    intent: DashTimelinePlanningIntent,
    media_kind: DashMediaKind,
) -> Result<DashComponentPlan, DashPlanError> {
    if locations.len() != mpd.periods.len() {
        return Err(DashRepresentationSelectionError::Absent.into());
    }
    let mpd_base = resolve_optional_base(manifest_base, mpd.base_url.as_ref())?;
    let mut periods = Vec::with_capacity(mpd.periods.len());
    for (period, &(adaptation_index, representation_index)) in mpd.periods.iter().zip(locations) {
        let adaptation = period
            .adaptation_sets
            .get(adaptation_index)
            .ok_or(DashRepresentationSelectionError::Absent)?;
        let representation = adaptation
            .representations
            .get(representation_index)
            .ok_or(DashRepresentationSelectionError::Absent)?;
        if representation.media_kind != media_kind {
            return Err(DashRepresentationSelectionError::Absent.into());
        }
        periods.push(build_manifest_period(
            period,
            SelectedDashRepresentation {
                adaptation,
                representation,
            },
            &mpd_base,
            maximum_segments,
            intent,
        )?);
    }
    Ok(DashComponentPlan {
        media_kind,
        duration: component_snapshot_duration(mpd.media_presentation_duration, &periods)?,
        periods,
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
    let period_bound = match period.duration {
        DashPresentationDuration::FiniteMilliseconds(duration_milliseconds) => {
            DashPeriodTimelineBound::Finite(Duration::from_millis(duration_milliseconds))
        }
        DashPresentationDuration::OpenEnded => DashPeriodTimelineBound::OpenEnded,
    };
    let declared_lifecycle = match period_bound {
        DashPeriodTimelineBound::Finite(duration) => DashPeriodLifecycle::Finite(duration),
        DashPeriodTimelineBound::OpenEnded => DashPeriodLifecycle::OpenEnded,
    };
    let (input, timestamp_mapping, snapshot_duration) = match &selected.representation.addressing {
        DashAddressing::Template(template) => {
            let (resources, media_time_origin, snapshot_duration) = plan_template(
                selected.representation,
                template,
                &representation_base,
                period_bound,
                maximum_segments,
                intent,
            )?;
            (
                DashPeriodInputPlan::Ordered {
                    resources,
                    query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
                },
                DashTimestampMapping::MediaTimeOrigin(media_time_origin),
                snapshot_duration,
            )
        }
        DashAddressing::List(list) => {
            let duration = finite_period_duration(period_bound)?;
            (
                DashPeriodInputPlan::Ordered {
                    resources: plan_list(list, &representation_base, duration, maximum_segments)?,
                    query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
                },
                DashTimestampMapping::NormalizeAtFirstPacket,
                duration,
            )
        }
        DashAddressing::Base(segment_base) => {
            let duration = finite_period_duration(period_bound)?;
            validate_segment_base(segment_base)?;
            (
                DashPeriodInputPlan::Range {
                    target: representation_base,
                    query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
                    catalog_probe_content_length: segment_base_catalog_probe_content_length(
                        segment_base,
                    ),
                },
                DashTimestampMapping::MediaTimeOrigin(Duration::ZERO),
                duration,
            )
        }
        DashAddressing::SingleResource => {
            let duration = finite_period_duration(period_bound)?;
            (
                DashPeriodInputPlan::Range {
                    target: representation_base,
                    query_application: AdaptiveResourceQueryApplication::ApplyScopedReplacement,
                    catalog_probe_content_length: None,
                },
                DashTimestampMapping::MediaTimeOrigin(Duration::ZERO),
                duration,
            )
        }
    };
    Ok(DashComponentPeriodPlan {
        container: selected.representation.container,
        timeline_start: Duration::from_millis(period.start_milliseconds),
        declared_lifecycle,
        duration: snapshot_duration,
        timestamp_mapping,
        input,
    })
}

/// Addressing без explicit SegmentTimeline остаётся finite-only.
fn finite_period_duration(
    period_bound: DashPeriodTimelineBound,
) -> Result<Duration, DashPlanError> {
    match period_bound {
        DashPeriodTimelineBound::Finite(duration) => Ok(duration),
        DashPeriodTimelineBound::OpenEnded => {
            Err(DashPlanError::OpenEndedPeriodRequiresExplicitTimeline)
        }
    }
}
