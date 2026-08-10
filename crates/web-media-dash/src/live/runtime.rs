//! Dynamic DASH demux, refresh worker и S31L evidence publication.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dash_mpd_core::{DashMediaKind, DashMpdParseRequest, parse_dynamic_dash_mpd};
use demux_api::{ProgressiveDemuxer, ProgressiveRuntimeGeneration};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, MediaMetadata, MediaTime, TrackInfo,
};
use thiserror::Error;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_transport_api::SourceGeneration;

use super::{
    DashClockFetchObservation, DashLiveAvailability, DashLiveRefreshError, DashLiveSelection,
    DashLiveSnapshot, DashWallClock, build_dash_live_snapshot_with_selection,
    resolve_dash_live_clock,
};
use crate::catalog::DashLogicalRepresentationSelection;
use crate::component::DashComponentFactory;
use crate::plan::{
    DashComponentPlan, DashPeriodInputPlan, DashPlannedResource, DashPresentationContinuationPoint,
    DashPresentationPlan,
};
use crate::request::{DashManifestInput, DashVodOpenPolicy};
use crate::selection::DashPresentationSelection;
use crate::source::DashLiveTransportProvider;
use crate::transactional_av::TransactionalDashAvDemuxer;

mod refresh;
mod replacement;
mod session_timeline;
mod timeline;
mod track_publication;

use replacement::{DashLiveReadProgress, replacement_target_for_expired_reader};
use session_timeline::DashLiveSessionTimeline;
use timeline::DashLiveTimelineCoordinator;
use track_publication::DashLiveTrackPublication;

/// App-owned endpoint re-extraction request.
#[derive(Debug, Clone, Copy)]
pub struct DashEndpointRefreshRequest {
    /// Последняя принятая transport generation.
    pub previous_generation: SourceGeneration,
}

/// Fresh staged endpoint/material after semantic rematch, ещё не authoritative state.
pub struct DashEndpointRefreshReply {
    /// Fresh scoped HTTP context.
    pub http: Box<AdaptiveHttpContext>,
    /// Strictly newer transport generation.
    pub generation: SourceGeneration,
    /// Fresh MPD target и unchanged parser budgets.
    pub manifest: DashManifestInput,
}

impl std::fmt::Debug for DashEndpointRefreshReply {
    /// Не раскрывает endpoint.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashEndpointRefreshReply")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Secret-safe endpoint refresh failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DashEndpointRefreshError {
    /// Shared cancellation завершила recovery.
    #[error("DASH endpoint refresh cancelled")]
    Cancelled,
    /// App owner больше недоступен.
    #[error("DASH endpoint refresh owner disconnected")]
    OwnerDisconnected,
    /// Exact candidate не rematch-ится.
    #[error("DASH endpoint semantic rematch failed")]
    SemanticRematchFailed,
    /// Fresh candidate не входит в strict dynamic profile.
    #[error("DASH endpoint candidate is incompatible")]
    IncompatibleLiveCandidate,
    /// Bounded попытки исчерпаны.
    #[error("DASH endpoint refresh attempts exhausted")]
    AttemptsExhausted,
}

/// App composition boundary для fresh extraction/transport generation.
pub trait DashEndpointRefreshPort: Send + Sync {
    /// Возвращает staged endpoint без mutation app/runtime authoritative state.
    fn refresh(
        &self,
        request: DashEndpointRefreshRequest,
    ) -> std::result::Result<DashEndpointRefreshReply, DashEndpointRefreshError>;
}

/// Полный неустановленный dynamic open request.
#[derive(Clone)]
pub struct DashLiveOpenRequest {
    /// Initial manifest-scoped HTTP context.
    pub http: Box<AdaptiveHttpContext>,
    /// Initial transport generation.
    pub generation: SourceGeneration,
    /// Exact MPD target и parser bounds.
    pub manifest: DashManifestInput,
    /// Exact selected representations.
    pub selection: DashPresentationSelection,
    /// Injected existing container factories.
    pub demux_registry: Arc<demux_api::DemuxRegistry>,
    /// Explicit S34/S31 bounds.
    pub policy: DashVodOpenPolicy,
    /// Injected local clock.
    pub wall_clock: Arc<dyn DashWallClock>,
    /// Neutral port identity.
    pub timeline_port_generation: DynamicMediaTimelinePortGeneration,
    /// Initial provider epoch.
    pub initial_source_epoch: DynamicMediaTimelineEpoch,
    /// App-owned fresh endpoint recovery.
    pub endpoint_refresh: Arc<dyn DashEndpointRefreshPort>,
}

/// Неустановленный live result с receipted seek и S31L port.
pub struct DashLiveOpenResult {
    demuxer: ProgressiveDemuxer,
    timeline_port: DynamicMediaTimelinePort,
}

impl DashLiveOpenResult {
    /// Возвращает worker demuxer, seek handle и timeline port одной generation.
    pub fn into_parts(
        self,
    ) -> (
        ProgressiveDemuxer,
        Option<demux_api::ProgressiveAsyncSeekHandle>,
        DynamicMediaTimelinePort,
    ) {
        let seek = self.demuxer.async_seek_handle();
        (self.demuxer, seek, self.timeline_port)
    }
}

impl std::fmt::Debug for DashLiveOpenResult {
    /// Runtime internals и endpoint-ы не раскрываются.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashLiveOpenResult")
            .finish_non_exhaustive()
    }
}

/// Typed initial preparation failure.
#[derive(Debug, Error)]
pub enum DashLiveOpenError {
    /// Initial HTTP fetch.
    #[error("DASH live manifest fetch failed")]
    Transport(#[from] AdaptiveTransportError),
    /// Strict dynamic schema/profile.
    #[error("DASH live MPD rejected")]
    Manifest(#[from] dash_mpd_core::DashDynamicMpdError),
    /// Clock/planning availability.
    #[error("DASH live availability failed")]
    Availability(#[from] DashLiveRefreshError),
    /// Demux readiness/seek.
    #[error("DASH live demux readiness failed")]
    Runtime(#[from] anyhow::Error),
    /// Progressive worker spawn.
    #[error("DASH live progressive worker failed")]
    Progressive(#[from] demux_api::ProgressiveDemuxStartupError),
    /// Refresh thread spawn.
    #[error("DASH live refresh worker spawn failed")]
    RefreshWorkerSpawn(#[source] std::io::Error),
}

/// Atomic snapshot/transport owner shared by demux and refresh workers.
struct DashLiveShared {
    state: Mutex<DashLiveSharedState>,
    /// Единственный владелец source-native ↔ public session преобразования.
    session_timeline: DashLiveSessionTimeline,
    coordinator: Arc<DashLiveTimelineCoordinator>,
    endpoint_refresh: Arc<dyn DashEndpointRefreshPort>,
    endpoint_refresh_lock: Mutex<()>,
    refresh_request: DashLiveOpenRequest,
    refresh_selection: DashLiveSelection,
}

struct DashLiveSharedState {
    snapshot: DashLiveSnapshot,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    manifest: DashManifestInput,
    revision: u64,
    /// Deadline текущего authoritative MPD, измеренный от начала accepted fetch-а.
    accepted_refresh_deadline: Instant,
}

impl DashLiveShared {
    /// Валидирует staged manifest/transport и synchronously коммитит fresh plan.
    fn recover_endpoint(
        &self,
        failed_generation: SourceGeneration,
    ) -> std::result::Result<(), AdaptiveTransportError> {
        let _single_flight = self
            .endpoint_refresh_lock
            .lock()
            .map_err(|_| AdaptiveTransportError::WorkerStopped)?;
        let previous_generation = self
            .state
            .lock()
            .map_err(|_| AdaptiveTransportError::WorkerStopped)?
            .generation;
        if previous_generation != failed_generation {
            return Ok(());
        }
        let reply = self
            .endpoint_refresh
            .refresh(DashEndpointRefreshRequest {
                previous_generation,
            })
            .map_err(|error| match error {
                DashEndpointRefreshError::Cancelled => AdaptiveTransportError::Cancelled,
                _ => AdaptiveTransportError::WorkerStopped,
            })?;
        refresh::stage_and_commit_endpoint(
            &self.refresh_request,
            &self.refresh_selection,
            self,
            failed_generation,
            reply,
        )
        .map_err(|_| AdaptiveTransportError::WorkerStopped)
    }
}

impl DashLiveTransportProvider for DashLiveShared {
    /// Каждый resource fetch получает context/generation из одного lock snapshot-а.
    fn current_transport(
        &self,
    ) -> std::result::Result<(AdaptiveHttpContext, SourceGeneration), AdaptiveTransportError> {
        self.state
            .lock()
            .map(|state| (state.http.clone(), state.generation))
            .map_err(|_| AdaptiveTransportError::WorkerStopped)
    }

    /// Exact failed resource remap-ится только после fully accepted fresh MPD.
    fn recover_expired_resource(
        &self,
        failed_generation: SourceGeneration,
        media_kind: DashMediaKind,
        period_timeline_start: Duration,
        failed_resource: &DashPlannedResource,
    ) -> std::result::Result<DashPlannedResource, AdaptiveTransportError> {
        {
            let state = self
                .state
                .lock()
                .map_err(|_| AdaptiveTransportError::WorkerStopped)?;
            if let Some(replacement) = remap_resource(
                &state.snapshot.plan,
                media_kind,
                period_timeline_start,
                failed_resource,
            ) && (state.generation != failed_generation || replacement != *failed_resource)
            {
                return Ok(replacement);
            }
        }
        self.recover_endpoint(failed_generation)?;
        let state = self
            .state
            .lock()
            .map_err(|_| AdaptiveTransportError::WorkerStopped)?;
        remap_resource(
            &state.snapshot.plan,
            media_kind,
            period_timeline_start,
            failed_resource,
        )
        .ok_or(AdaptiveTransportError::WorkerStopped)
    }
}

/// Runtime-fatal refresh result observed by demux worker.
#[derive(Debug, Error)]
enum DashLiveRuntimeFailure {
    #[error("DASH live refresh failed")]
    Refresh,
    #[error("DASH live refresh cancelled")]
    Cancelled,
}

/// Live demux wrapper swaps only fully accepted newer snapshots.
struct DashLiveDemuxer {
    current: Box<dyn Demuxer + Send>,
    continuation_point: DashPresentationContinuationPoint,
    published_tracks: DashLiveTrackPublication,
    pending_track_update: Option<DemuxTrackListUpdate>,
    shared: Arc<DashLiveShared>,
    observed_revision: u64,
    last_packet_end: Option<MediaTime>,
    fatal: Arc<Mutex<Option<DashLiveRuntimeFailure>>>,
    policy: DashVodOpenPolicy,
    registry: Arc<demux_api::DemuxRegistry>,
}

impl DashLiveDemuxer {
    /// Проверяет refresh fatal без раскрытия endpoint.
    fn check_fatal(&self) -> Result<()> {
        let mut fatal = self
            .fatal
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live fatal mutex poisoned"))?;
        if let Some(failure) = fatal.take() {
            return Err(anyhow::Error::new(failure));
        }
        Ok(())
    }

    /// Готовит fresh snapshot сразу у source target-а, не открывая DVR head как посредника.
    fn prepare_snapshot_at(
        &self,
        plan: DashPresentationPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        request: DemuxSeekRequest,
    ) -> Result<(Box<dyn Demuxer + Send>, DemuxSeekResult)> {
        let stable_public_tracks = self.current.tracks().to_vec();
        let live_transport = Arc::clone(&self.shared) as Arc<dyn DashLiveTransportProvider>;
        match plan {
            DashPresentationPlan::Single(component) => {
                let factory = DashComponentFactory::new_live(
                    component,
                    http,
                    generation,
                    self.policy,
                    Arc::clone(&self.registry),
                    live_transport,
                );
                let (component, result) =
                    factory.prepare_seek_replacement(request, &stable_public_tracks)?;
                Ok((Box::new(component), result))
            }
            DashPresentationPlan::Separate { video, audio } => {
                let video_factory = DashComponentFactory::new_live(
                    video,
                    http.clone(),
                    generation,
                    self.policy,
                    Arc::clone(&self.registry),
                    Arc::clone(&live_transport),
                );
                let audio_factory = DashComponentFactory::new_live(
                    audio,
                    http,
                    generation,
                    self.policy,
                    Arc::clone(&self.registry),
                    live_transport,
                );
                let (demuxer, result) = TransactionalDashAvDemuxer::prepare_at(
                    video_factory,
                    audio_factory,
                    &stable_public_tracks,
                    request,
                    self.policy.composite_lead_policy,
                )?;
                Ok((Box::new(demuxer), result))
            }
        }
    }

    /// Открывает конкретный accepted snapshot и атомарно устанавливает его после seek.
    fn install_snapshot_at(
        &mut self,
        revision: u64,
        plan: DashPresentationPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        request: DemuxSeekRequest,
    ) -> Result<DemuxSeekResult> {
        let continuation_point = plan.continuation_point()?;
        let (replacement, result) = self.prepare_snapshot_at(plan, http, generation, request)?;
        let replacement_tracks =
            self.shared
                .session_timeline
                .track_list_update_to_session(DemuxTrackListUpdate::new(
                    replacement.tracks().to_vec(),
                    replacement.duration(),
                ));
        let pending_track_update = self.published_tracks.publish_if_changed(replacement_tracks);
        self.current = replacement;
        self.continuation_point = continuation_point;
        self.observed_revision = revision;
        if let Some(update) = pending_track_update {
            self.pending_track_update = Some(update);
        }
        Ok(result)
    }

    /// Открывает fully accepted snapshot через discontinuous seek/recovery path.
    fn replace_with_latest_at(
        &mut self,
        request: DemuxSeekRequest,
    ) -> Result<Option<DemuxSeekResult>> {
        let replacement_input = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?;
            (state.revision > self.observed_revision).then(|| {
                (
                    state.revision,
                    state.snapshot.plan.clone(),
                    state.http.clone(),
                    state.generation,
                )
            })
        };
        let Some((revision, plan, http, generation)) = replacement_input else {
            return Ok(None);
        };
        self.install_snapshot_at(revision, plan, http, generation, request)
            .map(Some)
    }

    /// Атомарно устанавливает только ещё не прочитанный suffix fresh snapshot-а.
    fn install_snapshot_continuation(
        &mut self,
        revision: u64,
        plan: DashPresentationPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
    ) -> Result<bool> {
        let next_continuation_point = plan.continuation_point()?;
        let Some(replacement) = open_plan_continuation(
            plan,
            self.continuation_point,
            http,
            generation,
            self.policy,
            Arc::clone(&self.registry),
            Arc::clone(&self.shared) as Arc<dyn DashLiveTransportProvider>,
        )?
        else {
            return Ok(false);
        };
        let replacement_tracks =
            self.shared
                .session_timeline
                .track_list_update_to_session(DemuxTrackListUpdate::new(
                    replacement.tracks().to_vec(),
                    replacement.duration(),
                ));
        let pending_track_update = self.published_tracks.publish_if_changed(replacement_tracks);
        self.current = replacement;
        self.continuation_point = next_continuation_point;
        self.observed_revision = revision;
        if let Some(update) = pending_track_update {
            self.pending_track_update = Some(update);
        }
        Ok(true)
    }

    /// Берёт accepted newer revision, но не превращает EOF continuation в seek.
    fn replace_with_latest_continuation(&mut self) -> Result<bool> {
        let replacement_input = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?;
            (state.revision > self.observed_revision).then(|| {
                (
                    state.revision,
                    state.snapshot.plan.clone(),
                    state.http.clone(),
                    state.generation,
                )
            })
        };
        let Some((revision, plan, http, generation)) = replacement_input else {
            return Ok(false);
        };
        self.install_snapshot_continuation(revision, plan, http, generation)
    }

    /// Explicit live seek всегда переоткрывает authoritative immutable plan.
    ///
    /// Component HTTP sources forward-only; попытка byte-seek уже читаемого
    /// source нарушила бы их контракт даже при неизменной MPD revision.
    fn reopen_authoritative_at(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let (revision, plan, http, generation) = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?;
            (
                state.revision,
                state.snapshot.plan.clone(),
                state.http.clone(),
                state.generation,
            )
        };
        self.install_snapshot_at(revision, plan, http, generation, request)
    }

    /// На EOF продолжает в пределах fresh manifest cap, даже если old edge уже expired.
    fn replace_after_refresh(&mut self) -> Result<bool> {
        self.replace_with_latest_continuation()
    }

    /// До чтения не даёт paused/stalled demux-у обратиться к уже истёкшему snapshot-у.
    ///
    /// Fresh revision сам по себе не повод переоткрывать active demuxer: пока
    /// последняя позиция остаётся в authoritative DVR window, старый immutable
    /// plan безопасно дочитывается до EOF. Replacement нужен только до первого
    /// packet-а либо когда sliding head уже обогнал последнюю прочитанную позицию.
    fn replace_if_current_position_expired(&mut self) -> Result<bool> {
        let replacement_target = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?;
            let progress = self.last_packet_end.map_or(
                DashLiveReadProgress::Unread,
                DashLiveReadProgress::LastPacketEnd,
            );
            replacement_target_for_expired_reader(
                self.observed_revision,
                state.revision,
                progress,
                &state.snapshot.availability,
            )
        };
        let Some(target) = replacement_target else {
            return Ok(false);
        };
        self.replace_with_latest_at(DemuxSeekRequest {
            timestamp: target.as_duration(),
            mode: media_core::DemuxSeekMode::DecodePointBefore,
        })
        .map(|result| result.is_some())
    }
}

impl Demuxer for DashLiveDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.published_tracks.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.current.media_metadata()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.current.seekability()
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        self.check_fatal()?;
        if let Some(update) = self.pending_track_update.take() {
            return Ok(DemuxReadEvent::TracksChanged(update));
        }
        if self.replace_if_current_position_expired()? {
            return Ok(DemuxReadEvent::TemporarilyUnavailable(
                self.policy.retry_hint,
            ));
        }
        match self.current.next_event()? {
            DemuxReadEvent::Packet(source_packet) => {
                let packet_end = MediaTime::from_duration(
                    source_packet
                        .duration
                        .and_then(|duration| source_packet.pts.checked_add(duration))
                        .unwrap_or(source_packet.pts),
                );
                self.last_packet_end =
                    Some(self.last_packet_end.map_or(packet_end, |last_packet_end| {
                        last_packet_end.max(packet_end)
                    }));
                let packet = self
                    .shared
                    .session_timeline
                    .packet_to_session(source_packet)?;
                self.shared.coordinator.observe_packet(&packet)?;
                Ok(DemuxReadEvent::Packet(packet))
            }
            DemuxReadEvent::TracksChanged(mut update) => {
                update = self
                    .shared
                    .session_timeline
                    .track_list_update_to_session(update);
                match self.published_tracks.publish_if_changed(update) {
                    Some(changed) => Ok(DemuxReadEvent::TracksChanged(changed)),
                    None => Ok(DemuxReadEvent::TemporarilyUnavailable(
                        self.policy.retry_hint,
                    )),
                }
            }
            DemuxReadEvent::EndOfStream if self.replace_after_refresh()? => Ok(
                DemuxReadEvent::TemporarilyUnavailable(self.policy.retry_hint),
            ),
            DemuxReadEvent::EndOfStream => Ok(DemuxReadEvent::TemporarilyUnavailable(
                self.policy.retry_hint,
            )),
            event => Ok(event),
        }
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest {
            timestamp,
            mode: media_core::DemuxSeekMode::Accurate,
        })
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        self.check_fatal()?;
        let source_request = self
            .shared
            .session_timeline
            .seek_request_to_source(request)?;
        let source_result = self.reopen_authoritative_at(source_request)?;
        self.last_packet_end = Some(source_result.actual_position);
        self.shared
            .session_timeline
            .seek_result_to_session(source_result, request.timestamp)
            .map_err(Into::into)
    }
}

/// Готовит initial runtime и запускает bounded refresh owner.
pub fn prepare_dash_live(
    request: DashLiveOpenRequest,
) -> std::result::Result<DashLiveOpenResult, DashLiveOpenError> {
    let selection = DashLiveSelection::Evidence(request.selection.clone());
    prepare_dash_live_with_selection(request, selection)
}

pub(crate) fn prepare_dash_live_logical(
    request: DashLiveOpenRequest,
    selection: DashLogicalRepresentationSelection,
) -> std::result::Result<DashLiveOpenResult, DashLiveOpenError> {
    prepare_dash_live_with_selection(request, DashLiveSelection::Logical(Box::new(selection)))
}

fn prepare_dash_live_with_selection(
    request: DashLiveOpenRequest,
    selection: DashLiveSelection,
) -> std::result::Result<DashLiveOpenResult, DashLiveOpenError> {
    let fetch_started = Instant::now();
    let local_before_fetch = request.wall_clock.now_utc();
    let fetched = request
        .http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            request.generation,
            request.manifest.target.clone(),
            request.policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))?;
    let local_after_fetch = request.wall_clock.now_utc();
    let mpd = parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: fetched.bytes(),
        xml_budgets: request.manifest.xml_budgets,
        limits: request.manifest.mpd_limits,
    })?;
    let clock = resolve_dash_live_clock(
        &mpd.utc_timing,
        fetched.final_target(),
        &request.http,
        request.generation,
        Arc::clone(&request.wall_clock),
        DashClockFetchObservation {
            local_before_fetch,
            local_after_fetch,
        },
    )
    .map_err(DashLiveRefreshError::Clock)?;
    let snapshot = build_dash_live_snapshot_with_selection(
        mpd,
        fetched.final_target(),
        &selection,
        request.policy.maximum_planned_segments,
        &clock,
    )?;
    let accepted_refresh_deadline = refresh::refresh_deadline(
        fetch_started,
        snapshot.mpd.minimum_update_period_milliseconds,
    )
    .ok_or_else(|| anyhow::anyhow!("DASH initial refresh deadline overflow"))?;
    let session_timeline =
        DashLiveSessionTimeline::from_initial_snapshot(&snapshot).map_err(anyhow::Error::new)?;
    let session_availability = session_timeline
        .availability_to_session(&snapshot.availability)
        .map_err(anyhow::Error::new)?;
    let has_video = selection_has_video(&selection);
    let has_audio = selection_has_audio(&selection);
    let (coordinator, timeline_port) = DashLiveTimelineCoordinator::new(
        session_availability,
        has_video,
        has_audio,
        request.timeline_port_generation,
        request.initial_source_epoch,
    )?;
    let cancellation = request.http.cancellation().clone();
    let refresh_request = request.clone();
    let shared = Arc::new(DashLiveShared {
        state: Mutex::new(DashLiveSharedState {
            snapshot,
            http: (*request.http).clone(),
            generation: request.generation,
            manifest: request.manifest.clone(),
            revision: 1,
            accepted_refresh_deadline,
        }),
        session_timeline,
        coordinator,
        endpoint_refresh: Arc::clone(&request.endpoint_refresh),
        endpoint_refresh_lock: Mutex::new(()),
        refresh_request: refresh_request.clone(),
        refresh_selection: selection.clone(),
    });
    // Open не наследует snapshot guard: source синхронно возвращается в
    // `current_transport()` и повторно берёт тот же mutex.
    let initial_plan = {
        shared
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?
            .snapshot
            .plan
            .clone()
    };
    let initial_continuation_point = initial_plan
        .continuation_point()
        .map_err(anyhow::Error::new)?;
    let mut current = open_plan(
        initial_plan,
        (*request.http).clone(),
        request.generation,
        request.policy,
        Arc::clone(&request.demux_registry),
        Some(Arc::clone(&shared) as Arc<dyn DashLiveTransportProvider>),
    )?;
    // Initial resource open мог принять fresh endpoint snapshot, поэтому edge
    // читается после open, но guard освобождается до re-entrant seek replacement.
    let initial_live_edge = {
        shared
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?
            .snapshot
            .availability
            .live_edge
            .as_duration()
    };
    current
        .seek_with_request(DemuxSeekRequest {
            timestamp: initial_live_edge,
            mode: media_core::DemuxSeekMode::DecodePointBefore,
        })
        .context("DASH live initial edge seek failed")?;
    let initial_tracks = session_timeline
        .track_list_update_to_session(DemuxTrackListUpdate::new(
            current.tracks().to_vec(),
            current.duration(),
        ))
        .tracks;
    let fatal = Arc::new(Mutex::new(None));
    let refresh_shared = Arc::clone(&shared);
    let refresh_fatal = Arc::clone(&fatal);
    let inner: Box<dyn Demuxer + Send> = Box::new(DashLiveDemuxer {
        current,
        continuation_point: initial_continuation_point,
        published_tracks: DashLiveTrackPublication::new(initial_tracks),
        pending_track_update: None,
        shared,
        observed_revision: 1,
        last_packet_end: None,
        fatal,
        policy: request.policy,
        registry: request.demux_registry,
    });
    let demuxer = ProgressiveDemuxer::new_receipted_seekable(
        inner,
        cancellation,
        request.policy.progressive_limits,
        request.policy.retry_hint,
        ProgressiveRuntimeGeneration::new(request.generation.value()),
        request.policy.asynchronous_seek_limits,
    )?;
    refresh::spawn_refresh_worker(refresh_request, selection, refresh_shared, refresh_fatal)?;
    Ok(DashLiveOpenResult {
        demuxer,
        timeline_port,
    })
}

/// Открывает immutable selected plan теми же S34 component factories.
fn open_plan(
    plan: DashPresentationPlan,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: DashVodOpenPolicy,
    registry: Arc<demux_api::DemuxRegistry>,
    live_transport: Option<Arc<dyn DashLiveTransportProvider>>,
) -> Result<Box<dyn Demuxer + Send>> {
    match plan {
        DashPresentationPlan::Single(component) => {
            let factory = DashComponentFactory::new_live(
                component,
                http,
                generation,
                policy,
                registry,
                live_transport.context("DASH live transport provider отсутствует")?,
            );
            Ok(Box::new(factory.open()?))
        }
        DashPresentationPlan::Separate { video, audio } => {
            let live_transport =
                live_transport.context("DASH live transport provider отсутствует")?;
            let video_factory = DashComponentFactory::new_live(
                video,
                http.clone(),
                generation,
                policy,
                Arc::clone(&registry),
                Arc::clone(&live_transport),
            );
            let audio_factory = DashComponentFactory::new_live(
                audio,
                http,
                generation,
                policy,
                registry,
                live_transport,
            );
            let video = video_factory.open()?;
            let audio = audio_factory.open()?;
            Ok(Box::new(TransactionalDashAvDemuxer::new(
                video_factory,
                audio_factory,
                video,
                audio,
                policy.composite_lead_policy,
            )?))
        }
    }
}

/// Открывает fresh plan с первого media fragment-а после consumed snapshot boundary.
///
/// В отличие от `open_plan` этот путь не ищет decode anchor и не выполняет seek:
/// decoder продолжает ту же elementary stream reference chain.
#[allow(clippy::too_many_arguments)]
fn open_plan_continuation(
    plan: DashPresentationPlan,
    point: DashPresentationContinuationPoint,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: DashVodOpenPolicy,
    registry: Arc<demux_api::DemuxRegistry>,
    live_transport: Arc<dyn DashLiveTransportProvider>,
) -> Result<Option<Box<dyn Demuxer + Send>>> {
    match (plan, point) {
        (
            DashPresentationPlan::Single(component),
            DashPresentationContinuationPoint::Single(component_point),
        ) => {
            let factory = DashComponentFactory::new_live(
                component,
                http,
                generation,
                policy,
                registry,
                live_transport,
            );
            Ok(factory
                .open_continuation_after(component_point)?
                .map(|component| Box::new(component) as Box<dyn Demuxer + Send>))
        }
        (
            DashPresentationPlan::Separate { video, audio },
            DashPresentationContinuationPoint::Separate {
                video: video_point,
                audio: audio_point,
            },
        ) => {
            let video_factory = DashComponentFactory::new_live(
                video,
                http.clone(),
                generation,
                policy,
                Arc::clone(&registry),
                Arc::clone(&live_transport),
            );
            let audio_factory = DashComponentFactory::new_live(
                audio,
                http,
                generation,
                policy,
                registry,
                live_transport,
            );
            let Some(video) = video_factory.open_continuation_after(video_point)? else {
                return Ok(None);
            };
            let Some(audio) = audio_factory.open_continuation_after(audio_point)? else {
                return Ok(None);
            };
            Ok(Some(Box::new(TransactionalDashAvDemuxer::new(
                video_factory,
                audio_factory,
                video,
                audio,
                policy.composite_lead_policy,
            )?)))
        }
        _ => anyhow::bail!("DASH live continuation plan shape changed across refresh"),
    }
}

/// Находит fresh URL того же component/Period/resource без сравнения secret target.
fn remap_resource(
    plan: &DashPresentationPlan,
    media_kind: DashMediaKind,
    period_timeline_start: Duration,
    failed_resource: &DashPlannedResource,
) -> Option<DashPlannedResource> {
    let component = match plan {
        DashPresentationPlan::Single(component) if component.media_kind == media_kind => component,
        DashPresentationPlan::Separate { video, .. } if media_kind == DashMediaKind::Video => video,
        DashPresentationPlan::Separate { audio, .. } if media_kind == DashMediaKind::Audio => audio,
        _ => return None,
    };
    remap_component_resource(component, period_timeline_start, failed_resource)
}

/// Resource identity включает timeline/role/range, но намеренно исключает endpoint.
fn remap_component_resource(
    component: &DashComponentPlan,
    period_timeline_start: Duration,
    failed_resource: &DashPlannedResource,
) -> Option<DashPlannedResource> {
    let period = component
        .periods
        .iter()
        .find(|period| period.timeline_start == period_timeline_start)?;
    let DashPeriodInputPlan::Ordered { resources, .. } = &period.input else {
        return None;
    };
    let mut matches = resources.iter().filter(|candidate| {
        candidate.kind == failed_resource.kind
            && candidate.byte_range == failed_resource.byte_range
            && candidate.timeline_start == failed_resource.timeline_start
            && candidate.duration == failed_resource.duration
    });
    let replacement = matches.next()?.clone();
    matches.next().is_none().then_some(replacement)
}

fn selection_has_video(selection: &DashLiveSelection) -> bool {
    match selection {
        DashLiveSelection::Evidence(DashPresentationSelection::Single { main }) => {
            matches!(main.media_kind, DashMediaKind::Video | DashMediaKind::Muxed)
        }
        DashLiveSelection::Evidence(DashPresentationSelection::Separate { .. }) => true,
        DashLiveSelection::Logical(selection) => match selection.as_ref() {
            DashLogicalRepresentationSelection::Single(lane) => matches!(
                lane.contract.kind,
                DashMediaKind::Video | DashMediaKind::Muxed
            ),
            DashLogicalRepresentationSelection::Separate { .. } => true,
        },
    }
}

fn selection_has_audio(selection: &DashLiveSelection) -> bool {
    match selection {
        DashLiveSelection::Evidence(DashPresentationSelection::Single { main }) => {
            matches!(main.media_kind, DashMediaKind::Audio | DashMediaKind::Muxed)
        }
        DashLiveSelection::Evidence(DashPresentationSelection::Separate { .. }) => true,
        DashLiveSelection::Logical(selection) => match selection.as_ref() {
            DashLogicalRepresentationSelection::Single(lane) => matches!(
                lane.contract.kind,
                DashMediaKind::Audio | DashMediaKind::Muxed
            ),
            DashLogicalRepresentationSelection::Separate { .. } => true,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dash_mpd_core::{DashContainer, DashMediaKind};
    use source_core::HttpRequestTarget;
    use web_media_adaptive::AdaptiveResourceQueryApplication;

    use super::{DashPresentationPlan, remap_resource};
    use crate::plan::{
        DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPeriodLifecycle,
        DashPlannedResource,
    };
    use crate::request::DashSerializedFragmentKind;

    /// Создаёт один media resource без отражения URL в assertion diagnostics.
    fn resource(target: &str) -> DashPlannedResource {
        DashPlannedResource {
            kind: DashSerializedFragmentKind::Media,
            target: HttpRequestTarget::parse_exact(target).expect("valid test target"),
            byte_range: None,
            timeline_start: Some(Duration::from_secs(4)),
            duration: Some(Duration::from_secs(2)),
        }
    }

    /// Формирует minimal strict ordered component plan.
    fn plan(resource: DashPlannedResource) -> DashPresentationPlan {
        DashPresentationPlan::Single(DashComponentPlan {
            media_kind: DashMediaKind::Video,
            periods: vec![DashComponentPeriodPlan {
                container: DashContainer::IsoBmff,
                timeline_start: Duration::from_secs(10),
                declared_lifecycle: DashPeriodLifecycle::Finite(Duration::from_secs(20)),
                duration: Duration::from_secs(20),
                timestamp_mapping: crate::plan::DashTimestampMapping::MediaTimeOrigin(
                    Duration::ZERO,
                ),
                input: DashPeriodInputPlan::Ordered {
                    resources: vec![resource],
                    query_application: AdaptiveResourceQueryApplication::MergeScopedAddition,
                },
            }],
            duration: Duration::from_secs(20),
        })
    }

    #[test]
    fn endpoint_remap_uses_component_period_and_timeline_identity_not_old_target() {
        let failed = resource("https://old.example.test/video/segment.m4s?token=old");
        let fresh = resource("https://fresh.example.test/new-path/segment.m4s?token=fresh");
        let replacement = remap_resource(
            &plan(fresh.clone()),
            DashMediaKind::Video,
            Duration::from_secs(10),
            &failed,
        )
        .expect("same semantic resource is remapped");

        assert!(replacement == fresh);
        assert!(
            remap_resource(
                &plan(resource("https://fresh.example.test/segment.m4s")),
                DashMediaKind::Video,
                Duration::from_secs(11),
                &failed,
            )
            .is_none()
        );
    }
}
