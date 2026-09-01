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

use dash_mpd_core::{DashDynamicMpd, DashUtcTimestamp};
use source_core::HttpRequestTarget;
use thiserror::Error;
use web_media_adaptive::AdaptiveTransportError;

use crate::DashPlanError;
use crate::catalog::{DashLogicalRepresentationSelection, rematch_logical_selection};
use crate::plan::{
    DashPresentationPlan, build_dynamic_manifest_plan, build_manifest_plan_from_logical_selection,
};
use crate::selection::{DashPresentationSelection, DashRepresentationSelectionError};

// Blocking clock/demux/refresh lifecycle живёт отдельно от pure timing math.
mod availability;
mod clock;
mod runtime;
pub use availability::DashLiveAvailability;
pub use clock::DashClockFetchObservation;
pub(crate) use clock::resolve_dash_live_clock;
pub use runtime::{
    DashEndpointRefreshError, DashEndpointRefreshPort, DashEndpointRefreshReply,
    DashEndpointRefreshRequest, DashFetchedLiveManifestInput, DashLiveOpenError,
    DashLiveOpenRequest, DashLiveOpenResult, prepare_dash_live,
};
pub(crate) use runtime::{
    DashLiveInitialManifest, DashLiveRuntimeOpenRequest, prepare_dash_live_logical,
};

/// Injected local wall clock; production implementation остаётся app-owned.
pub trait DashWallClock: Send + Sync {
    /// Возвращает текущий UTC estimate локальной системы.
    fn now_utc(&self) -> DashUtcTimestamp;
}

/// Clock synchronization failure без UTC payload.
#[derive(Debug, Error)]
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
    /// External clock URI reference нельзя безопасно разрешить.
    #[error("DASH UTC clock target resolution failed")]
    Target,
    /// Bounded HTTP XSDATE response не содержит допустимый timestamp.
    #[error("DASH UTC clock response is invalid")]
    InvalidResponse,
    /// Общий adaptive transport не смог получить external clock.
    #[error("DASH UTC clock transport failed")]
    Transport(#[from] AdaptiveTransportError),
}

impl DashLiveClockError {
    /// Refresh owner отличает cooperative cancellation от fatal clock failure.
    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Transport(AdaptiveTransportError::Cancelled))
    }
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
    /// Последний `r=-1` open Period-а не имеет finite next-start/Period end.
    OpenEndedTimelineRepeat,
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
    let availability = availability::calculate_availability(&mpd, &plan, &selection, clock)?;
    Ok(DashLiveSnapshot {
        mpd,
        plan,
        selection,
        availability,
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
        DashPlanError::OpenEndedTimelineRepeat => {
            DashLiveRefreshError::ProfileExcluded(DashLiveProfileExclusion::OpenEndedTimelineRepeat)
        }
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
            availability::validate_continuity(
                &current.mpd,
                &next.mpd,
                &current.selection,
                &next.selection,
            )?;
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
    availability::validate_continuity(
        &current.mpd,
        &next.mpd,
        &current.selection,
        &next.selection,
    )?;
    *current = next;
    Ok(DashLiveRefreshOutcome::Replaced)
}

#[cfg(test)]
mod tests;
