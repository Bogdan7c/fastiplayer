//! Dynamic DASH demux, refresh worker и S31L evidence publication.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use dash_mpd_core::DashMediaKind;
use demux_api::ProgressiveDemuxer;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, MediaMetadata, MediaTime, TrackInfo,
};
use thiserror::Error;
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveTransportError};
use web_media_transport_api::SourceGeneration;

use super::{
    DashClockFetchObservation, DashLiveAvailability, DashLiveRefreshError, DashLiveSelection,
    DashLiveSnapshot, DashWallClock, resolve_dash_live_clock,
};
use crate::catalog::DashLogicalRepresentationSelection;
use crate::component::DashComponentFactory;
use crate::plan::{DashPlannedResource, DashPresentationContinuationPoint, DashPresentationPlan};
use crate::request::{DashManifestInput, DashVodOpenPolicy};
use crate::selection::DashPresentationSelection;
use crate::source::DashLiveTransportProvider;
use crate::transactional_av::TransactionalDashAvDemuxer;

mod open;
mod refresh;
mod replacement;
mod session_timeline;
mod timeline;
mod track_publication;

use open::{open_plan_continuation, prepare_dash_live_with_selection, remap_resource};
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
