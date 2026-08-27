//! Checked availability math и continuity выбранных DASH live lanes.

use std::time::Duration;

use dash_mpd_core::{DashAddressing, DashBaseUrl, DashDynamicMpd, DashPeriod, DashRepresentation};
use media_core::{MediaTime, TimelineRange};

use super::{DashLiveClockError, DashLiveRefreshError, DashLiveSelection, DashSynchronizedClock};
use crate::catalog::{DashLogicalRepresentationLane, DashLogicalRepresentationSelection};
use crate::plan::{
    DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPresentationPlan,
};
use crate::request::DashSerializedFragmentKind;
use crate::selection::{
    DashPresentationSelection, DashRepresentationEvidence, select_representation,
};

/// Manifest-derived cap выбранных Representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashLiveAvailability {
    /// Safe playable edge после SPD и A/V intersection.
    pub live_edge: MediaTime,
    /// Manifest/clock intersection; ещё не public DVR evidence.
    pub manifest_range: TimelineRange,
}

/// Вычисляет единый manifest cap для exact selected lane topology.
pub(super) fn calculate_availability(
    mpd: &DashDynamicMpd,
    plan: &DashPresentationPlan,
    selection: &DashLiveSelection,
    clock: &DashSynchronizedClock,
) -> Result<DashLiveAvailability, DashLiveRefreshError> {
    let now_on_timeline = synchronized_timeline_now(mpd, clock)?;
    let delay = Duration::from_millis(mpd.suggested_presentation_delay_milliseconds);
    let delayed_edge = now_on_timeline
        .checked_sub(delay)
        .ok_or(DashLiveRefreshError::EmptyAvailability)?;
    let window_start = match mpd.time_shift_buffer_depth_milliseconds {
        Some(depth) => now_on_timeline.saturating_sub(Duration::from_millis(depth)),
        None => Duration::ZERO,
    };
    let manifest_range = match (plan, selection) {
        (
            DashPresentationPlan::Single(component),
            DashLiveSelection::Evidence(DashPresentationSelection::Single { main }),
        ) => component_availability(
            mpd,
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
                mpd,
                video,
                video_selection,
                window_start,
                now_on_timeline,
                delayed_edge,
            )?;
            let audio = component_availability(
                mpd,
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
                mpd,
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
                    mpd,
                    video,
                    &video_lane.locations,
                    window_start,
                    now_on_timeline,
                    delayed_edge,
                )?;
                let audio = component_availability_from_locations(
                    mpd,
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
    Ok(DashLiveAvailability {
        live_edge: manifest_range.end,
        manifest_range,
    })
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
pub(super) fn validate_continuity(
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
            || current_period.duration != next_period.duration
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
