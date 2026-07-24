//! Dynamic DASH demux, refresh worker и S31L evidence publication.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dash_mpd_core::{DashMediaKind, DashMpdParseRequest, parse_dynamic_dash_mpd};
use demux_api::{ProgressiveDemuxer, ProgressiveRuntimeGeneration};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer,
    DynamicMediaTimelineEpoch, DynamicMediaTimelinePort, DynamicMediaTimelinePortGeneration,
    MediaMetadata, MediaTime, TrackInfo,
};
use thiserror::Error;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_transport_api::SourceGeneration;

use super::{
    DashLiveAvailability, DashLiveRefreshError, DashLiveSnapshot, DashSynchronizedClock,
    DashWallClock, build_dash_live_snapshot,
};
use crate::component::DashComponentFactory;
use crate::plan::{
    DashComponentPlan, DashPeriodInputPlan, DashPlannedResource, DashPresentationPlan,
};
use crate::request::{DashManifestInput, DashVodOpenPolicy};
use crate::selection::DashPresentationSelection;
use crate::source::DashLiveTransportProvider;
use crate::transactional_av::TransactionalDashAvDemuxer;

mod refresh;
mod timeline;

use timeline::DashLiveTimelineCoordinator;

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
    coordinator: Arc<DashLiveTimelineCoordinator>,
    endpoint_refresh: Arc<dyn DashEndpointRefreshPort>,
    endpoint_refresh_lock: Mutex<()>,
    refresh_request: DashLiveOpenRequest,
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
        refresh::stage_and_commit_endpoint(&self.refresh_request, self, failed_generation, reply)
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

    /// Открывает fully accepted snapshot и применяет seek до его публикации demux-у.
    fn replace_with_latest(
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
        let mut replacement = open_plan(
            plan,
            http,
            generation,
            self.policy,
            Arc::clone(&self.registry),
            Some(Arc::clone(&self.shared) as Arc<dyn DashLiveTransportProvider>),
        )?;
        let result = replacement.seek_with_request(request)?;
        self.current = replacement;
        self.observed_revision = revision;
        Ok(Some(result))
    }

    /// На EOF продолжает в пределах fresh manifest cap, даже если old edge уже expired.
    fn replace_after_refresh(&mut self) -> Result<bool> {
        let (window_start, live_edge) = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?;
            (
                state.snapshot.availability.manifest_range.start,
                state.snapshot.availability.live_edge,
            )
        };
        let target = self
            .last_packet_end
            .unwrap_or(live_edge)
            .max(window_start)
            .min(live_edge);
        self.replace_with_latest(DemuxSeekRequest {
            timestamp: target.as_duration(),
            mode: media_core::DemuxSeekMode::DecodePointBefore,
        })
        .map(|result| result.is_some())
    }
}

impl Demuxer for DashLiveDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.current.tracks()
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
        match self.current.next_event()? {
            DemuxReadEvent::Packet(packet) => {
                self.last_packet_end = Some(MediaTime::from_duration(
                    packet
                        .duration
                        .and_then(|duration| packet.pts.checked_add(duration))
                        .unwrap_or(packet.pts),
                ));
                self.shared.coordinator.observe_packet(&packet)?;
                Ok(DemuxReadEvent::Packet(packet))
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
        if let Some(result) = self.replace_with_latest(request)? {
            self.last_packet_end = Some(result.actual_position);
            return Ok(result);
        }
        let result = self.current.seek_with_request(request)?;
        self.last_packet_end = Some(result.actual_position);
        Ok(result)
    }
}

/// Готовит initial runtime и запускает bounded refresh owner.
pub fn prepare_dash_live(
    request: DashLiveOpenRequest,
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
    let clock = DashSynchronizedClock::from_direct_utc(
        Arc::clone(&request.wall_clock),
        local_before_fetch,
        local_after_fetch,
        mpd.direct_utc_time,
    )
    .map_err(DashLiveRefreshError::Clock)?;
    let snapshot = build_dash_live_snapshot(
        mpd,
        fetched.final_target(),
        &request.selection,
        request.policy.maximum_planned_segments,
        &clock,
    )?;
    let accepted_refresh_deadline = refresh::refresh_deadline(
        fetch_started,
        snapshot.mpd.minimum_update_period_milliseconds,
    )
    .ok_or_else(|| anyhow::anyhow!("DASH initial refresh deadline overflow"))?;
    let has_video = selection_has_video(&request.selection);
    let has_audio = selection_has_audio(&request.selection);
    let (coordinator, timeline_port) = DashLiveTimelineCoordinator::new(
        snapshot.availability.clone(),
        has_video,
        has_audio,
        request.timeline_port_generation,
        request.initial_source_epoch,
    );
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
        coordinator,
        endpoint_refresh: Arc::clone(&request.endpoint_refresh),
        endpoint_refresh_lock: Mutex::new(()),
        refresh_request: refresh_request.clone(),
    });
    let mut current = open_plan(
        shared
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?
            .snapshot
            .plan
            .clone(),
        (*request.http).clone(),
        request.generation,
        request.policy,
        Arc::clone(&request.demux_registry),
        Some(Arc::clone(&shared) as Arc<dyn DashLiveTransportProvider>),
    )?;
    current
        .seek_with_request(DemuxSeekRequest {
            timestamp: shared
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?
                .snapshot
                .availability
                .live_edge
                .as_duration(),
            mode: media_core::DemuxSeekMode::DecodePointBefore,
        })
        .context("DASH live initial edge seek failed")?;
    let fatal = Arc::new(Mutex::new(None));
    let refresh_shared = Arc::clone(&shared);
    let refresh_fatal = Arc::clone(&fatal);
    let inner: Box<dyn Demuxer + Send> = Box::new(DashLiveDemuxer {
        current,
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
    refresh::spawn_refresh_worker(refresh_request, refresh_shared, refresh_fatal)?;
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

fn selection_has_video(selection: &DashPresentationSelection) -> bool {
    match selection {
        DashPresentationSelection::Single { main } => {
            main.media_kind == dash_mpd_core::DashMediaKind::Video
        }
        DashPresentationSelection::Separate { .. } => true,
    }
}

fn selection_has_audio(selection: &DashPresentationSelection) -> bool {
    match selection {
        DashPresentationSelection::Single { main } => {
            main.media_kind == dash_mpd_core::DashMediaKind::Audio
        }
        DashPresentationSelection::Separate { .. } => true,
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
        DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPlannedResource,
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
