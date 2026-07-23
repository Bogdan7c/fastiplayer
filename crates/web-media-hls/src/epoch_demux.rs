//! Blocking HLS component demuxer с transactional multi-epoch seek.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use demux_api::{DemuxHints, DemuxInput, DemuxRegistry};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, MediaMetadata, MediaTime, Packet, PacketKeyframe, TrackId, TrackInfo, TrackKind,
};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use crate::plan::{HlsComponentPlan, HlsEpochPlan};
use crate::seek::{HlsSeekAnchor, HlsSeekAnchorKind, SharedHlsSeekIndex};
use crate::source::HlsEpochSegmentSource;
use crate::{HlsRequiredContainer, HlsVodOpenPolicy};

/// Cloneable construction recipe для offside transactional component replacement.
#[derive(Clone)]
pub(crate) struct HlsComponentFactory {
    plan: HlsComponentPlan,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: HlsVodOpenPolicy,
    registry: Arc<DemuxRegistry>,
    seek_index: SharedHlsSeekIndex,
}

impl HlsComponentFactory {
    /// Фиксирует immutable plan и shared proven seek index одного component-а.
    pub(crate) fn new(
        plan: HlsComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        seek_index: SharedHlsSeekIndex,
    ) -> Self {
        Self {
            plan,
            http,
            generation,
            policy,
            registry,
            seek_index,
        }
    }

    /// Открывает initial component и доказывает первый decode anchor.
    pub(crate) fn open(&self) -> Result<HlsComponentDemuxer> {
        HlsComponentDemuxer::open(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            self.seek_index.clone(),
        )
    }

    /// Полностью готовит positioned replacement, не меняя active component/composite.
    pub(crate) fn prepare_seek_replacement(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        let anchor = self.seek_index.lock().anchor_for(request)?;
        let preview = self.seek_index.lock().preview(request)?;
        let replacement_index =
            SharedHlsSeekIndex::new(self.policy.maximum_seek_index_entries.get());
        let mut replacement = HlsComponentDemuxer::open_from_epoch(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            replacement_index,
            anchor.epoch_index,
        )?;
        replacement.public_tracks = stable_public_tracks.to_vec();
        let replacement_tracks = replacement.current.tracks().to_vec();
        replacement.refresh_track_mapping(&replacement_tracks)?;
        replacement.position_replacement_at_anchor(anchor)?;
        replacement.seek_index = self.seek_index.clone();
        Ok((replacement, preview))
    }
}

/// Blocking multi-epoch component demuxer; весь объект живёт внутри progressive worker-а.
pub(crate) struct HlsComponentDemuxer {
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: HlsVodOpenPolicy,
    registry: Arc<DemuxRegistry>,
    plan: HlsComponentPlan,
    remaining_epochs: VecDeque<(usize, HlsEpochPlan)>,
    current_epoch_index: usize,
    current: Box<dyn Demuxer + Send>,
    current_timeline_start: Duration,
    current_timestamp_origin: Option<Duration>,
    public_tracks: Vec<TrackInfo>,
    track_mapping: Vec<(TrackId, TrackId)>,
    duration: Duration,
    metadata: Option<MediaMetadata>,
    replay_events: VecDeque<DemuxReadEvent>,
    seek_index: SharedHlsSeekIndex,
}

impl HlsComponentDemuxer {
    /// Открывает первый epoch и доказывает initial RAP/audio anchor до publication tracks.
    pub(crate) fn open(
        plan: HlsComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        seek_index: SharedHlsSeekIndex,
    ) -> Result<Self> {
        let mut component =
            Self::open_from_epoch(plan, http, generation, policy, registry, seek_index, 0)?;
        component.prime_initial_seek_anchor()?;
        Ok(component)
    }

    /// Создаёт replacement parser, не меняя active instance.
    #[allow(clippy::too_many_arguments)]
    fn open_from_epoch(
        plan: HlsComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        seek_index: SharedHlsSeekIndex,
        epoch_index: usize,
    ) -> Result<Self> {
        let epoch = plan
            .epochs
            .get(epoch_index)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("HLS seek epoch отсутствует в immutable plan"))?;
        let current_timeline_start = epoch.timeline_start;
        let current = open_epoch(
            plan.container,
            epoch,
            http.clone(),
            generation,
            policy,
            Arc::clone(&registry),
        )?;
        let public_tracks = current
            .tracks()
            .iter()
            .cloned()
            .map(|mut track| {
                track.duration = Some(plan.duration);
                track
            })
            .collect::<Vec<_>>();
        let track_mapping = public_tracks
            .iter()
            .map(|track| (track.id, track.id))
            .collect();
        let remaining_epochs = plan
            .epochs
            .iter()
            .cloned()
            .enumerate()
            .skip(epoch_index.saturating_add(1))
            .collect();
        let metadata = current.media_metadata();
        let duration = plan.duration;
        Ok(Self {
            http,
            generation,
            policy,
            registry,
            plan,
            remaining_epochs,
            current_epoch_index: epoch_index,
            current,
            current_timeline_start,
            current_timestamp_origin: None,
            public_tracks,
            track_mapping,
            duration,
            metadata,
            replay_events: VecDeque::new(),
            seek_index,
        })
    }

    /// Initial preflight читает только на worker-е и сохраняет exact events для replay.
    fn prime_initial_seek_anchor(&mut self) -> Result<()> {
        let has_video = self
            .public_tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video);
        let mut inspected_events = 0_usize;
        let mut inspected_bytes = 0_usize;
        while !self
            .seek_index
            .lock()
            .has_required_initial_anchor(has_video)
        {
            let event = self.read_next_inner_event()?;
            inspected_events = inspected_events.saturating_add(1);
            inspected_bytes = inspected_bytes.saturating_add(event_encoded_bytes(&event));
            self.ensure_seek_scan_within_budget(inspected_events, inspected_bytes)?;
            if matches!(event, DemuxReadEvent::EndOfStream) {
                anyhow::bail!("HLS VOD завершился до доказанного initial seek anchor");
            }
            self.replay_events.push_back(event);
        }
        Ok(())
    }

    /// Reopen сканирует replacement до уже доказанного packet-а и коммитит его отдельно.
    fn position_replacement_at_anchor(&mut self, anchor: HlsSeekAnchor) -> Result<()> {
        let mut inspected_events = 0_usize;
        let mut inspected_bytes = 0_usize;
        loop {
            let event = self.read_next_inner_event()?;
            inspected_events = inspected_events.saturating_add(1);
            inspected_bytes = inspected_bytes.saturating_add(event_encoded_bytes(&event));
            self.ensure_seek_scan_within_budget(inspected_events, inspected_bytes)?;
            match event {
                DemuxReadEvent::Packet(packet) if packet_matches_anchor(&packet, anchor) => {
                    self.replay_events.push_back(self.tracks_changed_event());
                    self.replay_events.push_back(DemuxReadEvent::Packet(packet));
                    return Ok(());
                }
                DemuxReadEvent::EndOfStream => {
                    anyhow::bail!("HLS replacement parser не воспроизвёл доказанный seek anchor");
                }
                DemuxReadEvent::Packet(_)
                | DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_)
                | DemuxReadEvent::TemporarilyUnavailable(_) => {}
            }
        }
    }

    fn ensure_seek_scan_within_budget(
        &self,
        inspected_events: usize,
        inspected_bytes: usize,
    ) -> Result<()> {
        if inspected_events > self.policy.maximum_seek_replay_events.get() {
            anyhow::bail!("HLS seek replay превысил caller-owned event budget");
        }
        if inspected_bytes > self.policy.maximum_seek_replay_bytes.get() {
            anyhow::bail!("HLS seek replay превысил caller-owned encoded-byte budget");
        }
        Ok(())
    }

    fn open_next_epoch(&mut self) -> Result<bool> {
        let Some((epoch_index, epoch)) = self.remaining_epochs.pop_front() else {
            return Ok(false);
        };
        self.current_epoch_index = epoch_index;
        self.current_timeline_start = epoch.timeline_start;
        self.current_timestamp_origin = None;
        self.current = open_epoch(
            self.plan.container,
            epoch,
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
        )?;
        let next_tracks = self.current.tracks().to_vec();
        self.refresh_track_mapping(&next_tracks)?;
        self.metadata = self
            .current
            .media_metadata()
            .or_else(|| self.metadata.clone());
        Ok(true)
    }

    fn refresh_track_mapping(&mut self, inner_tracks: &[TrackInfo]) -> Result<()> {
        let previous_layout = layout_ordinals(&self.public_tracks);
        let next_layout = layout_ordinals(inner_tracks);
        if previous_layout.len() != next_layout.len()
            || previous_layout
                .iter()
                .zip(&next_layout)
                .any(|((previous_kind, _), (next_kind, _))| previous_kind != next_kind)
        {
            anyhow::bail!("HLS discontinuity изменила required component track topology");
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

    fn remap_packet(&mut self, mut packet: Packet) -> Result<Packet> {
        packet.track_id = self
            .track_mapping
            .iter()
            .find_map(|(inner, public)| (*inner == packet.track_id).then_some(*public))
            .ok_or_else(|| anyhow::anyhow!("HLS epoch packet с неизвестным track id"))?;
        let origin = *self
            .current_timestamp_origin
            .get_or_insert_with(|| packet.dts.map_or(packet.pts, |dts| packet.pts.min(dts)));
        packet.pts = packet
            .pts
            .saturating_sub(origin)
            .checked_add(self.current_timeline_start)
            .ok_or_else(|| anyhow::anyhow!("HLS global packet PTS overflow"))?;
        packet.dts = match packet.dts {
            Some(dts) => Some(
                dts.saturating_sub(origin)
                    .checked_add(self.current_timeline_start)
                    .ok_or_else(|| anyhow::anyhow!("HLS global packet DTS overflow"))?,
            ),
            None => None,
        };
        self.seek_index
            .lock()
            .observe_packet(self.current_epoch_index, &packet);
        Ok(packet)
    }

    fn tracks_changed_event(&self) -> DemuxReadEvent {
        DemuxReadEvent::TracksChanged(DemuxTrackListUpdate {
            tracks: self.public_tracks.clone(),
            duration: Some(self.duration),
        })
    }

    fn read_next_inner_event(&mut self) -> Result<DemuxReadEvent> {
        match self.current.next_event()? {
            DemuxReadEvent::Packet(packet) => self.remap_packet(packet).map(DemuxReadEvent::Packet),
            DemuxReadEvent::EndOfStream if self.open_next_epoch()? => {
                Ok(self.tracks_changed_event())
            }
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

impl Demuxer for HlsComponentDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.public_tracks
    }

    fn duration(&self) -> Option<Duration> {
        Some(self.duration)
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
        self.read_next_inner_event()
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let anchor = self.seek_index.lock().anchor_for(request)?;
        let preview = self.seek_index.lock().preview(request)?;
        let replacement_index =
            SharedHlsSeekIndex::new(self.policy.maximum_seek_index_entries.get());
        let mut replacement = Self::open_from_epoch(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            replacement_index,
            anchor.epoch_index,
        )?;
        replacement.public_tracks = self.public_tracks.clone();
        let replacement_tracks = replacement.current.tracks().to_vec();
        replacement.refresh_track_mapping(&replacement_tracks)?;
        replacement.position_replacement_at_anchor(anchor)?;
        replacement.seek_index = self.seek_index.clone();
        *self = replacement;
        Ok(preview)
    }
}

fn open_epoch(
    container: HlsRequiredContainer,
    epoch: HlsEpochPlan,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: HlsVodOpenPolicy,
    registry: Arc<DemuxRegistry>,
) -> Result<Box<dyn Demuxer + Send>> {
    let cancellation = http.cancellation().clone();
    let source =
        HlsEpochSegmentSource::new(http, generation, epoch, policy.maximum_key_resource_bytes);
    registry
        .open_required_container(
            DemuxInput::ordered_segments(Box::new(source)),
            DemuxHints::none(),
            policy.demux_sniff_budget,
            cancellation,
            container
                .demux_container_id()
                .context("invalid static HLS container identity")?,
        )
        .context("HLS epoch container sniff/open failed")
}

fn packet_matches_anchor(packet: &Packet, anchor: HlsSeekAnchor) -> bool {
    let kind_matches = match anchor.kind {
        HlsSeekAnchorKind::VideoRandomAccessPoint => {
            packet.kind == TrackKind::Video && packet.keyframe == PacketKeyframe::Keyframe
        }
        HlsSeekAnchorKind::AudioPacket => packet.kind == TrackKind::Audio,
    };
    kind_matches && MediaTime::from_duration(packet.pts) == anchor.position
}

fn event_encoded_bytes(event: &DemuxReadEvent) -> usize {
    match event {
        DemuxReadEvent::Packet(packet) => packet.data.len(),
        DemuxReadEvent::EndOfStream
        | DemuxReadEvent::TracksChanged(_)
        | DemuxReadEvent::MediaMetadataChanged(_)
        | DemuxReadEvent::TemporarilyUnavailable(_) => 0,
    }
}

fn layout_ordinals(tracks: &[TrackInfo]) -> Vec<(TrackKind, usize)> {
    let mut video_index = 0;
    let mut audio_index = 0;
    tracks
        .iter()
        .map(|track| match track.kind {
            TrackKind::Video => {
                let ordinal = video_index;
                video_index += 1;
                (TrackKind::Video, ordinal)
            }
            TrackKind::Audio => {
                let ordinal = audio_index;
                audio_index += 1;
                (TrackKind::Audio, ordinal)
            }
        })
        .collect()
}
