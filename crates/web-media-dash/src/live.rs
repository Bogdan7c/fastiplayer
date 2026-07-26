//! Provider-owned dynamic DASH timing и refresh ordering.
//!
//! Результат этого слоя — только manifest cap. Public DVR обязан дополнительно
//! пересечь его с retained resource, audio packet и video RAP evidence.
//!
//! Runtime policy:
//! - local wall clock калибруется direct UTC относительно before/after fetch midpoint;
//! - complete segment доступен, когда его end не позже `now + total ATO`;
//! - safe live edge ограничен complete segment availability и `now - SPD`;
//! - manifest window начинается с `now - timeShiftBufferDepth`, а без TSB — с AST;
//! - отдельные audio/video окна всегда пересекаются;
//! - меньший `publishTime` игнорируется, равный не меняет snapshot, больший
//!   атомарно принимается только при stable AST/Period/Representation contract;
//! - S31L DVR публикуется лишь из manifest cap и actual audio/video RAP evidence.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use dash_mpd_core::{
    DashAddressing, DashBaseUrl, DashDynamicMpd, DashPeriod, DashRepresentation, DashUtcTimestamp,
};
use media_core::{MediaTime, TimelineRange};
use source_core::HttpRequestTarget;
use thiserror::Error;

use crate::DashPlanError;
use crate::catalog::{
    DashLogicalRepresentationLane, DashLogicalRepresentationSelection, rematch_logical_selection,
};
use crate::plan::{
    DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPresentationPlan,
    build_dynamic_manifest_plan, build_manifest_plan_from_logical_selection,
};
use crate::request::DashSerializedFragmentKind;
use crate::selection::{
    DashPresentationSelection, DashRepresentationEvidence, DashRepresentationSelectionError,
    select_representation,
};

// Blocking demux/refresh lifecycle живёт отдельно от pure timing math.
mod runtime;
pub(crate) use runtime::prepare_dash_live_logical;
pub use runtime::{
    DashEndpointRefreshError, DashEndpointRefreshPort, DashEndpointRefreshReply,
    DashEndpointRefreshRequest, DashLiveOpenError, DashLiveOpenRequest, DashLiveOpenResult,
    prepare_dash_live,
};

/// Injected local wall clock; production implementation остаётся app-owned.
pub trait DashWallClock: Send + Sync {
    /// Возвращает текущий UTC estimate локальной системы.
    fn now_utc(&self) -> DashUtcTimestamp;
}

/// Clock synchronization failure без UTC payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DashLiveClockError {
    /// Signed offset либо synchronized timestamp вышел за диапазон.
    #[error("DASH synchronized clock arithmetic overflow")]
    Overflow,
    /// Local clock пошёл назад между началом и концом fetch-а.
    #[error("DASH local clock regressed during synchronization")]
    ClockRegression,
    /// Synchronized now предшествует availability start.
    #[error("DASH synchronized clock precedes availability start")]
    BeforeAvailabilityStart,
}

/// Clock с direct-UTC offset/evidence текущего MPD response-а.
#[derive(Clone)]
pub struct DashSynchronizedClock {
    local: Arc<dyn DashWallClock>,
    offset_nanoseconds: i128,
}

impl std::fmt::Debug for DashSynchronizedClock {
    /// Не раскрывает direct UTCTiming payload.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashSynchronizedClock")
            .finish_non_exhaustive()
    }
}

impl DashSynchronizedClock {
    /// Фиксирует offset относительно midpoint local clock вокруг MPD fetch-а.
    ///
    /// При нечётном RTT используется exact policy `after - floor(RTT / 2)`,
    /// поэтому лишняя наносекунда детерминированно остаётся на стороне `after`.
    pub fn from_direct_utc(
        local: Arc<dyn DashWallClock>,
        local_before_fetch: DashUtcTimestamp,
        local_after_fetch: DashUtcTimestamp,
        direct_utc_at_response: DashUtcTimestamp,
    ) -> Result<Self, DashLiveClockError> {
        let round_trip_nanoseconds = local_after_fetch
            .unix_nanoseconds()
            .checked_sub(local_before_fetch.unix_nanoseconds())
            .ok_or(DashLiveClockError::Overflow)?;
        if round_trip_nanoseconds < 0 {
            return Err(DashLiveClockError::ClockRegression);
        }
        let local_midpoint = local_after_fetch
            .unix_nanoseconds()
            .checked_sub(round_trip_nanoseconds / 2)
            .ok_or(DashLiveClockError::Overflow)?;
        let offset_nanoseconds = direct_utc_at_response
            .unix_nanoseconds()
            .checked_sub(local_midpoint)
            .ok_or(DashLiveClockError::Overflow)?;
        Ok(Self {
            local,
            offset_nanoseconds,
        })
    }

    /// Возвращает synchronized wall clock без нового network запроса.
    pub fn now_utc(&self) -> Result<DashUtcTimestamp, DashLiveClockError> {
        let unix_nanoseconds = self
            .local
            .now_utc()
            .unix_nanoseconds()
            .checked_add(self.offset_nanoseconds)
            .ok_or(DashLiveClockError::Overflow)?;
        Ok(DashUtcTimestamp::from_unix_nanoseconds(unix_nanoseconds))
    }
}

/// Manifest-derived cap выбранных Representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashLiveAvailability {
    /// Safe playable edge после SPD и A/V intersection.
    pub live_edge: MediaTime,
    /// Manifest/clock intersection; ещё не public DVR evidence.
    pub manifest_range: TimelineRange,
}

/// Immutable accepted dynamic snapshot и finite announced plan.
#[derive(Clone)]
pub struct DashLiveSnapshot {
    /// Pure checked dynamic schema.
    pub mpd: DashDynamicMpd,
    /// Exact selected finite resources текущего snapshot-а.
    pub(crate) plan: DashPresentationPlan,
    /// Exact выбранные logical lanes для refresh continuity.
    selection: DashLiveSelection,
    /// Manifest-only availability cap.
    pub availability: DashLiveAvailability,
}

/// Runtime-private selector сохраняет source-compatible evidence path и exact logical lane path.
#[derive(Clone)]
pub(crate) enum DashLiveSelection {
    Evidence(DashPresentationSelection),
    Logical(Box<DashLogicalRepresentationSelection>),
}

impl std::fmt::Debug for DashLiveSnapshot {
    /// Resource targets не попадают в diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashLiveSnapshot")
            .field("availability", &self.availability)
            .finish_non_exhaustive()
    }
}

/// Typed refresh continuity/ordering failure.
#[derive(Debug, Error)]
pub enum DashLiveRefreshError {
    /// Валидная DASH timing-модель намеренно не входит в S35 profile.
    #[error("DASH live profile is excluded: {0:?}")]
    ProfileExcluded(DashLiveProfileExclusion),
    /// Planning выбранных Representation не удалось.
    #[error("DASH live planning failed")]
    Plan(#[from] DashPlanError),
    /// Exact representation identity отсутствует либо неоднозначна.
    #[error("DASH live representation selection failed")]
    Selection(#[from] DashRepresentationSelectionError),
    /// Clock synchronization не даёт точного MPD now.
    #[error("DASH live clock failed")]
    Clock(#[from] DashLiveClockError),
    /// Availability intersection пуста.
    #[error("DASH selected representations have no shared availability")]
    EmptyAvailability,
    /// Stable MPD/Period/Representation identity изменилась.
    #[error("DASH live refresh continuity failed")]
    Continuity,
}

/// Typed причины отказа от timing-модели, которую нельзя приблизительно сжать в S31L range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashLiveProfileExclusion {
    /// Explicit SegmentTimeline содержит дыру.
    TimelineGap,
    /// Explicit SegmentTimeline содержит перекрытие.
    TimelineOverlap,
    /// Segment reference пересекает Period boundary и требует decode-only clipping.
    SegmentCrossesPeriodBoundary,
}

/// Refresh outcome явно различает equal/stale/newer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashLiveRefreshOutcome {
    /// Equal publishTime завершает turn без замены state.
    EqualUnchanged,
    /// Older publishTime проигнорирован без mutation.
    StaleIgnored,
    /// Newer fully validated snapshot атомарно готов к commit.
    Replaced,
}

/// Строит selected snapshot и точный manifest availability cap.
pub fn build_dash_live_snapshot(
    mpd: DashDynamicMpd,
    manifest_base: &HttpRequestTarget,
    selection: &DashPresentationSelection,
    maximum_segments: NonZeroUsize,
    clock: &DashSynchronizedClock,
) -> Result<DashLiveSnapshot, DashLiveRefreshError> {
    build_dash_live_snapshot_with_selection(
        mpd,
        manifest_base,
        &DashLiveSelection::Evidence(selection.clone()),
        maximum_segments,
        clock,
    )
}

pub(crate) fn build_dash_live_snapshot_with_selection(
    mpd: DashDynamicMpd,
    manifest_base: &HttpRequestTarget,
    selection: &DashLiveSelection,
    maximum_segments: NonZeroUsize,
    clock: &DashSynchronizedClock,
) -> Result<DashLiveSnapshot, DashLiveRefreshError> {
    let selection = match selection {
        DashLiveSelection::Evidence(selection) => DashLiveSelection::Evidence(selection.clone()),
        DashLiveSelection::Logical(selection) => DashLiveSelection::Logical(Box::new(
            rematch_logical_selection(&mpd.presentation, selection)
                .map_err(|_| DashLiveRefreshError::Continuity)?,
        )),
    };
    let plan = match &selection {
        DashLiveSelection::Evidence(selection) => build_dynamic_manifest_plan(
            &mpd.presentation,
            manifest_base,
            selection,
            maximum_segments,
        ),
        DashLiveSelection::Logical(selection) => build_manifest_plan_from_logical_selection(
            &mpd.presentation,
            manifest_base,
            selection,
            maximum_segments,
            crate::catalog::DashRepresentationLaneTimelineMode::Dynamic,
        ),
    }
    .map_err(map_dynamic_plan_error)?;
    let now_on_timeline = synchronized_timeline_now(&mpd, clock)?;
    let delay = Duration::from_millis(mpd.suggested_presentation_delay_milliseconds);
    let delayed_edge = now_on_timeline
        .checked_sub(delay)
        .ok_or(DashLiveRefreshError::EmptyAvailability)?;
    let window_start = match mpd.time_shift_buffer_depth_milliseconds {
        Some(depth) => now_on_timeline.saturating_sub(Duration::from_millis(depth)),
        None => Duration::ZERO,
    };
    let manifest_range = match (&plan, &selection) {
        (
            DashPresentationPlan::Single(component),
            DashLiveSelection::Evidence(DashPresentationSelection::Single { main }),
        ) => component_availability(
            &mpd,
            component,
            main,
            window_start,
            now_on_timeline,
            delayed_edge,
        )?,
        (
            DashPresentationPlan::Separate { video, audio },
            DashLiveSelection::Evidence(DashPresentationSelection::Separate {
                video: video_selection,
                audio: audio_selection,
            }),
        ) => {
            let video = component_availability(
                &mpd,
                video,
                video_selection,
                window_start,
                now_on_timeline,
                delayed_edge,
            )?;
            let audio = component_availability(
                &mpd,
                audio,
                audio_selection,
                window_start,
                now_on_timeline,
                delayed_edge,
            )?;
            intersect_ranges(video, audio).ok_or(DashLiveRefreshError::EmptyAvailability)?
        }
        (plan, DashLiveSelection::Logical(selection)) => match (plan, selection.as_ref()) {
            (
                DashPresentationPlan::Single(component),
                DashLogicalRepresentationSelection::Single(lane),
            ) => component_availability_from_locations(
                &mpd,
                component,
                &lane.locations,
                window_start,
                now_on_timeline,
                delayed_edge,
            )?,
            (
                DashPresentationPlan::Separate { video, audio },
                DashLogicalRepresentationSelection::Separate {
                    video: video_lane,
                    audio: audio_lane,
                },
            ) => {
                let video = component_availability_from_locations(
                    &mpd,
                    video,
                    &video_lane.locations,
                    window_start,
                    now_on_timeline,
                    delayed_edge,
                )?;
                let audio = component_availability_from_locations(
                    &mpd,
                    audio,
                    &audio_lane.locations,
                    window_start,
                    now_on_timeline,
                    delayed_edge,
                )?;
                intersect_ranges(video, audio).ok_or(DashLiveRefreshError::EmptyAvailability)?
            }
            _ => return Err(DashLiveRefreshError::Continuity),
        },
        _ => return Err(DashLiveRefreshError::Continuity),
    };
    Ok(DashLiveSnapshot {
        mpd,
        plan,
        selection,
        availability: DashLiveAvailability {
            live_edge: manifest_range.end,
            manifest_range,
        },
    })
}

/// Сохраняет S35 `ProfileExcluded` vocabulary для неоднозначных timing-моделей.
fn map_dynamic_plan_error(error: DashPlanError) -> DashLiveRefreshError {
    match error {
        DashPlanError::TimelineGap => {
            DashLiveRefreshError::ProfileExcluded(DashLiveProfileExclusion::TimelineGap)
        }
        DashPlanError::TimelineOverlap => {
            DashLiveRefreshError::ProfileExcluded(DashLiveProfileExclusion::TimelineOverlap)
        }
        DashPlanError::SegmentCrossesPeriodBoundary => DashLiveRefreshError::ProfileExcluded(
            DashLiveProfileExclusion::SegmentCrossesPeriodBoundary,
        ),
        other => DashLiveRefreshError::Plan(other),
    }
}

/// Применяет publish ordering и continuity до caller-owned atomic swap.
pub fn refresh_dash_live_snapshot(
    current: &mut DashLiveSnapshot,
    next: DashLiveSnapshot,
) -> Result<DashLiveRefreshOutcome, DashLiveRefreshError> {
    match next.mpd.publish_time.cmp(&current.mpd.publish_time) {
        std::cmp::Ordering::Less => Ok(DashLiveRefreshOutcome::StaleIgnored),
        std::cmp::Ordering::Equal => Ok(DashLiveRefreshOutcome::EqualUnchanged),
        std::cmp::Ordering::Greater => {
            validate_continuity(&current.mpd, &next.mpd, &current.selection, &next.selection)?;
            *current = next;
            Ok(DashLiveRefreshOutcome::Replaced)
        }
    }
}

/// Endpoint recovery разрешает equal publishTime, но сохраняет identity validation.
pub(crate) fn replace_dash_live_endpoint_snapshot(
    current: &mut DashLiveSnapshot,
    next: DashLiveSnapshot,
) -> Result<DashLiveRefreshOutcome, DashLiveRefreshError> {
    if next.mpd.publish_time < current.mpd.publish_time {
        return Ok(DashLiveRefreshOutcome::StaleIgnored);
    }
    validate_continuity(&current.mpd, &next.mpd, &current.selection, &next.selection)?;
    *current = next;
    Ok(DashLiveRefreshOutcome::Replaced)
}

/// Переводит synchronized wall clock на MPD timeline.
fn synchronized_timeline_now(
    mpd: &DashDynamicMpd,
    clock: &DashSynchronizedClock,
) -> Result<Duration, DashLiveClockError> {
    let elapsed = clock
        .now_utc()?
        .unix_nanoseconds()
        .checked_sub(mpd.availability_start_time.unix_nanoseconds())
        .ok_or(DashLiveClockError::Overflow)?;
    let elapsed =
        u64::try_from(elapsed).map_err(|_| DashLiveClockError::BeforeAvailabilityStart)?;
    Ok(Duration::from_nanos(elapsed))
}

/// Вычисляет availability одного component с period-specific ATO.
fn component_availability(
    mpd: &DashDynamicMpd,
    component: &DashComponentPlan,
    evidence: &DashRepresentationEvidence,
    window_start: Duration,
    now_on_timeline: Duration,
    delayed_edge: Duration,
) -> Result<TimelineRange, DashLiveRefreshError> {
    calculate_component_availability(
        mpd,
        component,
        AvailabilitySelection::Evidence(evidence),
        window_start,
        now_on_timeline,
        delayed_edge,
    )
}

fn component_availability_from_locations(
    mpd: &DashDynamicMpd,
    component: &DashComponentPlan,
    locations: &[(usize, usize)],
    window_start: Duration,
    now_on_timeline: Duration,
    delayed_edge: Duration,
) -> Result<TimelineRange, DashLiveRefreshError> {
    if locations.len() != mpd.presentation.periods.len() {
        return Err(DashLiveRefreshError::Continuity);
    }
    calculate_component_availability(
        mpd,
        component,
        AvailabilitySelection::Locations(locations),
        window_start,
        now_on_timeline,
        delayed_edge,
    )
}

#[derive(Clone, Copy)]
enum AvailabilitySelection<'selection> {
    Evidence(&'selection DashRepresentationEvidence),
    Locations(&'selection [(usize, usize)]),
}

fn calculate_component_availability(
    mpd: &DashDynamicMpd,
    component: &DashComponentPlan,
    selection: AvailabilitySelection<'_>,
    window_start: Duration,
    now_on_timeline: Duration,
    delayed_edge: Duration,
) -> Result<TimelineRange, DashLiveRefreshError> {
    let mut earliest = None;
    let mut latest = None;
    for (period_index, (period, period_plan)) in mpd
        .presentation
        .periods
        .iter()
        .zip(&component.periods)
        .enumerate()
    {
        let (adaptation, representation) = match selection {
            AvailabilitySelection::Evidence(evidence) => {
                let selected = select_representation(period, evidence)?;
                (selected.adaptation, selected.representation)
            }
            AvailabilitySelection::Locations(locations) => {
                let &(adaptation_index, representation_index) = locations
                    .get(period_index)
                    .ok_or(DashLiveRefreshError::Continuity)?;
                let adaptation = period
                    .adaptation_sets
                    .get(adaptation_index)
                    .ok_or(DashLiveRefreshError::Continuity)?;
                let representation = adaptation
                    .representations
                    .get(representation_index)
                    .ok_or(DashLiveRefreshError::Continuity)?;
                (adaptation, representation)
            }
        };
        let offset = total_availability_offset_nanoseconds(
            mpd.presentation.base_url.as_ref(),
            period,
            adaptation,
            representation,
        )?;
        let availability_end = add_signed_duration(now_on_timeline, offset)
            .ok_or(DashLiveRefreshError::EmptyAvailability)?;
        for resource in period_media_resources(period_plan) {
            let local_start = resource
                .timeline_start
                .ok_or(DashLiveRefreshError::Continuity)?;
            let resource_start = period_plan
                .timeline_start
                .checked_add(local_start)
                .ok_or(DashLiveRefreshError::Continuity)?;
            let resource_end = resource_start
                .checked_add(resource.duration.ok_or(DashLiveRefreshError::Continuity)?)
                .ok_or(DashLiveRefreshError::Continuity)?;
            if resource_end < window_start
                || resource_end > availability_end
                || resource_start >= delayed_edge
            {
                continue;
            }
            earliest =
                Some(earliest.map_or(resource_start, |value: Duration| value.min(resource_start)));
            latest = Some(latest.map_or(resource_end, |value: Duration| value.max(resource_end)));
        }
    }
    let start = earliest
        .map(|start| start.max(window_start))
        .ok_or(DashLiveRefreshError::EmptyAvailability)?;
    let end = latest
        .map(|end| end.min(delayed_edge))
        .filter(|end| start < *end)
        .ok_or(DashLiveRefreshError::EmptyAvailability)?;
    Ok(TimelineRange {
        start: MediaTime::from_duration(start),
        end: MediaTime::from_duration(end),
    })
}

/// Извлекает только media resources ordered profile-а.
fn period_media_resources(
    period: &DashComponentPeriodPlan,
) -> impl Iterator<Item = &crate::plan::DashPlannedResource> {
    let resources = match &period.input {
        DashPeriodInputPlan::Ordered { resources, .. } => resources.as_slice(),
        DashPeriodInputPlan::Range { .. } => &[],
    };
    resources
        .iter()
        .filter(|resource| resource.kind == DashSerializedFragmentKind::Media)
}

/// Складывает ATO всех применимых BaseURL и SegmentTemplate.
fn total_availability_offset_nanoseconds(
    root_base: Option<&DashBaseUrl>,
    period: &DashPeriod,
    adaptation: &dash_mpd_core::DashAdaptationSet,
    representation: &dash_mpd_core::DashRepresentation,
) -> Result<i128, DashLiveRefreshError> {
    let mut total = 0_i128;
    for base in [
        root_base,
        period.base_url.as_ref(),
        adaptation.base_url.as_ref(),
        representation.base_url.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        total = total
            .checked_add(base.availability_time_offset_nanoseconds.unwrap_or(0))
            .ok_or(DashLiveRefreshError::Continuity)?;
    }
    let DashAddressing::Template(template) = &representation.addressing else {
        return Err(DashLiveRefreshError::Continuity);
    };
    total
        .checked_add(template.availability_time_offset_nanoseconds.unwrap_or(0))
        .ok_or(DashLiveRefreshError::Continuity)
}

/// Применяет signed ATO к non-negative MPD duration.
fn add_signed_duration(duration: Duration, offset_nanoseconds: i128) -> Option<Duration> {
    let nanoseconds = i128::try_from(duration.as_nanos())
        .ok()?
        .checked_add(offset_nanoseconds)?;
    u64::try_from(nanoseconds).ok().map(Duration::from_nanos)
}

/// Пересекает A/V availability.
fn intersect_ranges(left: TimelineRange, right: TimelineRange) -> Option<TimelineRange> {
    let start = left.start.max(right.start);
    let end = left.end.min(right.end);
    (start < end).then_some(TimelineRange { start, end })
}

/// Проверяет immutable identity/timing существующих Period-ов.
fn validate_continuity(
    current: &DashDynamicMpd,
    next: &DashDynamicMpd,
    current_selection: &DashLiveSelection,
    next_selection: &DashLiveSelection,
) -> Result<(), DashLiveRefreshError> {
    if current.availability_start_time != next.availability_start_time
        || !selection_contract_is_stable(current_selection, next_selection)
    {
        return Err(DashLiveRefreshError::Continuity);
    }
    for (current_period_index, current_period) in current.presentation.periods.iter().enumerate() {
        let Some(period_id) = current_period.id.as_deref() else {
            return Err(DashLiveRefreshError::Continuity);
        };
        let Some((next_period_index, next_period)) = next
            .presentation
            .periods
            .iter()
            .enumerate()
            .find(|(_, period)| period.id.as_deref() == Some(period_id))
        else {
            continue;
        };
        if current_period.start_milliseconds != next_period.start_milliseconds
            || current_period.duration_milliseconds != next_period.duration_milliseconds
            || !period_selected_contract_is_stable(
                current_period,
                next_period,
                current_period_index,
                next_period_index,
                current_selection,
                next_selection,
            )
        {
            return Err(DashLiveRefreshError::Continuity);
        }
    }
    Ok(())
}

fn selection_contract_is_stable(current: &DashLiveSelection, next: &DashLiveSelection) -> bool {
    match (current, next) {
        (DashLiveSelection::Evidence(current), DashLiveSelection::Evidence(next)) => {
            current == next
        }
        (DashLiveSelection::Logical(current), DashLiveSelection::Logical(next)) => {
            logical_selection_contract_is_stable(current, next)
        }
        _ => false,
    }
}

fn logical_selection_contract_is_stable(
    current: &DashLogicalRepresentationSelection,
    next: &DashLogicalRepresentationSelection,
) -> bool {
    match (current, next) {
        (
            DashLogicalRepresentationSelection::Single(current),
            DashLogicalRepresentationSelection::Single(next),
        ) => current.contract == next.contract,
        (
            DashLogicalRepresentationSelection::Separate {
                video: current_video,
                audio: current_audio,
            },
            DashLogicalRepresentationSelection::Separate {
                video: next_video,
                audio: next_audio,
            },
        ) => {
            current_video.contract == next_video.contract
                && current_audio.contract == next_audio.contract
        }
        _ => false,
    }
}

/// Не выбранные siblings могут reorder/add/remove; authoritative lanes обязаны совпасть точно.
fn period_selected_contract_is_stable(
    current: &DashPeriod,
    next: &DashPeriod,
    current_period_index: usize,
    next_period_index: usize,
    current_selection: &DashLiveSelection,
    next_selection: &DashLiveSelection,
) -> bool {
    let evidence_stable = |evidence: &DashRepresentationEvidence| {
        let Ok(current) = select_representation(current, evidence) else {
            return false;
        };
        let Ok(next) = select_representation(next, evidence) else {
            return false;
        };
        representation_contract_is_stable(current.representation, next.representation, true)
    };
    let logical_stable = |current_lane: &DashLogicalRepresentationLane,
                          next_lane: &DashLogicalRepresentationLane| {
        let Some(current) = lane_period_representation(current, current_lane, current_period_index)
        else {
            return false;
        };
        let Some(next) = lane_period_representation(next, next_lane, next_period_index) else {
            return false;
        };
        representation_contract_is_stable(current, next, false)
    };
    match (current_selection, next_selection) {
        (
            DashLiveSelection::Evidence(DashPresentationSelection::Single { main }),
            DashLiveSelection::Evidence(_),
        ) => evidence_stable(main),
        (
            DashLiveSelection::Evidence(DashPresentationSelection::Separate { video, audio }),
            DashLiveSelection::Evidence(_),
        ) => evidence_stable(video) && evidence_stable(audio),
        (DashLiveSelection::Logical(current), DashLiveSelection::Logical(next)) => {
            match (current.as_ref(), next.as_ref()) {
                (
                    DashLogicalRepresentationSelection::Single(current),
                    DashLogicalRepresentationSelection::Single(next),
                ) => logical_stable(current, next),
                (
                    DashLogicalRepresentationSelection::Separate {
                        video: current_video,
                        audio: current_audio,
                    },
                    DashLogicalRepresentationSelection::Separate {
                        video: next_video,
                        audio: next_audio,
                    },
                ) => {
                    logical_stable(current_video, next_video)
                        && logical_stable(current_audio, next_audio)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn lane_period_representation<'period>(
    period: &'period DashPeriod,
    lane: &DashLogicalRepresentationLane,
    period_index: usize,
) -> Option<&'period DashRepresentation> {
    let &(adaptation_index, representation_index) = lane.locations.get(period_index)?;
    period
        .adaptation_sets
        .get(adaptation_index)?
        .representations
        .get(representation_index)
}

/// Endpoint references и sliding SegmentTimeline могут меняться; media shape — нет.
fn representation_contract_is_stable(
    current: &DashRepresentation,
    next: &DashRepresentation,
    require_same_id: bool,
) -> bool {
    let template_timing = |representation: &DashRepresentation| match &representation.addressing {
        DashAddressing::Template(template) => Some((
            template.timescale,
            template.presentation_time_offset,
            template.duration,
        )),
        _ => None,
    };
    (!require_same_id || current.id == next.id)
        && current.bandwidth == next.bandwidth
        && current.width == next.width
        && current.height == next.height
        && current.frame_rate == next.frame_rate
        && current.audio_sampling_rate == next.audio_sampling_rate
        && current.audio_channel_configuration == next.audio_channel_configuration
        && current.language == next.language
        && current.color == next.color
        && current.container == next.container
        && current.media_kind == next.media_kind
        && current.codecs == next.codecs
        && template_timing(current) == template_timing(next)
}

#[cfg(test)]
mod tests;
