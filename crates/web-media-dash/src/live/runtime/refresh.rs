//! Последовательный MPD refresh и single-flight endpoint replacement.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dash_mpd_core::{DashMpdParseRequest, parse_dynamic_dash_mpd};
use source_core::CancellationToken;
use web_media_adaptive::{
    AdaptiveResourceFetchRequest, AdaptiveResourcePurpose, AdaptiveResourceQueryApplication,
    AdaptiveTransportError,
};

use super::{
    DashEndpointRefreshReply, DashLiveOpenError, DashLiveOpenRequest, DashLiveRuntimeFailure,
    DashLiveShared, DashSynchronizedClock,
};
use crate::live::{
    DashLiveRefreshOutcome, DashLiveSnapshot, build_dash_live_snapshot, refresh_dash_live_snapshot,
    replace_dash_live_endpoint_snapshot,
};

/// Refresh worker никогда не join-ится на player owner; cancellation обрывает loop.
pub(super) fn spawn_refresh_worker(
    request: DashLiveOpenRequest,
    shared: Arc<DashLiveShared>,
    fatal: Arc<Mutex<Option<DashLiveRuntimeFailure>>>,
) -> std::result::Result<(), DashLiveOpenError> {
    thread::Builder::new()
        .name("dash-live-refresh".to_owned())
        .spawn(move || run_refresh_loop(request, shared, &fatal))
        .map(|_| ())
        .map_err(DashLiveOpenError::RefreshWorkerSpawn)
}

/// Последовательно refresh-ит MPD; commit находится под одним snapshot mutex.
fn run_refresh_loop(
    request: DashLiveOpenRequest,
    shared: Arc<DashLiveShared>,
    fatal: &Mutex<Option<DashLiveRuntimeFailure>>,
) {
    let mut retry_not_before: Option<Instant> = None;
    loop {
        let (authoritative_deadline, cancellation) = match shared.state.lock() {
            Ok(state) => (
                state.accepted_refresh_deadline,
                state.http.cancellation().clone(),
            ),
            Err(_) => {
                set_fatal(fatal, DashLiveRuntimeFailure::Refresh);
                return;
            }
        };
        let attempt_deadline = retry_not_before.map_or(authoritative_deadline, |retry| {
            retry.max(authoritative_deadline)
        });
        if wait_for_cancellation_until(&cancellation, attempt_deadline) {
            return;
        }
        let fetch_started = Instant::now();
        match refresh_once(&request, &shared, fetch_started) {
            Ok(_) => {
                retry_not_before = next_retry_deadline(&shared, fetch_started);
            }
            Err(RefreshAttemptError::Cancelled) => {
                set_fatal(fatal, DashLiveRuntimeFailure::Cancelled);
                return;
            }
            Err(RefreshAttemptError::EndpointExpired) => {
                let previous_generation = match shared.state.lock() {
                    Ok(state) => state.generation,
                    Err(_) => {
                        set_fatal(fatal, DashLiveRuntimeFailure::Refresh);
                        return;
                    }
                };
                if let Err(error) = shared.recover_endpoint(previous_generation) {
                    let failure = match error {
                        AdaptiveTransportError::Cancelled => DashLiveRuntimeFailure::Cancelled,
                        _ => DashLiveRuntimeFailure::Refresh,
                    };
                    set_fatal(fatal, failure);
                    return;
                }
                retry_not_before = None;
            }
            Err(RefreshAttemptError::Fatal) => {
                set_fatal(fatal, DashLiveRuntimeFailure::Refresh);
                return;
            }
        }
    }
}

/// Внутренняя классификация: не раскрывает transport details в player error.
enum RefreshAttemptError {
    Cancelled,
    EndpointExpired,
    Fatal,
}

/// Полностью построенный snapshot до authoritative mutation.
struct StagedSnapshot {
    snapshot: DashLiveSnapshot,
    accepted_refresh_deadline: Instant,
}

/// Endpoint reply fetch/parse/build/continuity-валидируется до единого runtime commit-а.
pub(super) fn stage_and_commit_endpoint(
    request: &DashLiveOpenRequest,
    shared: &DashLiveShared,
    failed_generation: web_media_transport_api::SourceGeneration,
    reply: DashEndpointRefreshReply,
) -> std::result::Result<(), ()> {
    if reply.generation.value() <= failed_generation.value() {
        return Err(());
    }
    let fetch_started = Instant::now();
    let staged = fetch_snapshot(
        request,
        &reply.http,
        reply.generation,
        &reply.manifest,
        fetch_started,
    )
    .map_err(|_| ())?;

    let mut state = shared.state.lock().map_err(|_| ())?;
    if state.generation != failed_generation || reply.generation.value() <= state.generation.value()
    {
        return Err(());
    }
    let mut accepted_snapshot = state.snapshot.clone();
    if replace_dash_live_endpoint_snapshot(&mut accepted_snapshot, staged.snapshot)
        .map_err(|_| ())?
        != DashLiveRefreshOutcome::Replaced
    {
        return Err(());
    }
    let next_revision = state.revision.checked_add(1).ok_or(())?;
    shared
        .coordinator
        .replace_availability(accepted_snapshot.availability.clone())
        .map_err(|_| ())?;
    state.snapshot = accepted_snapshot;
    state.http = *reply.http;
    state.generation = reply.generation;
    state.manifest = reply.manifest;
    state.revision = next_revision;
    state.accepted_refresh_deadline = staged.accepted_refresh_deadline;
    Ok(())
}

/// Fetch/parse/build/validate нового snapshot-а до mutation.
fn refresh_once(
    request: &DashLiveOpenRequest,
    shared: &DashLiveShared,
    fetch_started: Instant,
) -> std::result::Result<DashLiveRefreshOutcome, RefreshAttemptError> {
    let (http, generation, manifest) = shared
        .state
        .lock()
        .map(|state| (state.http.clone(), state.generation, state.manifest.clone()))
        .map_err(|_| RefreshAttemptError::Fatal)?;
    let staged = fetch_snapshot(request, &http, generation, &manifest, fetch_started)?;
    let mut state = shared
        .state
        .lock()
        .map_err(|_| RefreshAttemptError::Fatal)?;
    if state.generation != generation {
        return Ok(DashLiveRefreshOutcome::StaleIgnored);
    }
    let mut accepted_snapshot = state.snapshot.clone();
    let outcome = refresh_dash_live_snapshot(&mut accepted_snapshot, staged.snapshot)
        .map_err(|_| RefreshAttemptError::Fatal)?;
    if outcome == DashLiveRefreshOutcome::Replaced {
        let next_revision = state
            .revision
            .checked_add(1)
            .ok_or(RefreshAttemptError::Fatal)?;
        shared
            .coordinator
            .replace_availability(accepted_snapshot.availability.clone())
            .map_err(|_| RefreshAttemptError::Fatal)?;
        state.snapshot = accepted_snapshot;
        state.revision = next_revision;
        state.accepted_refresh_deadline = staged.accepted_refresh_deadline;
    }
    Ok(outcome)
}

/// Выполняет network observation и pure snapshot build без shared-state mutation.
fn fetch_snapshot(
    request: &DashLiveOpenRequest,
    http: &web_media_adaptive::AdaptiveHttpContext,
    generation: web_media_transport_api::SourceGeneration,
    manifest: &crate::request::DashManifestInput,
    fetch_started: Instant,
) -> std::result::Result<StagedSnapshot, RefreshAttemptError> {
    let local_before_fetch = request.wall_clock.now_utc();
    let fetched = http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            generation,
            manifest.target.clone(),
            request.policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))
        .map_err(classify_refresh_transport)?;
    let local_after_fetch = request.wall_clock.now_utc();
    let mpd = parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: fetched.bytes(),
        xml_budgets: manifest.xml_budgets,
        limits: manifest.mpd_limits,
    })
    .map_err(|_| RefreshAttemptError::Fatal)?;
    let clock = DashSynchronizedClock::from_direct_utc(
        Arc::clone(&request.wall_clock),
        local_before_fetch,
        local_after_fetch,
        mpd.direct_utc_time,
    )
    .map_err(|_| RefreshAttemptError::Fatal)?;
    let snapshot = build_dash_live_snapshot(
        mpd,
        fetched.final_target(),
        &request.selection,
        request.policy.maximum_planned_segments,
        &clock,
    )
    .map_err(|_| RefreshAttemptError::Fatal)?;
    let accepted_refresh_deadline = refresh_deadline(
        fetch_started,
        snapshot.mpd.minimum_update_period_milliseconds,
    )
    .ok_or(RefreshAttemptError::Fatal)?;
    Ok(StagedSnapshot {
        snapshot,
        accepted_refresh_deadline,
    })
}

/// Только auth/not-found expiry запускает expensive re-extraction.
fn classify_refresh_transport(error: AdaptiveTransportError) -> RefreshAttemptError {
    match error {
        AdaptiveTransportError::Cancelled => RefreshAttemptError::Cancelled,
        other if matches!(other.http_status_code(), Some(401 | 403 | 404 | 410)) => {
            RefreshAttemptError::EndpointExpired
        }
        _ => RefreshAttemptError::Fatal,
    }
}

/// First fatal wins.
fn set_fatal(fatal: &Mutex<Option<DashLiveRuntimeFailure>>, failure: DashLiveRuntimeFailure) {
    if let Ok(mut slot) = fatal.lock()
        && slot.is_none()
    {
        *slot = Some(failure);
    }
}

/// Equal/stale fetch не двигает authoritative deadline, но сохраняет bounded polling cadence.
fn next_retry_deadline(shared: &DashLiveShared, fetch_started: Instant) -> Option<Instant> {
    let cadence = shared.state.lock().ok().map(|state| {
        Duration::from_millis(state.snapshot.mpd.minimum_update_period_milliseconds)
    })?;
    fetch_started.checked_add(cadence)
}

/// Authoritative MUP deadline всегда привязан к началу accepted fetch-а.
pub(super) fn refresh_deadline(fetch_started: Instant, mup_milliseconds: u64) -> Option<Instant> {
    fetch_started.checked_add(Duration::from_millis(mup_milliseconds))
}

/// Возвращает только остаток MUP; долгий fetch даёт немедленный следующий turn.
fn remaining_until(deadline: Instant, now: Instant) -> Duration {
    deadline
        .checked_duration_since(now)
        .unwrap_or(Duration::ZERO)
}

/// Делит potentially large wait до absolute deadline на bounded cancellation checks.
fn wait_for_cancellation_until(cancellation: &CancellationToken, deadline: Instant) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return true;
        }
        let remaining = remaining_until(deadline, Instant::now());
        if remaining.is_zero() {
            return false;
        }
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use source_core::CancellationToken;

    use super::{refresh_deadline, remaining_until, wait_for_cancellation_until};

    #[test]
    fn refresh_wait_observes_already_cancelled_shutdown_without_waiting_for_mup() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert!(wait_for_cancellation_until(
            &cancellation,
            std::time::Instant::now() + Duration::from_secs(60)
        ));
    }

    #[test]
    fn mup_deadline_waits_only_fetch_start_remainder_and_never_full_duration_again() {
        let fetch_started = std::time::Instant::now();
        let deadline = refresh_deadline(fetch_started, 10).expect("small deadline fits");

        assert_eq!(
            remaining_until(deadline, fetch_started + Duration::from_millis(4)),
            Duration::from_millis(6)
        );
        assert_eq!(
            remaining_until(deadline, fetch_started + Duration::from_millis(10)),
            Duration::ZERO
        );
        assert_eq!(
            remaining_until(deadline, fetch_started + Duration::from_millis(12)),
            Duration::ZERO
        );
    }
}
