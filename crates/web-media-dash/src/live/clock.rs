//! Runtime-owned DASH UTC synchronization через общий adaptive HTTP boundary.

use std::sync::Arc;

use dash_mpd_core::{DashUtcTimestamp, DashUtcTiming};
use source_core::HttpRequestTarget;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
};
use web_media_transport_api::SourceGeneration;

use super::{DashLiveClockError, DashSynchronizedClock, DashWallClock};

/// Local clock observation вокруг уже выполненного MPD fetch-а.
#[derive(Clone, Copy)]
pub struct DashClockFetchObservation {
    /// Local UTC непосредственно перед запросом.
    pub(crate) local_before_fetch: DashUtcTimestamp,
    /// Local UTC сразу после получения bounded body.
    pub(crate) local_after_fetch: DashUtcTimestamp,
}

impl DashClockFetchObservation {
    /// Связывает direct-UTC sample с точными локальными границами root fetch-а.
    #[must_use]
    pub const fn new(
        local_before_fetch: DashUtcTimestamp,
        local_after_fetch: DashUtcTimestamp,
    ) -> Self {
        Self {
            local_before_fetch,
            local_after_fetch,
        }
    }
}

/// Разрешает pure timing descriptor, сохраняя сеть за runtime boundary.
pub(crate) fn resolve_dash_live_clock(
    timing: &DashUtcTiming,
    manifest_base: &HttpRequestTarget,
    http: &AdaptiveHttpContext,
    generation: SourceGeneration,
    wall_clock: Arc<dyn DashWallClock>,
    manifest_observation: DashClockFetchObservation,
) -> Result<DashSynchronizedClock, DashLiveClockError> {
    match timing {
        DashUtcTiming::Direct(timestamp) => DashSynchronizedClock::from_direct_utc(
            wall_clock,
            manifest_observation.local_before_fetch,
            manifest_observation.local_after_fetch,
            *timestamp,
        ),
        DashUtcTiming::HttpXsDate(resource) => {
            let clock_target = manifest_base
                .resolve_reference(resource.reference())
                .map_err(|_| DashLiveClockError::Target)?;
            let local_before_fetch = wall_clock.now_utc();
            let fetched_clock =
                http.fetch_resource_blocking(AdaptiveResourceFetchRequest::clock_synchronization(
                    generation,
                    clock_target,
                    http.maximum_resource_bytes(AdaptiveResourcePurpose::ClockSynchronization),
                ))?;
            let local_after_fetch = wall_clock.now_utc();
            let external_timestamp =
                DashUtcTimestamp::parse_xs_datetime_response(fetched_clock.bytes())
                    .map_err(|_| DashLiveClockError::InvalidResponse)?;
            DashSynchronizedClock::from_direct_utc(
                wall_clock,
                local_before_fetch,
                local_after_fetch,
                external_timestamp,
            )
        }
    }
}
