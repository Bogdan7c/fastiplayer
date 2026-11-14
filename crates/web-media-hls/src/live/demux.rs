use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use demux_api::DemuxRegistry;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, MediaMetadata, MediaTime, Packet, PacketDecodeStartInitialization, PacketKeyframe,
    TrackId, TrackInfo, TrackKind,
};
use source_core::HttpReconnectPolicy;
use web_media_transport_api::SourceGeneration;

use super::network_outage::{
    HlsLiveSegmentNetworkRetry, HlsLiveSegmentRetryState, NO_PACKETS_DELIVERED,
};
use super::refresh::HlsLiveRefreshControl;
use super::segment_observer::HlsLiveSegmentExpiryObserver;
use super::{
    HlsLiveComponentKind, HlsLiveComponentSnapshot, HlsLiveSegmentIdentity,
    HlsLiveTimelineCoordinator, HlsLiveVideoDecodeStartEvidence,
};
use super::{hold_back, rejoin};
use crate::epoch_demux::open_epoch_with_key_cache_and_observer;
use crate::source::{HlsResourceExpiryObserver, SharedHlsKeyCache};
use crate::timestamp_origins::HlsTimestampOriginsWriter;
use crate::{HlsRequiredContainer, HlsVodOpenPolicy};

const MAXIMUM_SEGMENT_TRANSITIONS_PER_READ: usize = 4;

/// Cloneable live component recipe для initial open и transactional seek.
#[derive(Clone)]
pub(crate) struct HlsLiveComponentFactory {
    kind: HlsLiveComponentKind,
    container: HlsRequiredContainer,
    policy: HlsVodOpenPolicy,
    registry: Arc<DemuxRegistry>,
    coordinator: Arc<HlsLiveTimelineCoordinator>,
    refresh_control: Arc<HlsLiveRefreshControl>,
    /// Сколько ждать сеть, если сегмент не скачался (решение 334).
    network_reconnect: HttpReconnectPolicy,
    /// Публикация origin-ов сегментов для субтитров (только main-компонент; иначе no-op).
    timestamp_origins: HlsTimestampOriginsWriter,
}

impl HlsLiveComponentFactory {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        kind: HlsLiveComponentKind,
        container: HlsRequiredContainer,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        coordinator: Arc<HlsLiveTimelineCoordinator>,
        refresh_control: Arc<HlsLiveRefreshControl>,
        network_reconnect: HttpReconnectPolicy,
    ) -> Self {
        Self {
            kind,
            container,
            policy,
            registry,
            coordinator,
            refresh_control,
            network_reconnect,
            timestamp_origins: HlsTimestampOriginsWriter::disabled(),
        }
    }

    /// Включает публикацию origin-ов сегментов, которыми этот компонент пересчитывает пакеты.
    pub(super) fn publishing_timestamp_origins(
        mut self,
        timestamp_origins: HlsTimestampOriginsWriter,
    ) -> Self {
        self.timestamp_origins = timestamp_origins;
        self
    }

    pub fn open(&self) -> Result<HlsLiveComponentDemuxer> {
        let snapshot = self.snapshot()?;
        let join_position = self.coordinator.join_position()?;
        let start_index = hold_back::start_index_for_join(&snapshot, join_position);
        HlsLiveComponentDemuxer::open_at(self.clone(), snapshot, start_index, None, None)
    }

    pub fn prepare_seek_replacement(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
    ) -> Result<(HlsLiveComponentDemuxer, DemuxSeekResult)> {
        let (identity, anchor) = match self.kind {
            HlsLiveComponentKind::Main => self.coordinator.main_anchor_for(request.timestamp)?,
            HlsLiveComponentKind::AlternateAudio => {
                self.coordinator.audio_anchor_for(request.timestamp)?
            }
        }
        .ok_or_else(|| anyhow!("HLS live seek target has no retained decode anchor"))?;
        let snapshot = self.snapshot()?;
        let segment_index = snapshot
            .segments
            .iter()
            .position(|segment| segment.identity == identity)
            .ok_or_else(|| anyhow!("HLS live seek anchor has already been pushed out"))?;
        let replacement = HlsLiveComponentDemuxer::open_at(
            self.clone(),
            snapshot,
            segment_index,
            Some(stable_public_tracks),
            Some(anchor),
        )?;
        Ok((
            replacement,
            DemuxSeekResult {
                requested_position: MediaTime::from_duration(request.timestamp),
                actual_position: MediaTime::from_duration(anchor),
                actual_track_timestamp: None,
            },
        ))
    }

    fn snapshot(&self) -> Result<HlsLiveComponentSnapshot> {
        match self.kind {
            HlsLiveComponentKind::Main => self.coordinator.main_snapshot(),
            HlsLiveComponentKind::AlternateAudio => self
                .coordinator
                .audio_snapshot()?
                .context("HLS live alternate-audio snapshot missing"),
        }
    }

    fn runtime_snapshot(
        &self,
    ) -> Result<(
        HlsLiveComponentSnapshot,
        super::timeline::HlsLiveTransportSnapshot,
    )> {
        match self.kind {
            HlsLiveComponentKind::Main => self.coordinator.main_runtime_snapshot(),
            HlsLiveComponentKind::AlternateAudio => self
                .coordinator
                .audio_runtime_snapshot()?
                .context("HLS live alternate-audio runtime snapshot missing"),
        }
    }
}

/// Segment-scoped component demuxer; generic event API остаётся неизменным.
pub(crate) struct HlsLiveComponentDemuxer {
    factory: HlsLiveComponentFactory,
    current_identity: HlsLiveSegmentIdentity,
    current_segment_end: Duration,
    current: Box<dyn Demuxer + Send>,
    current_timestamp_origin: Option<Duration>,
    current_timeline_start: Duration,
    public_tracks: Vec<TrackInfo>,
    track_mapping: Vec<(TrackId, TrackId)>,
    metadata: Option<MediaMetadata>,
    replay_events: VecDeque<DemuxReadEvent>,
    key_cache: SharedHlsKeyCache,
    observed_manifest_live_edge: Duration,
    observed_transport_generation: SourceGeneration,
    current_expiry_observer: Arc<HlsLiveSegmentExpiryObserver>,
    pending_expiry: Option<HlsLivePendingExpiry>,
    /// Ожидание сети после сбоя сегмента и учёт уже отданных пакетов (решение 334).
    network_retry: HlsLiveSegmentNetworkRetry,
}

#[derive(Clone, Copy)]
struct HlsLivePendingExpiry {
    generation: SourceGeneration,
    identity: HlsLiveSegmentIdentity,
    timeline_start: Duration,
}

enum HlsLiveSegmentAdvance {
    Opened,
    AtEdge,
    RefreshPending,
    /// Сегмент не скачался из-за сети: повтор отложен (решение 334).
    WaitingForNetwork,
}

impl HlsLiveComponentDemuxer {
    fn open_at(
        factory: HlsLiveComponentFactory,
        snapshot: HlsLiveComponentSnapshot,
        segment_index: usize,
        stable_public_tracks: Option<&[TrackInfo]>,
        seek_anchor: Option<Duration>,
    ) -> Result<Self> {
        let segment = snapshot
            .segments
            .get(segment_index)
            .cloned()
            .ok_or_else(|| anyhow!("HLS live start segment is missing."))?;
        let key_cache = SharedHlsKeyCache::default();
        let (_, transport) = factory.runtime_snapshot()?;
        let expiry_observer = HlsLiveSegmentExpiryObserver::new(
            transport.generation,
            Arc::clone(&factory.refresh_control),
        );
        let current = open_epoch_with_key_cache_and_observer(
            factory.container,
            segment.epoch,
            transport.http,
            transport.generation,
            factory.policy,
            Arc::clone(&factory.registry),
            key_cache.clone(),
            Some(Arc::clone(&expiry_observer) as Arc<dyn HlsResourceExpiryObserver>),
        )?;
        let mut public_tracks = current.tracks().to_vec();
        for track in &mut public_tracks {
            track.duration = None;
        }
        if let Some(stable_tracks) = stable_public_tracks {
            public_tracks = stabilize_tracks(stable_tracks, &public_tracks)?;
        }
        let track_mapping = track_mapping(current.tracks(), &public_tracks)?;
        let network_retry = HlsLiveSegmentNetworkRetry::new(factory.network_reconnect);
        let mut demuxer = Self {
            factory,
            current_identity: segment.identity,
            current_segment_end: segment.timeline_end,
            current,
            current_timestamp_origin: None,
            current_timeline_start: segment.timeline_start,
            public_tracks,
            track_mapping,
            metadata: None,
            replay_events: VecDeque::new(),
            key_cache,
            observed_manifest_live_edge: snapshot.manifest_live_edge,
            observed_transport_generation: transport.generation,
            current_expiry_observer: expiry_observer,
            pending_expiry: None,
            network_retry,
        };
        if let Some(anchor) = seek_anchor {
            demuxer.prime_seek_anchor(anchor)?;
        }
        Ok(demuxer)
    }

    fn prime_seek_anchor(&mut self, anchor: Duration) -> Result<()> {
        let mut observed_events = 0usize;
        let mut observed_bytes = 0usize;
        while observed_events < self.factory.policy.maximum_seek_replay_events.get()
            && observed_bytes < self.factory.policy.maximum_seek_replay_bytes.get()
        {
            observed_events += 1;
            match self.read_current_event()? {
                DemuxReadEvent::Packet(packet) if packet.pts >= anchor => {
                    self.replay_events.push_back(DemuxReadEvent::Packet(packet));
                    return Ok(());
                }
                DemuxReadEvent::Packet(packet) => {
                    observed_bytes = observed_bytes.saturating_add(packet.data.len());
                }
                DemuxReadEvent::TracksChanged(update) => {
                    self.replay_events
                        .push_back(DemuxReadEvent::TracksChanged(update));
                }
                DemuxReadEvent::MediaMetadataChanged(metadata) => {
                    self.replay_events
                        .push_back(DemuxReadEvent::MediaMetadataChanged(metadata));
                }
                DemuxReadEvent::TemporarilyUnavailable(_) => {
                    return Err(anyhow!("HLS live seek anchor temporarily unavailable"));
                }
                DemuxReadEvent::EndOfStream => {
                    return Err(anyhow!("HLS live seek anchor not found within the segment"));
                }
            }
        }
        Err(anyhow!("HLS live seek anchor replay budget exceeded"))
    }

    fn read_current_event(&mut self) -> Result<DemuxReadEvent> {
        loop {
            match self.current.next_event()? {
                DemuxReadEvent::Packet(packet) => {
                    // После повтора сегмента (сеть вернулась) пакеты, которые плеер уже
                    // получил, приходят снова; пересчитываем их на шкалу (origin сегмента
                    // тот же) и выбрасываем.
                    let packet = self.remap_packet(packet)?;
                    if self.network_retry.is_already_delivered_packet() {
                        continue;
                    }
                    return Ok(DemuxReadEvent::Packet(packet));
                }
                DemuxReadEvent::TracksChanged(update) => {
                    self.apply_inner_tracks(&update.tracks)?;
                    return Ok(self.tracks_changed_event());
                }
                DemuxReadEvent::MediaMetadataChanged(metadata) => {
                    self.metadata = Some(metadata.clone());
                    return Ok(DemuxReadEvent::MediaMetadataChanged(metadata));
                }
                event => return Ok(event),
            }
        }
    }

    fn remap_packet(&mut self, mut packet: Packet) -> Result<Packet> {
        packet.track_id = self
            .track_mapping
            .iter()
            .find_map(|(inner, public)| (*inner == packet.track_id).then_some(*public))
            .ok_or_else(|| anyhow!("HLS live packet has an unknown track id"))?;
        let origin_is_new = self.current_timestamp_origin.is_none();
        let origin = *self
            .current_timestamp_origin
            .get_or_insert_with(|| packet.dts.map_or(packet.pts, |dts| packet.pts.min(dts)));
        if origin_is_new {
            // Origin сегмента зафиксирован ровно сейчас: публикуем ту же пару, что применяем ниже.
            self.factory.timestamp_origins.publish(
                self.current_identity.discontinuity_sequence,
                origin,
                self.current_timeline_start,
            );
        }
        packet.pts = packet
            .pts
            .saturating_sub(origin)
            .checked_add(self.current_timeline_start)
            .ok_or_else(|| anyhow!("HLS live packet PTS overflow"))?;
        packet.dts = packet
            .dts
            .map(|dts| {
                dts.saturating_sub(origin)
                    .checked_add(self.current_timeline_start)
                    .ok_or_else(|| anyhow!("HLS live packet DTS overflow"))
            })
            .transpose()?;
        let video_decode_start = self.video_decode_start_evidence(&packet)?;
        match self.factory.kind {
            HlsLiveComponentKind::Main => self.factory.coordinator.observe_main_packet(
                self.current_identity,
                &packet,
                video_decode_start,
            )?,
            HlsLiveComponentKind::AlternateAudio => self
                .factory
                .coordinator
                .observe_audio_packet(self.current_identity, &packet)?,
        }
        Ok(packet)
    }

    /// MPEG-TS H.264 anchor обязан пережить production decoder flush без старых SPS/PPS.
    fn video_decode_start_evidence(
        &self,
        packet: &Packet,
    ) -> Result<HlsLiveVideoDecodeStartEvidence> {
        if packet.kind != TrackKind::Video || packet.keyframe != PacketKeyframe::Keyframe {
            return Ok(HlsLiveVideoDecodeStartEvidence::NotProven);
        }
        if self.factory.container != HlsRequiredContainer::TransportStream {
            return Ok(HlsLiveVideoDecodeStartEvidence::Proven);
        }
        let track = self
            .public_tracks
            .iter()
            .find(|track| track.id == packet.track_id)
            .ok_or_else(|| anyhow!("HLS live video packet lost public track"))?;
        if track.codec_id != "V_MPEG4/ISO/AVC" {
            return Ok(HlsLiveVideoDecodeStartEvidence::Proven);
        }
        Ok(
            if packet.decode_start_initialization()
                == PacketDecodeStartInitialization::IncludesInBandConfiguration
            {
                HlsLiveVideoDecodeStartEvidence::Proven
            } else {
                HlsLiveVideoDecodeStartEvidence::NotProven
            },
        )
    }

    fn open_next_retained_segment(&mut self) -> Result<HlsLiveSegmentAdvance> {
        let (snapshot, transport) = self.factory.runtime_snapshot()?;
        if key_cache_requires_reset(
            self.observed_manifest_live_edge,
            self.observed_transport_generation,
            snapshot.manifest_live_edge,
            transport.generation,
        ) {
            self.key_cache.clear()?;
            self.observed_manifest_live_edge = snapshot.manifest_live_edge;
            self.observed_transport_generation = transport.generation;
        }
        let next = snapshot
            .segments
            .iter()
            .position(|segment| segment.identity == self.current_identity)
            .and_then(|index| snapshot.segments.get(index + 1))
            .or_else(|| rejoin::first_segment_after(&snapshot, self.current_segment_end))
            .cloned();
        let Some(segment) = next else {
            return Ok(HlsLiveSegmentAdvance::AtEdge);
        };
        self.open_segment_replacement(segment, transport, NO_PACKETS_DELIVERED)
    }

    /// Открывает сегмент на смену текущему.
    ///
    /// `packets_already_delivered` — сколько пакетов этого сегмента плеер уже получил
    /// до сетевого сбоя (`NO_PACKETS_DELIVERED` у нового сегмента): их выбросим при чтении.
    fn open_segment_replacement(
        &mut self,
        segment: super::snapshot::HlsLiveSegmentSnapshot,
        transport: super::timeline::HlsLiveTransportSnapshot,
        packets_already_delivered: usize,
    ) -> Result<HlsLiveSegmentAdvance> {
        let expiry_observer = HlsLiveSegmentExpiryObserver::new(
            transport.generation,
            Arc::clone(&self.factory.refresh_control),
        );
        let replacement = open_epoch_with_key_cache_and_observer(
            self.factory.container,
            segment.epoch.clone(),
            transport.http,
            transport.generation,
            self.factory.policy,
            Arc::clone(&self.factory.registry),
            self.key_cache.clone(),
            Some(Arc::clone(&expiry_observer) as Arc<dyn HlsResourceExpiryObserver>),
        );
        let replacement = match replacement {
            Ok(replacement) => replacement,
            Err(_) if expiry_observer.take_observed() => {
                self.factory.coordinator.expire_segment(
                    segment.identity,
                    self.factory.kind == HlsLiveComponentKind::AlternateAudio,
                )?;
                self.pending_expiry = Some(HlsLivePendingExpiry {
                    generation: transport.generation,
                    identity: segment.identity,
                    timeline_start: segment.timeline_start,
                });
                return Ok(HlsLiveSegmentAdvance::RefreshPending);
            }
            Err(error) => {
                // Сеть пропала: ждём её в пределах бюджета, а не роняем эфир.
                if let Some(retry_after) = expiry_observer.take_network_failure()
                    && self.network_retry.defer(
                        segment.identity,
                        packets_already_delivered,
                        retry_after,
                        Instant::now(),
                    )
                {
                    return Ok(HlsLiveSegmentAdvance::WaitingForNetwork);
                }
                return Err(error);
            }
        };
        let changed = self.apply_inner_tracks(replacement.tracks())?;
        self.current = replacement;
        self.current_identity = segment.identity;
        self.current_segment_end = segment.timeline_end;
        self.current_timeline_start = segment.timeline_start;
        self.current_timestamp_origin = None;
        self.current_expiry_observer = expiry_observer;
        self.pending_expiry = None;
        self.network_retry
            .note_segment_opened(packets_already_delivered);
        if changed {
            self.replay_events.push_back(self.tracks_changed_event());
        }
        Ok(HlsLiveSegmentAdvance::Opened)
    }

    fn observe_current_expiry(&mut self) -> Result<bool> {
        if !self.current_expiry_observer.take_observed() {
            return Ok(false);
        }
        self.factory.coordinator.expire_segment(
            self.current_identity,
            self.factory.kind == HlsLiveComponentKind::AlternateAudio,
        )?;
        self.pending_expiry = Some(HlsLivePendingExpiry {
            generation: self.current_expiry_observer.generation(),
            identity: self.current_identity,
            timeline_start: self.current_timeline_start,
        });
        Ok(true)
    }

    fn recover_pending_expiry(&mut self) -> Result<HlsLiveSegmentAdvance> {
        let Some(pending) = self.pending_expiry else {
            return Ok(HlsLiveSegmentAdvance::Opened);
        };
        let (snapshot, transport) = self.factory.runtime_snapshot()?;
        if transport.generation.value() < pending.generation.value() {
            return Err(anyhow!("HLS live transport generation regressed"));
        }
        if transport.generation == pending.generation {
            return Ok(HlsLiveSegmentAdvance::RefreshPending);
        }
        if key_cache_requires_reset(
            self.observed_manifest_live_edge,
            self.observed_transport_generation,
            snapshot.manifest_live_edge,
            transport.generation,
        ) {
            self.key_cache.clear()?;
            self.observed_manifest_live_edge = snapshot.manifest_live_edge;
            self.observed_transport_generation = transport.generation;
        }
        let segment = snapshot
            .segments
            .iter()
            .find(|segment| segment.identity == pending.identity)
            .or_else(|| {
                snapshot
                    .segments
                    .iter()
                    .find(|segment| segment.timeline_end > pending.timeline_start)
            })
            .cloned();
        let Some(segment) = segment else {
            self.pending_expiry = None;
            return Ok(HlsLiveSegmentAdvance::AtEdge);
        };
        self.open_segment_replacement(segment, transport, NO_PACKETS_DELIVERED)
    }

    /// Текущий сегмент оборвался из-за сети: откладывает его повтор с пропуском
    /// уже отданных пакетов. `false` — сбой не сетевой или бюджет ожидания исчерпан.
    fn defer_current_segment_after_network_failure(&mut self) -> bool {
        let Some(retry_after) = self.current_expiry_observer.take_network_failure() else {
            return false;
        };
        let packets_already_delivered = self.network_retry.packets_read_in_segment();
        self.network_retry.defer(
            self.current_identity,
            packets_already_delivered,
            retry_after,
            Instant::now(),
        )
    }

    /// Повторяет сегмент, отложенный из-за сети, когда подошёл срок.
    ///
    /// `Some` — ещё ждём или снова не вышло: читать сейчас нечего. Если окно за время
    /// ожидания ушло и сегмента в нём нет, продолжаем со следующего за прочитанным —
    /// тем же правилом, что после склейки (`rejoin::first_segment_after`).
    fn resume_after_network_failure(&mut self) -> Result<Option<DemuxReadEvent>> {
        let waiting = DemuxReadEvent::TemporarilyUnavailable(self.factory.policy.retry_hint);
        let pending = match self.network_retry.take_due(Instant::now()) {
            HlsLiveSegmentRetryState::NotPending => return Ok(None),
            HlsLiveSegmentRetryState::Waiting => return Ok(Some(waiting)),
            HlsLiveSegmentRetryState::Due(pending) => pending,
        };
        let (snapshot, transport) = self.factory.runtime_snapshot()?;
        let same_segment = snapshot
            .segments
            .iter()
            .find(|segment| segment.identity == pending.identity)
            .cloned()
            .map(|segment| (segment, pending.packets_already_delivered));
        let resumed = same_segment.or_else(|| {
            rejoin::first_segment_after(&snapshot, self.current_segment_end)
                .cloned()
                .map(|segment| (segment, NO_PACKETS_DELIVERED))
        });
        let Some((segment, packets_already_delivered)) = resumed else {
            // Оборванный сегмент читать нельзя: ждём, пока в окне появится продолжение.
            self.network_retry.keep_waiting_for_segment(pending);
            return Ok(Some(waiting));
        };
        match self.open_segment_replacement(segment, transport, packets_already_delivered)? {
            HlsLiveSegmentAdvance::Opened => Ok(None),
            HlsLiveSegmentAdvance::AtEdge
            | HlsLiveSegmentAdvance::RefreshPending
            | HlsLiveSegmentAdvance::WaitingForNetwork => Ok(Some(waiting)),
        }
    }

    fn apply_inner_tracks(&mut self, inner_tracks: &[TrackInfo]) -> Result<bool> {
        let stabilized = stabilize_tracks(&self.public_tracks, inner_tracks)?;
        let changed = stabilized != self.public_tracks;
        self.track_mapping = track_mapping(inner_tracks, &stabilized)?;
        if changed {
            self.public_tracks = stabilized;
        }
        Ok(changed)
    }

    fn tracks_changed_event(&self) -> DemuxReadEvent {
        DemuxReadEvent::TracksChanged(DemuxTrackListUpdate {
            tracks: self.public_tracks.clone(),
            duration: None,
        })
    }
}

impl Demuxer for HlsLiveComponentDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.public_tracks
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.metadata.clone()
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        if let Some(event) = self.replay_events.pop_front() {
            return Ok(event);
        }
        if let Some(waiting) = self.resume_after_network_failure()? {
            return Ok(waiting);
        }
        match self.recover_pending_expiry()? {
            HlsLiveSegmentAdvance::RefreshPending | HlsLiveSegmentAdvance::WaitingForNetwork => {
                return Ok(DemuxReadEvent::TemporarilyUnavailable(
                    self.factory.policy.retry_hint,
                ));
            }
            HlsLiveSegmentAdvance::Opened => {
                if let Some(event) = self.replay_events.pop_front() {
                    return Ok(event);
                }
            }
            HlsLiveSegmentAdvance::AtEdge => {
                let snapshot = self.factory.snapshot()?;
                return if snapshot.end_list {
                    Ok(DemuxReadEvent::EndOfStream)
                } else {
                    Ok(DemuxReadEvent::TemporarilyUnavailable(
                        self.factory.policy.retry_hint,
                    ))
                };
            }
        }
        for _ in 0..MAXIMUM_SEGMENT_TRANSITIONS_PER_READ {
            let event = match self.read_current_event() {
                Ok(event) => event,
                Err(_) if self.observe_current_expiry()? => {
                    return Ok(DemuxReadEvent::TemporarilyUnavailable(
                        self.factory.policy.retry_hint,
                    ));
                }
                Err(_) if self.defer_current_segment_after_network_failure() => {
                    return Ok(DemuxReadEvent::TemporarilyUnavailable(
                        self.factory.policy.retry_hint,
                    ));
                }
                Err(error) => return Err(error),
            };
            match event {
                DemuxReadEvent::EndOfStream => match self.open_next_retained_segment()? {
                    HlsLiveSegmentAdvance::Opened => {
                        if let Some(event) = self.replay_events.pop_front() {
                            return Ok(event);
                        }
                    }
                    HlsLiveSegmentAdvance::RefreshPending
                    | HlsLiveSegmentAdvance::WaitingForNetwork => {
                        return Ok(DemuxReadEvent::TemporarilyUnavailable(
                            self.factory.policy.retry_hint,
                        ));
                    }
                    HlsLiveSegmentAdvance::AtEdge => {
                        let snapshot = self.factory.snapshot()?;
                        return if snapshot.end_list {
                            Ok(DemuxReadEvent::EndOfStream)
                        } else {
                            Ok(DemuxReadEvent::TemporarilyUnavailable(
                                self.factory.policy.retry_hint,
                            ))
                        };
                    }
                },
                event => return Ok(event),
            }
        }
        Ok(DemuxReadEvent::TemporarilyUnavailable(
            self.factory.policy.retry_hint,
        ))
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let (replacement, result) = self
            .factory
            .prepare_seek_replacement(request, &self.public_tracks)?;
        *self = replacement;
        Ok(result)
    }
}

/// Key bytes не переживают accepted manifest revision либо transport generation.
fn key_cache_requires_reset(
    observed_live_edge: Duration,
    observed_generation: SourceGeneration,
    accepted_live_edge: Duration,
    accepted_generation: SourceGeneration,
) -> bool {
    accepted_live_edge != observed_live_edge || accepted_generation != observed_generation
}

fn stabilize_tracks(stable: &[TrackInfo], current: &[TrackInfo]) -> Result<Vec<TrackInfo>> {
    if stable.len() != current.len()
        || stable
            .iter()
            .zip(current)
            .any(|(stable, current)| stable.kind != current.kind)
    {
        return Err(anyhow!("HLS live track-kind topology changed"));
    }
    Ok(stable
        .iter()
        .zip(current)
        .map(|(stable, current)| {
            let mut track = current.clone();
            track.id = stable.id;
            track.duration = None;
            track
        })
        .collect())
}

fn track_mapping(inner: &[TrackInfo], public: &[TrackInfo]) -> Result<Vec<(TrackId, TrackId)>> {
    if inner.len() != public.len() {
        return Err(anyhow!("HLS live track mapping cardinality changed"));
    }
    inner
        .iter()
        .zip(public)
        .map(|(inner, public)| {
            if inner.kind != public.kind {
                return Err(anyhow!("HLS live track mapping kind changed"));
            }
            Ok((inner.id, public.id))
        })
        .collect()
}

#[cfg(test)]
#[path = "demux_tests.rs"]
mod tests;
