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
use crate::plan::{
    DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPresentationPlan,
    build_dynamic_manifest_plan,
};
use crate::request::DashSerializedFragmentKind;
use crate::selection::{
    DashPresentationSelection, DashRepresentationEvidence, DashRepresentationSelectionError,
    select_representation,
};

// Blocking demux/refresh lifecycle живёт отдельно от pure timing math.
mod runtime;
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
    /// Manifest-only availability cap.
    pub availability: DashLiveAvailability,
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
    let plan = build_dynamic_manifest_plan(
        &mpd.presentation,
        manifest_base,
        selection,
        maximum_segments,
    )
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
    let manifest_range = match (&plan, selection) {
        (DashPresentationPlan::Single(component), DashPresentationSelection::Single { main }) => {
            component_availability(
                &mpd,
                component,
                main,
                window_start,
                now_on_timeline,
                delayed_edge,
            )?
        }
        (
            DashPresentationPlan::Separate { video, audio },
            DashPresentationSelection::Separate {
                video: video_selection,
                audio: audio_selection,
            },
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
        _ => return Err(DashLiveRefreshError::Continuity),
    };
    Ok(DashLiveSnapshot {
        mpd,
        plan,
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
            validate_continuity(&current.mpd, &next.mpd)?;
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
    validate_continuity(&current.mpd, &next.mpd)?;
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
    let mut earliest = None;
    let mut latest = None;
    for (period, period_plan) in mpd.presentation.periods.iter().zip(&component.periods) {
        let selected = select_representation(period, evidence)?;
        let offset = total_availability_offset_nanoseconds(
            mpd.presentation.base_url.as_ref(),
            period,
            selected.adaptation,
            selected.representation,
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
) -> Result<(), DashLiveRefreshError> {
    if current.availability_start_time != next.availability_start_time {
        return Err(DashLiveRefreshError::Continuity);
    }
    for current_period in &current.presentation.periods {
        let Some(period_id) = current_period.id.as_deref() else {
            return Err(DashLiveRefreshError::Continuity);
        };
        let Some(next_period) = next
            .presentation
            .periods
            .iter()
            .find(|period| period.id.as_deref() == Some(period_id))
        else {
            continue;
        };
        if current_period.start_milliseconds != next_period.start_milliseconds
            || current_period.duration_milliseconds != next_period.duration_milliseconds
            || !period_representation_contract_is_stable(current_period, next_period)
        {
            return Err(DashLiveRefreshError::Continuity);
        }
    }
    Ok(())
}

/// Проверяет ordered adaptation/Representation identity и immutable media shape.
fn period_representation_contract_is_stable(current: &DashPeriod, next: &DashPeriod) -> bool {
    current.adaptation_sets.len() == next.adaptation_sets.len()
        && current
            .adaptation_sets
            .iter()
            .zip(&next.adaptation_sets)
            .all(|(current_adaptation, next_adaptation)| {
                current_adaptation.id == next_adaptation.id
                    && current_adaptation.representations.len()
                        == next_adaptation.representations.len()
                    && current_adaptation
                        .representations
                        .iter()
                        .zip(&next_adaptation.representations)
                        .all(|(current_representation, next_representation)| {
                            representation_contract_is_stable(
                                current_representation,
                                next_representation,
                            )
                        })
            })
}

/// Endpoint references и sliding SegmentTimeline могут меняться; media shape — нет.
fn representation_contract_is_stable(
    current: &DashRepresentation,
    next: &DashRepresentation,
) -> bool {
    let template_timing = |representation: &DashRepresentation| match &representation.addressing {
        DashAddressing::Template(template) => Some((
            template.timescale,
            template.presentation_time_offset,
            template.duration,
        )),
        _ => None,
    };
    current.id == next.id
        && current.bandwidth == next.bandwidth
        && current.width == next.width
        && current.height == next.height
        && current.container == next.container
        && current.media_kind == next.media_kind
        && current.codecs == next.codecs
        && template_timing(current) == template_timing(next)
}

#[cfg(test)]
mod tests;
