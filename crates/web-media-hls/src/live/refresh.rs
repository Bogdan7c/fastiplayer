use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use hls_playlist_core::HlsPlaylist;
use media_core::DynamicMediaTimelineEpoch;

use super::network_outage::{
    HlsLiveNetworkOutage, HlsLiveOutageDecision, transient_manifest_failure,
};
use super::open::{
    HlsLiveOpenError, SelectedLiveResources, fetch_manifest, load_selected_live, parse_playlist,
    validate_live_media,
};
use super::rejoin::{
    HlsLiveManifestFreshness, HlsLiveRefreshContinuation, manifest_freshness,
    plan_refresh_continuation,
};
use super::reload_schedule::{HlsLiveDueRenditions, HlsLiveReloadSchedules};
use super::{HlsLiveRejoinError, HlsLiveTimelineCoordinator, HlsLiveTransportSnapshot};
use crate::catalog::{HlsCatalogMatchMode, HlsCatalogReopenSelection};
use crate::open::HlsVodOpenError;
use crate::plan::build_segment_scoped_component_plan;
use crate::source::HlsRefreshableResourceKind;
use crate::{
    HlsEndpointRefreshError, HlsEndpointRefreshReason, HlsEndpointRefreshRequest,
    HlsLiveOpenRequest, HlsVodOpenRequest,
};

/// Secret-safe signal от segment/key source к единственному refresh owner-у.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HlsLiveEndpointExpirySignal {
    pub(super) generation: web_media_transport_api::SourceGeneration,
    pub(super) reason: HlsEndpointRefreshReason,
    pub(super) resource_kind: HlsRefreshableResourceKind,
}

#[derive(Default)]
struct HlsLiveRefreshControlState {
    shutdown: bool,
    pending_expiry: Option<HlsLiveEndpointExpirySignal>,
}

/// Общий wake/single-flight owner без locator-а и generic demux API.
pub(super) struct HlsLiveRefreshControl {
    state: Mutex<HlsLiveRefreshControlState>,
    wake: Condvar,
}

impl HlsLiveRefreshControl {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(HlsLiveRefreshControlState::default()),
            wake: Condvar::new(),
        })
    }

    pub(super) fn signal_expiry(&self, signal: HlsLiveEndpointExpirySignal) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let replace = state
            .pending_expiry
            .is_none_or(|pending| signal.generation.value() > pending.generation.value());
        if replace {
            state.pending_expiry = Some(signal);
        }
        self.wake.notify_all();
    }

    fn request_shutdown(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.shutdown = true;
        self.wake.notify_all();
    }

    fn wait_until(&self, deadline: Instant) -> HlsLiveRefreshWake {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if state.shutdown {
                return HlsLiveRefreshWake::Shutdown;
            }
            if let Some(signal) = state.pending_expiry.take() {
                return HlsLiveRefreshWake::EndpointExpiry(signal);
            }
            let now = Instant::now();
            let Some(wait) = deadline.checked_duration_since(now) else {
                return HlsLiveRefreshWake::Deadline;
            };
            let (next_state, _) = self
                .wake
                .wait_timeout(state, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next_state;
        }
    }
}

enum HlsLiveRefreshWake {
    Shutdown,
    EndpointExpiry(HlsLiveEndpointExpirySignal),
    Deadline,
}

/// Player-owned handle: Drop только отменяет и будит detached completion.
pub(super) struct HlsLiveRefreshOwner {
    control: Arc<HlsLiveRefreshControl>,
    cancellation: source_core::CancellationToken,
}

impl HlsLiveRefreshOwner {
    pub(super) fn spawn(
        request: HlsLiveOpenRequest,
        initial: SelectedLiveResources,
        coordinator: Arc<HlsLiveTimelineCoordinator>,
        fatal: Arc<Mutex<Option<HlsLiveRuntimeFailure>>>,
        control: Arc<HlsLiveRefreshControl>,
        catalog_selection: Option<HlsCatalogReopenSelection>,
    ) -> Result<Self, HlsLiveOpenError> {
        let worker_control = Arc::clone(&control);
        let cancellation = request.common.http.cancellation().clone();
        thread::Builder::new()
            .name("hls-live-refresh".to_owned())
            .spawn(move || {
                run_refresh_loop(
                    request,
                    initial,
                    coordinator,
                    worker_control,
                    &fatal,
                    catalog_selection,
                );
            })
            .map_err(HlsLiveOpenError::RefreshWorkerSpawn)?;
        Ok(Self {
            control,
            cancellation,
        })
    }
}

impl Drop for HlsLiveRefreshOwner {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.control.request_shutdown();
    }
}

/// Terminal refresh failure, который demux surface публикует без locator/secrets.
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub(super) enum HlsLiveRuntimeFailure {
    #[error("manifest refresh failed")]
    ManifestRefresh,
    #[error("endpoint refresh failed: {0}")]
    EndpointRefresh(#[source] HlsEndpointRefreshError),
    #[error("live refresh continuity failed")]
    Continuity,
    #[error("refresh owner synchronization failed")]
    Synchronization,
    #[error("live refresh cancelled")]
    Cancelled,
}

fn run_refresh_loop(
    mut request: HlsLiveOpenRequest,
    mut current: SelectedLiveResources,
    coordinator: Arc<HlsLiveTimelineCoordinator>,
    control: Arc<HlsLiveRefreshControl>,
    fatal: &Mutex<Option<HlsLiveRuntimeFailure>>,
    catalog_selection: Option<HlsCatalogReopenSelection>,
) {
    let Some(mut schedules) = HlsLiveReloadSchedules::initial(Instant::now(), &current) else {
        set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
        return;
    };
    // Серия сетевых сбоев плейлиста: пока бюджет не исчерпан, ждём сеть (решение 334).
    let mut manifest_outage = HlsLiveNetworkOutage::new(request.network_reconnect);
    loop {
        let Some(deadline) = schedules.next_deadline() else {
            return;
        };
        match control.wait_until(deadline) {
            HlsLiveRefreshWake::Shutdown => return,
            HlsLiveRefreshWake::EndpointExpiry(signal) => {
                let _resource_kind = signal.resource_kind;
                if signal.generation != request.common.generation {
                    continue;
                }
                let Some(next) = replace_expired_endpoint(
                    &mut request,
                    &coordinator,
                    fatal,
                    signal.reason,
                    catalog_selection.as_ref(),
                ) else {
                    return;
                };
                current = next;
                let Some(next_schedules) =
                    HlsLiveReloadSchedules::initial(Instant::now(), &current)
                else {
                    set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
                    return;
                };
                schedules = next_schedules;
            }
            HlsLiveRefreshWake::Deadline => {
                let poll_started = Instant::now();
                let due = schedules.due(poll_started);
                if !due.main && !due.audio {
                    continue;
                }
                let attempt = match refresh_selected_media(&request.common, &current, due) {
                    Ok(attempt) => {
                        manifest_outage.record_recovery();
                        attempt
                    }
                    Err(error)
                        if matches!(
                            &error,
                            HlsLiveOpenError::Transport(
                                web_media_adaptive::AdaptiveTransportError::Cancelled
                            )
                        ) =>
                    {
                        set_fatal(fatal, HlsLiveRuntimeFailure::Cancelled);
                        return;
                    }
                    Err(error) => {
                        let Some(reason) = endpoint_refresh_reason(&error) else {
                            if let Some(retry_after) = transient_manifest_failure(&error)
                                && let HlsLiveOutageDecision::RetryAt(retry_at) =
                                    manifest_outage.record_failure(Instant::now(), retry_after)
                            {
                                schedules.postpone_due_until(poll_started, retry_at);
                                continue;
                            }
                            set_fatal(fatal, HlsLiveRuntimeFailure::ManifestRefresh);
                            return;
                        };
                        let Some(next) = replace_expired_endpoint(
                            &mut request,
                            &coordinator,
                            fatal,
                            reason,
                            catalog_selection.as_ref(),
                        ) else {
                            return;
                        };
                        current = next;
                        let Some(next_schedules) =
                            HlsLiveReloadSchedules::initial(Instant::now(), &current)
                        else {
                            set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
                            return;
                        };
                        schedules = next_schedules;
                        continue;
                    }
                };
                match commit_refreshed_resources(
                    &coordinator,
                    &attempt.next,
                    attempt.freshness(),
                    request.initial_source_epoch,
                    None,
                    fatal,
                ) {
                    HlsLiveRefreshCommit::Committed => {}
                    HlsLiveRefreshCommit::Failed => return,
                    HlsLiveRefreshCommit::NeedsFreshManifests => {
                        // Устаревший плейлист соседа нельзя брать для склейки: сразу
                        // качаем все заново, следующая попытка будет полностью свежей.
                        schedules.make_every_rendition_due(poll_started);
                        continue;
                    }
                }
                if !schedules.record_reload(poll_started, &attempt) {
                    set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
                    return;
                }
                current = attempt.next;
            }
        }
    }
}

/// Одна попытка обновления: что скачано и изменилось ли.
pub(super) struct HlsLiveRefreshAttempt {
    pub(super) next: SelectedLiveResources,
    pub(super) due: HlsLiveDueRenditions,
    pub(super) main_changed: bool,
    pub(super) audio_changed: bool,
}

impl HlsLiveRefreshAttempt {
    /// Все ли выбранные плейлисты свежие в этой попытке (см. `rejoin::manifest_freshness`).
    fn freshness(&self) -> HlsLiveManifestFreshness {
        manifest_freshness(&self.next, self.due)
    }
}

fn replace_expired_endpoint(
    request: &mut HlsLiveOpenRequest,
    coordinator: &HlsLiveTimelineCoordinator,
    fatal: &Mutex<Option<HlsLiveRuntimeFailure>>,
    reason: HlsEndpointRefreshReason,
    catalog_selection: Option<&HlsCatalogReopenSelection>,
) -> Option<SelectedLiveResources> {
    let reply = match request.endpoint_refresh.refresh(HlsEndpointRefreshRequest {
        previous_generation: request.common.generation,
        reason,
    }) {
        Ok(reply) => reply,
        Err(error) => {
            set_fatal(fatal, HlsLiveRuntimeFailure::EndpointRefresh(error));
            return None;
        }
    };
    let replacement_common = HlsVodOpenRequest {
        http: reply.http,
        generation: reply.generation,
        manifest: reply.manifest,
        selection: request.common.selection.clone(),
        overrides: reply.overrides,
        containers: request.common.containers,
        demux_registry: Arc::clone(&request.common.demux_registry),
        policy: request.common.policy,
    };
    let next = match load_selected_live(
        &replacement_common,
        true,
        catalog_selection,
        HlsCatalogMatchMode::Semantic,
    ) {
        Ok(next) => next,
        Err(_) => {
            set_fatal(
                fatal,
                HlsLiveRuntimeFailure::EndpointRefresh(
                    HlsEndpointRefreshError::IncompatibleLiveCandidate,
                ),
            );
            return None;
        }
    };
    let Some(next_epoch_value) = request.initial_source_epoch.get().checked_add(1) else {
        set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
        return None;
    };
    let next_epoch = DynamicMediaTimelineEpoch::new(next_epoch_value);
    // Замена ссылки скачала все плейлисты заново, поэтому они свежие.
    let commit = commit_refreshed_resources(
        coordinator,
        &next,
        HlsLiveManifestFreshness::AllFetched,
        next_epoch,
        Some(HlsLiveTransportSnapshot {
            http: replacement_common.http.clone(),
            generation: replacement_common.generation,
        }),
        fatal,
    );
    if commit != HlsLiveRefreshCommit::Committed {
        // `NeedsFreshManifests` при всех свежих плейлистах не возникает; если всё же
        // возник — это нарушение собственного инварианта, а не сбой сети.
        if commit == HlsLiveRefreshCommit::NeedsFreshManifests {
            set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
        }
        return None;
    }
    request.common = replacement_common;
    request.initial_source_epoch = next_epoch;
    Some(next)
}

/// Итог применения одной попытки обновления к координатору.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HlsLiveRefreshCommit {
    /// Новое окно на шкале: обычное скольжение или склейка.
    Committed,
    /// Нить потеряна, а часть плейлистов в попытке устарела: перекачать все и решить снова.
    NeedsFreshManifests,
    /// Роковая ошибка уже записана в `fatal`.
    Failed,
}

/// Применяет скачанные плейлисты: продолжает окно, выравнивает по соседу или склеивает.
///
/// Склеивается только потеря нити (`RefreshGap`, решение 333); остальные ошибки
/// преемственности остаются роковыми, как раньше.
fn commit_refreshed_resources(
    coordinator: &HlsLiveTimelineCoordinator,
    next: &SelectedLiveResources,
    freshness: HlsLiveManifestFreshness,
    source_epoch: DynamicMediaTimelineEpoch,
    replacement_transport: Option<HlsLiveTransportSnapshot>,
    fatal: &Mutex<Option<HlsLiveRuntimeFailure>>,
) -> HlsLiveRefreshCommit {
    let (Ok(previous_main), Ok(previous_audio)) =
        (coordinator.main_snapshot(), coordinator.audio_snapshot())
    else {
        set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
        return HlsLiveRefreshCommit::Failed;
    };
    let continuation =
        match plan_refresh_continuation(&previous_main, previous_audio.as_ref(), next, freshness) {
            Ok(continuation) => continuation,
            Err(_) => {
                set_fatal(fatal, HlsLiveRuntimeFailure::Continuity);
                return HlsLiveRefreshCommit::Failed;
            }
        };
    match continuation {
        HlsLiveRefreshContinuation::NeedsFreshManifests => {
            HlsLiveRefreshCommit::NeedsFreshManifests
        }
        HlsLiveRefreshContinuation::Continued { main, audio } => {
            if coordinator
                .replace_snapshots(main, audio, source_epoch, replacement_transport)
                .is_err()
            {
                set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
                return HlsLiveRefreshCommit::Failed;
            }
            HlsLiveRefreshCommit::Committed
        }
        HlsLiveRefreshContinuation::LostThread => {
            match coordinator.rejoin_live_edge(next, source_epoch, replacement_transport) {
                Ok(outcome) => {
                    log::info!(
                        "HLS live lost its place in the playlist window and rejoined near the live edge: resume_at={:?} join={:?} edge {:?} -> {:?}",
                        outcome.resume_at,
                        outcome.join_position,
                        outcome.previous_live_edge,
                        outcome.live_edge,
                    );
                    HlsLiveRefreshCommit::Committed
                }
                Err(HlsLiveRejoinError::Rejoin(_)) => {
                    set_fatal(fatal, HlsLiveRuntimeFailure::Continuity);
                    HlsLiveRefreshCommit::Failed
                }
                Err(HlsLiveRejoinError::Coordinator(_)) => {
                    set_fatal(fatal, HlsLiveRuntimeFailure::Synchronization);
                    HlsLiveRefreshCommit::Failed
                }
            }
        }
    }
}

fn refresh_selected_media(
    request: &HlsVodOpenRequest,
    current: &SelectedLiveResources,
    due: HlsLiveDueRenditions,
) -> Result<HlsLiveRefreshAttempt, HlsLiveOpenError> {
    let (main_media, main_plan, main_changed) =
        if due.main {
            let main_resource = fetch_manifest(
                &request.http,
                request.generation,
                current.main_reload_target.clone(),
            )?;
            let HlsPlaylist::Media(main_media) = parse_playlist(&main_resource, request)? else {
                return Err(HlsVodOpenError::NestedMasterPlaylist.into());
            };
            validate_live_media(&main_media, current.main_container, true)?;
            let main_plan = build_segment_scoped_component_plan(
                &main_media,
                current.main_container,
                main_resource.final_target(),
                &request.overrides,
            )?;
            main_plan.validate_resource_bound(request.http.maximum_resource_bytes(
                web_media_adaptive::AdaptiveResourcePurpose::MediaSegment,
            ))?;
            let changed = main_media != current.main_media;
            (main_media, main_plan, changed)
        } else {
            (current.main_media.clone(), current.main_plan.clone(), false)
        };
    let (audio_media, audio_plan, audio_changed) = match (
        current.audio_reload_target.as_ref(),
        current.audio_container,
    ) {
        (Some(target), Some(container)) if due.audio => {
            let resource = fetch_manifest(&request.http, request.generation, target.clone())?;
            let HlsPlaylist::Media(media) = parse_playlist(&resource, request)? else {
                return Err(HlsVodOpenError::NestedMasterPlaylist.into());
            };
            validate_live_media(&media, container, true)?;
            let plan = build_segment_scoped_component_plan(
                &media,
                container,
                resource.final_target(),
                &request.overrides,
            )?;
            plan.validate_resource_bound(request.http.maximum_resource_bytes(
                web_media_adaptive::AdaptiveResourcePurpose::MediaSegment,
            ))?;
            let changed = current
                .audio_media
                .as_ref()
                .is_none_or(|previous| previous != &media);
            (Some(media), Some(plan), changed)
        }
        (Some(_), Some(_)) => (
            current.audio_media.clone(),
            current.audio_plan.clone(),
            false,
        ),
        (None, None) if !due.audio => (None, None, false),
        (None, None) => {
            return Err(HlsLiveOpenError::Runtime(anyhow::anyhow!(
                "HLS live refresh requested absent audio rendition"
            )));
        }
        _ => {
            return Err(HlsLiveOpenError::Runtime(anyhow::anyhow!(
                "HLS live refresh audio pairing invalid"
            )));
        }
    };
    Ok(HlsLiveRefreshAttempt {
        next: SelectedLiveResources {
            main_media,
            main_plan,
            main_reload_target: current.main_reload_target.clone(),
            main_container: current.main_container,
            main_track_layout: current.main_track_layout,
            audio_media,
            audio_plan,
            audio_reload_target: current.audio_reload_target.clone(),
            audio_container: current.audio_container,
            subtitles: current.subtitles.clone(),
            subtitle_base: current.subtitle_base.clone(),
        },
        due,
        main_changed,
        audio_changed,
    })
}

fn endpoint_refresh_reason(error: &HlsLiveOpenError) -> Option<HlsEndpointRefreshReason> {
    let HlsLiveOpenError::Transport(error) = error else {
        return None;
    };
    error
        .http_status_code()
        .and_then(HlsEndpointRefreshReason::from_http_status)
}

fn set_fatal(fatal: &Mutex<Option<HlsLiveRuntimeFailure>>, failure: HlsLiveRuntimeFailure) {
    *fatal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(failure);
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use web_media_transport_api::SourceGeneration;

    use super::*;

    #[test]
    fn owner_drop_is_bounded_while_detached_refresh_operation_is_blocked() {
        let control = HlsLiveRefreshControl::new();
        let cancellation = source_core::CancellationToken::new();
        let owner = HlsLiveRefreshOwner {
            control: Arc::clone(&control),
            cancellation: cancellation.clone(),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).expect("test receiver is alive");
            release_rx.recv().expect("test releases blocked operation");
        });
        started_rx.recv().expect("blocked operation started");

        let drop_started = Instant::now();
        drop(owner);
        assert!(drop_started.elapsed() < Duration::from_millis(100));
        assert!(cancellation.is_cancelled());
        assert!(matches!(
            control.wait_until(Instant::now() + Duration::from_secs(30)),
            HlsLiveRefreshWake::Shutdown
        ));

        release_tx.send(()).expect("release blocked operation");
        worker.join().expect("test worker exits cleanly");
    }

    #[test]
    fn expiry_signal_is_single_flight_and_new_generation_supersedes_stale() {
        let control = HlsLiveRefreshControl::new();
        control.signal_expiry(HlsLiveEndpointExpirySignal {
            generation: SourceGeneration::new(4),
            reason: HlsEndpointRefreshReason::ResourceExpired,
            resource_kind: HlsRefreshableResourceKind::MediaOrInitialization,
        });
        control.signal_expiry(HlsLiveEndpointExpirySignal {
            generation: SourceGeneration::new(4),
            reason: HlsEndpointRefreshReason::AuthorizationExpired,
            resource_kind: HlsRefreshableResourceKind::EncryptionKey,
        });
        let HlsLiveRefreshWake::EndpointExpiry(first) =
            control.wait_until(Instant::now() + Duration::from_secs(1))
        else {
            panic!("expected first expiry");
        };
        assert_eq!(first.generation, SourceGeneration::new(4));
        assert_eq!(
            first.resource_kind,
            HlsRefreshableResourceKind::MediaOrInitialization
        );

        control.signal_expiry(HlsLiveEndpointExpirySignal {
            generation: SourceGeneration::new(5),
            reason: HlsEndpointRefreshReason::AuthorizationExpired,
            resource_kind: HlsRefreshableResourceKind::EncryptionKey,
        });
        let HlsLiveRefreshWake::EndpointExpiry(next) =
            control.wait_until(Instant::now() + Duration::from_secs(1))
        else {
            panic!("expected next expiry");
        };
        assert_eq!(next.generation, SourceGeneration::new(5));
        assert_eq!(
            next.resource_kind,
            HlsRefreshableResourceKind::EncryptionKey
        );
    }
}
