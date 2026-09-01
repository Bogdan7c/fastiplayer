use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use codec_core::{H264Packetization, probe_h264_packet_in_band_decode_start};
use demux_api::DemuxRegistry;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, MediaMetadata, MediaTime, Packet, PacketKeyframe, TrackId, TrackInfo, TrackKind,
};
use web_media_transport_api::SourceGeneration;

use super::refresh::{HlsLiveEndpointExpirySignal, HlsLiveRefreshControl};
use super::{
    HlsLiveComponentKind, HlsLiveComponentSnapshot, HlsLiveSegmentIdentity,
    HlsLiveTimelineCoordinator, HlsLiveVideoDecodeStartEvidence,
};
use crate::epoch_demux::open_epoch_with_key_cache_and_observer;
use crate::source::{HlsRefreshableResourceKind, HlsResourceExpiryObserver, SharedHlsKeyCache};
use crate::{HlsEndpointRefreshReason, HlsRequiredContainer, HlsVodOpenPolicy};

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
    ) -> Self {
        Self {
            kind,
            container,
            policy,
            registry,
            coordinator,
            refresh_control,
        }
    }

    pub fn open(&self) -> Result<HlsLiveComponentDemuxer> {
        let snapshot = self.snapshot()?;
        let start_index = live_start_index(&snapshot);
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
        .ok_or_else(|| anyhow!("HLS live seek target не имеет retained decode anchor"))?;
        let snapshot = self.snapshot()?;
        let segment_index = snapshot
            .segments
            .iter()
            .position(|segment| segment.identity == identity)
            .ok_or_else(|| anyhow!("HLS live seek anchor уже вытеснен"))?;
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
                .context("HLS live alternate-audio snapshot отсутствует"),
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
                .context("HLS live alternate-audio runtime snapshot отсутствует"),
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
}

struct HlsLiveSegmentExpiryObserver {
    generation: SourceGeneration,
    control: Arc<HlsLiveRefreshControl>,
    observed: AtomicBool,
}

impl HlsLiveSegmentExpiryObserver {
    fn new(generation: SourceGeneration, control: Arc<HlsLiveRefreshControl>) -> Arc<Self> {
        Arc::new(Self {
            generation,
            control,
            observed: AtomicBool::new(false),
        })
    }

    fn take_observed(&self) -> bool {
        self.observed.swap(false, Ordering::SeqCst)
    }

    const fn generation(&self) -> SourceGeneration {
        self.generation
    }
}

impl HlsResourceExpiryObserver for HlsLiveSegmentExpiryObserver {
    fn observe_refreshable_expiry(
        &self,
        reason: HlsEndpointRefreshReason,
        resource_kind: HlsRefreshableResourceKind,
    ) {
        self.observed.store(true, Ordering::SeqCst);
        self.control.signal_expiry(HlsLiveEndpointExpirySignal {
            generation: self.generation,
            reason,
            resource_kind,
        });
    }
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
            .ok_or_else(|| anyhow!("HLS live start segment отсутствует"))?;
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
                    return Err(anyhow!("HLS live seek anchor не найден внутри segment"));
                }
            }
        }
        Err(anyhow!("HLS live seek anchor replay budget exceeded"))
    }

    fn read_current_event(&mut self) -> Result<DemuxReadEvent> {
        match self.current.next_event()? {
            DemuxReadEvent::Packet(packet) => self.remap_packet(packet).map(DemuxReadEvent::Packet),
            DemuxReadEvent::TracksChanged(update) => {
                self.apply_inner_tracks(&update.tracks)?;
                Ok(self.tracks_changed_event())
            }
            DemuxReadEvent::MediaMetadataChanged(metadata) => {
                self.metadata = Some(metadata.clone());
                Ok(DemuxReadEvent::MediaMetadataChanged(metadata))
            }
            event => Ok(event),
        }
    }

    fn remap_packet(&mut self, mut packet: Packet) -> Result<Packet> {
        packet.track_id = self
            .track_mapping
            .iter()
            .find_map(|(inner, public)| (*inner == packet.track_id).then_some(*public))
            .ok_or_else(|| anyhow!("HLS live packet имеет неизвестный track id"))?;
        let origin = *self
            .current_timestamp_origin
            .get_or_insert_with(|| packet.dts.map_or(packet.pts, |dts| packet.pts.min(dts)));
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
            .ok_or_else(|| anyhow!("HLS live video packet потерял public track"))?;
        if track.codec_id != "V_MPEG4/ISO/AVC" {
            return Ok(HlsLiveVideoDecodeStartEvidence::Proven);
        }
        let is_self_contained =
            probe_h264_packet_in_band_decode_start(&packet.data, H264Packetization::AnnexB)
                .context("HLS live H.264 decode-start probe failed")?;
        Ok(if is_self_contained {
            HlsLiveVideoDecodeStartEvidence::Proven
        } else {
            HlsLiveVideoDecodeStartEvidence::NotProven
        })
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
            .or_else(|| {
                snapshot
                    .segments
                    .iter()
                    .find(|segment| segment.timeline_end > self.current_segment_end)
            })
            .cloned();
        let Some(segment) = next else {
            return Ok(HlsLiveSegmentAdvance::AtEdge);
        };
        self.open_segment_replacement(segment, transport)
    }

    fn open_segment_replacement(
        &mut self,
        segment: super::snapshot::HlsLiveSegmentSnapshot,
        transport: super::timeline::HlsLiveTransportSnapshot,
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
            Err(error) => return Err(error),
        };
        let changed = self.apply_inner_tracks(replacement.tracks())?;
        self.current = replacement;
        self.current_identity = segment.identity;
        self.current_segment_end = segment.timeline_end;
        self.current_timeline_start = segment.timeline_start;
        self.current_timestamp_origin = None;
        self.current_expiry_observer = expiry_observer;
        self.pending_expiry = None;
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
        self.open_segment_replacement(segment, transport)
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
        match self.recover_pending_expiry()? {
            HlsLiveSegmentAdvance::RefreshPending => {
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
                Err(error) => return Err(error),
            };
            match event {
                DemuxReadEvent::EndOfStream => match self.open_next_retained_segment()? {
                    HlsLiveSegmentAdvance::Opened => {
                        if let Some(event) = self.replay_events.pop_front() {
                            return Ok(event);
                        }
                    }
                    HlsLiveSegmentAdvance::RefreshPending => {
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

fn live_start_index(snapshot: &HlsLiveComponentSnapshot) -> usize {
    let safe_distance = snapshot.target_duration.saturating_mul(3);
    let target = snapshot.manifest_live_edge.saturating_sub(safe_distance);
    snapshot
        .segments
        .iter()
        .rposition(|segment| segment.timeline_start <= target)
        .unwrap_or(0)
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
mod tests {
    use media_core::TrackKind;

    use super::*;

    fn video_track(id: u32, codec_id: &str, duration: Option<Duration>) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(id),
            kind: TrackKind::Video,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: None,
            duration,
            sample_rate: None,
            channels: None,
            video: None,
        }
    }

    #[test]
    fn tracks_changed_is_driven_by_actual_config_not_segment_local_ids() {
        let stable = vec![video_track(7, "h264", None)];
        let same_config = vec![video_track(99, "h264", Some(Duration::from_secs(6)))];
        let stabilized = stabilize_tracks(&stable, &same_config).expect("same topology");
        assert_eq!(stabilized, stable);

        let changed_config = vec![video_track(100, "h265", Some(Duration::from_secs(6)))];
        let stabilized = stabilize_tracks(&stable, &changed_config).expect("same track kind");
        assert_ne!(stabilized, stable);

        let audio_topology = TrackInfo {
            kind: TrackKind::Audio,
            ..video_track(1, "aac", None)
        };
        assert!(stabilize_tracks(&stable, &[audio_topology]).is_err());
    }

    #[test]
    fn key_cache_is_reused_only_inside_one_accepted_snapshot_generation() {
        let edge = Duration::from_secs(18);
        let generation = SourceGeneration::new(4);
        assert!(!key_cache_requires_reset(
            edge, generation, edge, generation
        ));
        assert!(key_cache_requires_reset(
            edge,
            generation,
            Duration::from_secs(24),
            generation,
        ));
        assert!(key_cache_requires_reset(
            edge,
            generation,
            edge,
            SourceGeneration::new(5),
        ));
    }
}
