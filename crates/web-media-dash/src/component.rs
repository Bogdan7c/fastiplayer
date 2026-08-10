//! Blocking multi-period component demuxer с fresh parser per Period/config epoch.

mod timestamp;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use dash_mpd_core::{DashContainer, DashMediaKind};
use demux_api::{DemuxContainerId, DemuxHints, DemuxInput, DemuxRegistry};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, MediaMetadata, MediaTime, Packet, PacketKeyframe, TrackId, TrackInfo, TrackKind,
};
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveRangeByteSource, AdaptiveRangeSourceConfig};
use web_media_transport_api::SourceGeneration;

use crate::plan::{
    DashComponentContinuationPoint, DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan,
};
use crate::request::{DashSerializedFragmentKind, DashVodOpenPolicy};
use crate::source::{DashLiveTransportProvider, DashOrderedSegmentSource};
use timestamp::{globalize_packet_timestamp, globalize_seek_result, timestamp_mapping_for_open};

/// Candidate fragment завершился без decode-safe video/audio anchor-а.
#[derive(Debug, thiserror::Error)]
#[error("DASH fragment sequence завершилась до decode-safe anchor")]
struct DashDecodeAnchorUnavailableError;

/// Initial demux topology не соответствует advertised Representation kind.
#[derive(Debug, thiserror::Error)]
#[error("DASH component track topology does not match its plan")]
pub(crate) struct DashComponentTrackShapeError;

/// Cloneable recipe для transactional offside replacement.
#[derive(Clone)]
pub(crate) struct DashComponentFactory {
    /// Immutable presentation plan.
    plan: DashComponentPlan,
    /// Shared S31 HTTP policy.
    http: AdaptiveHttpContext,
    /// Exact source generation.
    generation: SourceGeneration,
    /// Caller-owned bounds.
    policy: DashVodOpenPolicy,
    /// Injected neutral registry.
    registry: Arc<DemuxRegistry>,
    /// Optional dynamic endpoint owner.
    live_transport: Option<Arc<dyn DashLiveTransportProvider>>,
}

impl DashComponentFactory {
    /// Фиксирует dependencies без network side effects.
    pub(crate) fn new(
        plan: DashComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: DashVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
    ) -> Self {
        Self {
            plan,
            http,
            generation,
            policy,
            registry,
            live_transport: None,
        }
    }

    /// Фиксирует live endpoint owner для каждого init/media fetch-а.
    pub(crate) fn new_live(
        plan: DashComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: DashVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        live_transport: Arc<dyn DashLiveTransportProvider>,
    ) -> Self {
        Self {
            plan,
            http,
            generation,
            policy,
            registry,
            live_transport: Some(live_transport),
        }
    }

    /// Открывает initial component и доказывает required track/anchor readiness.
    pub(crate) fn open(&self) -> Result<DashComponentDemuxer> {
        DashComponentDemuxer::open(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            self.live_transport.clone(),
        )
    }

    /// Открывает первый fragment fresh snapshot-а после полностью consumed old plan-а.
    ///
    /// Это не seek: decoder сохраняет reference state, поэтому здесь запрещены
    /// decode-point scan, preroll replay и искусственный TracksChanged.
    pub(crate) fn open_continuation_after(
        &self,
        point: DashComponentContinuationPoint,
    ) -> Result<Option<DashComponentDemuxer>> {
        let Some((period_index, media_index)) = self.plan.first_media_after(point)? else {
            return Ok(None);
        };
        let replacement = DashComponentDemuxer::open_from_period(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            self.live_transport.clone(),
            period_index,
            media_index,
        )?;
        replacement.validate_required_track_shape()?;
        Ok(Some(replacement))
    }

    pub(crate) fn prepare_seek_replacement(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
    ) -> Result<(DashComponentDemuxer, DemuxSeekResult)> {
        let (period_index, local_target) = locate_period(&self.plan, request.timestamp)?;
        let period = &self.plan.periods[period_index];
        let media_index = media_index_for_target(period, local_target);
        match &period.input {
            DashPeriodInputPlan::Range { .. } => {
                let mut replacement = DashComponentDemuxer::open_from_period(
                    self.plan.clone(),
                    self.http.clone(),
                    self.generation,
                    self.policy,
                    Arc::clone(&self.registry),
                    self.live_transport.clone(),
                    period_index,
                    media_index,
                )?;
                replacement.public_tracks = stable_public_tracks.to_vec();
                let current_tracks = replacement.current.tracks().to_vec();
                replacement.refresh_track_mapping(&current_tracks)?;
                let inner_result = replacement.current.seek_with_request(DemuxSeekRequest {
                    timestamp: local_target,
                    mode: request.mode,
                })?;
                replacement.current_timestamp_origin = Some(Duration::ZERO);
                replacement
                    .replay_events
                    .push_back(replacement.tracks_changed_event());
                let result =
                    globalize_seek_result(inner_result, request.timestamp, period.timeline_start)?;
                Ok((replacement, result))
            }
            DashPeriodInputPlan::Ordered { .. } => self.prepare_ordered_seek_replacement(
                request,
                stable_public_tracks,
                period_index,
                media_index,
            ),
        }
    }

    fn prepare_ordered_seek_replacement(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
        period_index: usize,
        mut media_index: usize,
    ) -> Result<(DashComponentDemuxer, DemuxSeekResult)> {
        loop {
            let mut replacement = DashComponentDemuxer::open_from_period(
                self.plan.clone(),
                self.http.clone(),
                self.generation,
                self.policy,
                Arc::clone(&self.registry),
                self.live_transport.clone(),
                period_index,
                media_index,
            )?;
            replacement.public_tracks = stable_public_tracks.to_vec();
            let current_tracks = replacement.current.tracks().to_vec();
            replacement.refresh_track_mapping(&current_tracks)?;
            match replacement.prime_decode_anchor(request.timestamp) {
                Ok(result) => return Ok((replacement, result)),
                Err(source)
                    if source
                        .downcast_ref::<DashDecodeAnchorUnavailableError>()
                        .is_some()
                        && media_index > 0 =>
                {
                    media_index = media_index.saturating_sub(1);
                }
                Err(source) => return Err(source),
            }
        }
    }
}

/// Blocking component runtime; объект живёт только на media-open/progressive worker-е.
pub(crate) struct DashComponentDemuxer {
    /// Shared dependencies для self-contained seek replacement.
    factory: DashComponentFactory,
    /// Remaining Period indexes.
    remaining_periods: VecDeque<usize>,
    /// Current Period index.
    current_period_index: usize,
    /// Current fresh container parser.
    current: Box<dyn Demuxer + Send>,
    /// Global timestamp offset текущего parser input-а.
    current_timeline_start: Duration,
    /// Первый observed timestamp для segmented input normalization.
    current_timestamp_origin: Option<Duration>,
    /// Stable public tracks.
    public_tracks: Vec<TrackInfo>,
    /// Current inner→public TrackId map.
    track_mapping: Vec<(TrackId, TrackId)>,
    /// Presentation duration.
    duration: Duration,
    /// Latest bounded metadata.
    metadata: Option<MediaMetadata>,
    /// Events, сохранённые initial/seek preflight-ом.
    replay_events: VecDeque<DemuxReadEvent>,
}

impl DashComponentDemuxer {
    /// Открывает первый Period и доказывает required tracks + decode anchor.
    fn open(
        plan: DashComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: DashVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        live_transport: Option<Arc<dyn DashLiveTransportProvider>>,
    ) -> Result<Self> {
        let mut component = Self::open_from_period(
            plan,
            http,
            generation,
            policy,
            registry,
            live_transport,
            0,
            0,
        )?;
        component.validate_required_track_shape()?;
        if matches!(
            component.factory.plan.periods[0].input,
            DashPeriodInputPlan::Ordered { .. }
        ) {
            component.prime_decode_anchor(Duration::ZERO)?;
        }
        Ok(component)
    }

    /// Открывает fresh parser заданного Period-а и optional media fragment index-а.
    #[allow(clippy::too_many_arguments)]
    fn open_from_period(
        plan: DashComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: DashVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        live_transport: Option<Arc<dyn DashLiveTransportProvider>>,
        period_index: usize,
        first_media_index: usize,
    ) -> Result<Self> {
        let period = plan
            .periods
            .get(period_index)
            .ok_or_else(|| anyhow::anyhow!("DASH Period отсутствует в immutable plan"))?;
        let (current_timeline_start, current_timestamp_origin) =
            timestamp_mapping_for_open(period, first_media_index)?;
        let current = open_period(
            period,
            http.clone(),
            generation,
            policy,
            Arc::clone(&registry),
            live_transport
                .clone()
                .map(|provider| (provider, plan.media_kind)),
            first_media_index,
        )?;
        let duration = plan.duration;
        let public_tracks = current
            .tracks()
            .iter()
            .cloned()
            .map(|mut track| {
                track.duration = Some(duration);
                track
            })
            .collect::<Vec<_>>();
        let track_mapping = public_tracks
            .iter()
            .map(|track| (track.id, track.id))
            .collect();
        let metadata = current.media_metadata();
        let remaining_periods = ((period_index + 1)..plan.periods.len()).collect();
        let factory = match live_transport {
            Some(live_transport) => DashComponentFactory::new_live(
                plan,
                http,
                generation,
                policy,
                registry,
                live_transport,
            ),
            None => DashComponentFactory::new(plan, http, generation, policy, registry),
        };
        Ok(Self {
            factory,
            remaining_periods,
            current_period_index: period_index,
            current,
            current_timeline_start,
            current_timestamp_origin,
            public_tracks,
            track_mapping,
            duration,
            metadata,
            replay_events: VecDeque::new(),
        })
    }

    /// Проверяет published component topology до staged runtime publication.
    fn validate_required_track_shape(&self) -> Result<()> {
        let video_tracks = self
            .public_tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .count();
        let audio_tracks = self
            .public_tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .count();
        let valid = match self.factory.plan.media_kind {
            DashMediaKind::Video => video_tracks == 1 && audio_tracks == 0,
            DashMediaKind::Audio => video_tracks == 0 && audio_tracks == 1,
            DashMediaKind::Muxed => video_tracks == 1 && audio_tracks == 1,
        };
        if !valid {
            return Err(DashComponentTrackShapeError.into());
        }
        Ok(())
    }

    /// Сканирует fresh ordered parser до decode-safe video RAP либо первого audio packet-а.
    fn prime_decode_anchor(&mut self, requested: Duration) -> Result<DemuxSeekResult> {
        let requires_video_rap = self
            .public_tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video);
        let mut inspected_events = 0_usize;
        let mut inspected_bytes = 0_usize;
        loop {
            let event = self.read_next_inner_event()?;
            inspected_events = inspected_events.saturating_add(1);
            inspected_bytes = inspected_bytes.saturating_add(event_encoded_bytes(&event));
            self.ensure_scan_within_budget(inspected_events, inspected_bytes)?;
            match event {
                DemuxReadEvent::Packet(packet)
                    if (!requires_video_rap && packet.kind == TrackKind::Audio)
                        || (packet.kind == TrackKind::Video
                            && packet.keyframe == PacketKeyframe::Keyframe) =>
                {
                    let actual = packet.pts;
                    self.replay_events.push_back(self.tracks_changed_event());
                    self.replay_events.push_back(DemuxReadEvent::Packet(packet));
                    return Ok(DemuxSeekResult {
                        requested_position: MediaTime::from_duration(requested),
                        actual_position: MediaTime::from_duration(actual),
                        actual_track_timestamp: None,
                    });
                }
                DemuxReadEvent::EndOfStream => {
                    return Err(DashDecodeAnchorUnavailableError.into());
                }
                DemuxReadEvent::Packet(_)
                | DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_)
                | DemuxReadEvent::TemporarilyUnavailable(_) => {}
            }
        }
    }

    /// Применяет caller-owned event/byte budgets к initial и seek scans.
    fn ensure_scan_within_budget(
        &self,
        inspected_events: usize,
        inspected_bytes: usize,
    ) -> Result<()> {
        if inspected_events > self.factory.policy.maximum_seek_scan_events.get() {
            anyhow::bail!("DASH decode-anchor scan превысил event budget");
        }
        if inspected_bytes > self.factory.policy.maximum_seek_scan_bytes.get() {
            anyhow::bail!("DASH decode-anchor scan превысил encoded-byte budget");
        }
        Ok(())
    }

    /// Открывает следующий Period fresh parser-ом и публикует stable TracksChanged.
    fn open_next_period(&mut self) -> Result<bool> {
        let Some(period_index) = self.remaining_periods.pop_front() else {
            return Ok(false);
        };
        let period = &self.factory.plan.periods[period_index];
        let is_ordered = matches!(period.input, DashPeriodInputPlan::Ordered { .. });
        let (period_timeline_start, period_timestamp_origin) =
            timestamp_mapping_for_open(period, 0)?;
        let current = open_period(
            period,
            self.factory.http.clone(),
            self.factory.generation,
            self.factory.policy,
            Arc::clone(&self.factory.registry),
            self.factory
                .live_transport
                .clone()
                .map(|provider| (provider, self.factory.plan.media_kind)),
            0,
        )?;
        self.current_period_index = period_index;
        self.current = current;
        self.current_timeline_start = period_timeline_start;
        self.current_timestamp_origin = period_timestamp_origin;
        let current_tracks = self.current.tracks().to_vec();
        self.refresh_track_mapping(&current_tracks)?;
        self.validate_required_track_shape()?;
        if is_ordered {
            self.prime_decode_anchor(period_timeline_start)?;
        } else {
            self.replay_events.push_back(self.tracks_changed_event());
        }
        self.metadata = self
            .current
            .media_metadata()
            .or_else(|| self.metadata.clone());
        Ok(true)
    }

    /// Сохраняет stable public ids при config epoch/Period transition.
    fn refresh_track_mapping(&mut self, inner_tracks: &[TrackInfo]) -> Result<()> {
        if inner_tracks.len() != self.public_tracks.len()
            || inner_tracks
                .iter()
                .zip(&self.public_tracks)
                .any(|(inner, public)| inner.kind != public.kind)
        {
            anyhow::bail!("DASH Period изменил required component track topology");
        }
        self.track_mapping.clear();
        let mut refreshed = Vec::with_capacity(inner_tracks.len());
        for (inner, public) in inner_tracks.iter().zip(&self.public_tracks) {
            let mut track = inner.clone();
            self.track_mapping.push((inner.id, public.id));
            track.id = public.id;
            track.duration = Some(self.duration);
            refreshed.push(track);
        }
        self.public_tracks = refreshed;
        Ok(())
    }

    /// Переводит inner timestamps в monotonic public presentation timeline.
    fn remap_packet(&mut self, mut packet: Packet) -> Result<Packet> {
        packet.track_id = self
            .track_mapping
            .iter()
            .find_map(|(inner, public)| (*inner == packet.track_id).then_some(*public))
            .ok_or_else(|| anyhow::anyhow!("DASH packet имеет неизвестный track id"))?;
        let origin = *self
            .current_timestamp_origin
            .get_or_insert_with(|| packet.dts.map_or(packet.pts, |dts| packet.pts.min(dts)));
        packet.pts =
            globalize_packet_timestamp(packet.pts, origin, self.current_timeline_start, "PTS")?;
        packet.dts = packet
            .dts
            .map(|dts| globalize_packet_timestamp(dts, origin, self.current_timeline_start, "DTS"))
            .transpose()?;
        Ok(packet)
    }

    /// Создаёт stable public track update.
    fn tracks_changed_event(&self) -> DemuxReadEvent {
        DemuxReadEvent::TracksChanged(DemuxTrackListUpdate {
            tracks: self.public_tracks.clone(),
            duration: Some(self.duration),
        })
    }

    /// Читает current parser и выполняет fresh multi-period transition.
    fn read_next_inner_event(&mut self) -> Result<DemuxReadEvent> {
        match self.current.next_event()? {
            DemuxReadEvent::Packet(packet) => self.remap_packet(packet).map(DemuxReadEvent::Packet),
            DemuxReadEvent::EndOfStream if self.open_next_period()? => self
                .replay_events
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("DASH Period transition readiness отсутствует")),
            DemuxReadEvent::EndOfStream => Ok(DemuxReadEvent::EndOfStream),
            DemuxReadEvent::TracksChanged(update) => {
                self.refresh_track_mapping(&update.tracks)?;
                Ok(self.tracks_changed_event())
            }
            DemuxReadEvent::MediaMetadataChanged(metadata) => {
                self.metadata = Some(metadata.clone());
                Ok(DemuxReadEvent::MediaMetadataChanged(metadata))
            }
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                Ok(DemuxReadEvent::TemporarilyUnavailable(hint))
            }
        }
    }
}

impl Demuxer for DashComponentDemuxer {
    /// Возвращает stable public tracks.
    fn tracks(&self) -> &[TrackInfo] {
        &self.public_tracks
    }

    /// Static DASH всегда имеет finite duration.
    fn duration(&self) -> Option<Duration> {
        Some(self.duration)
    }

    /// Возвращает latest component metadata.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.metadata.clone()
    }

    /// Все admitted addressing modes имеют transactional seek implementation.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    /// Сначала replay-ит preflight/seek events, затем читает current parser.
    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        if let Some(event) = self.replay_events.pop_front() {
            return Ok(event);
        }
        self.read_next_inner_event()
    }

    /// Выполняет accurate seek через общий transactional path.
    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    /// Полностью готовит replacement до atomic `self` swap.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let (replacement, result) = self
            .factory
            .prepare_seek_replacement(request, &self.public_tracks)?;
        *self = replacement;
        Ok(result)
    }
}

/// Открывает addressing-specific input existing container factory-ой.
fn open_period(
    period: &DashComponentPeriodPlan,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: DashVodOpenPolicy,
    registry: Arc<DemuxRegistry>,
    live_transport: Option<(
        Arc<dyn DashLiveTransportProvider>,
        dash_mpd_core::DashMediaKind,
    )>,
    first_media_index: usize,
) -> Result<Box<dyn Demuxer + Send>> {
    let cancellation = http.cancellation().clone();
    let container = demux_container_id(period.container)?;
    match &period.input {
        DashPeriodInputPlan::Ordered {
            resources,
            query_application,
        } => {
            let source = match live_transport {
                Some((live_transport, media_kind)) => DashOrderedSegmentSource::new_live(
                    http,
                    generation,
                    resources,
                    *query_application,
                    policy.maximum_fragment_bytes,
                    first_media_index,
                    live_transport,
                    media_kind,
                    period.timeline_start,
                )?,
                None => DashOrderedSegmentSource::new(
                    http,
                    generation,
                    resources,
                    *query_application,
                    policy.maximum_fragment_bytes,
                    first_media_index,
                )?,
            };
            let demuxer = registry
                .open_required_container(
                    DemuxInput::ordered_segments(Box::new(source)),
                    DemuxHints::none(),
                    policy.demux_sniff_budget,
                    cancellation,
                    container,
                )
                .context("DASH ordered Period container sniff/open failed")?;
            Ok(demuxer)
        }
        DashPeriodInputPlan::Range {
            target,
            query_application,
        } => {
            if first_media_index != 0 {
                anyhow::bail!("DASH SegmentBase media index must remain zero");
            }
            let source = AdaptiveRangeByteSource::open(
                http,
                target.clone(),
                generation,
                AdaptiveRangeSourceConfig::new(policy.maximum_range_read_bytes, *query_application),
            )
            .context("DASH SegmentBase Range source probe failed")?;
            let demuxer = registry
                .open_required_container(
                    DemuxInput::byte_source(Box::new(source)),
                    DemuxHints::none(),
                    policy.demux_sniff_budget,
                    cancellation,
                    container,
                )
                .context("DASH SegmentBase container sniff/open failed")?;
            Ok(demuxer)
        }
    }
}

/// Возвращает exact existing S28 container identity.
fn demux_container_id(container: DashContainer) -> Result<DemuxContainerId> {
    let identity = match container {
        DashContainer::IsoBmff => "iso-bmff",
        DashContainer::WebM => "webm",
    };
    DemuxContainerId::new(identity).context("invalid static DASH container identity")
}

/// Находит Period, содержащий global target.
fn locate_period(plan: &DashComponentPlan, target: Duration) -> Result<(usize, Duration)> {
    let bounded_target = target.min(plan.duration);
    let period_index = plan
        .periods
        .iter()
        .enumerate()
        .rev()
        .find(|(_, period)| period.timeline_start <= bounded_target)
        .map(|(index, _)| index)
        .ok_or_else(|| anyhow::anyhow!("DASH seek target не принадлежит ни одному Period"))?;
    let period = &plan.periods[period_index];
    Ok((
        period_index,
        bounded_target
            .saturating_sub(period.timeline_start)
            .min(period.duration),
    ))
}

/// Находит media fragment at/before local target.
fn media_index_for_target(period: &DashComponentPeriodPlan, target: Duration) -> usize {
    match &period.input {
        DashPeriodInputPlan::Ordered { resources, .. } => resources
            .iter()
            .filter(|resource| resource.kind == DashSerializedFragmentKind::Media)
            .enumerate()
            .take_while(|(_, resource)| {
                resource.timeline_start.is_some_and(|start| start <= target)
            })
            .map(|(index, _)| index)
            .last()
            .unwrap_or(0),
        DashPeriodInputPlan::Range { .. } => 0,
    }
}

/// Считает encoded bytes одного scan event-а.
fn event_encoded_bytes(event: &DemuxReadEvent) -> usize {
    match event {
        DemuxReadEvent::Packet(packet) => packet.data.len(),
        DemuxReadEvent::EndOfStream
        | DemuxReadEvent::TracksChanged(_)
        | DemuxReadEvent::MediaMetadataChanged(_)
        | DemuxReadEvent::TemporarilyUnavailable(_) => 0,
    }
}
