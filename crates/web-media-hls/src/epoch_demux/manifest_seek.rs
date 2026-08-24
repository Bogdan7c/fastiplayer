//! Manifest-owned подготовка near-target HLS replacement-а для worker-receipted seek.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use demux_api::DemuxRegistry;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, MediaTime, Packet, PacketKeyframe,
    TrackInfo, TrackKind,
};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use super::{HlsComponentDemuxer, HlsComponentFactory, event_encoded_bytes, packet_matches_anchor};
use crate::HlsVodOpenPolicy;
use crate::plan::{HlsComponentPlan, HlsManifestSeekPoint};
use crate::seek::{HlsSeekAnchor, HlsSeekAnchorKind, HlsSeekIndex, SharedHlsSeekIndex};

impl HlsComponentFactory {
    /// Готовит receipted replacement возле manifest target и сохраняет exact-anchor fallback.
    pub(crate) fn prepare_receipted_seek_replacement(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        for manifest_point in self.plan.manifest_seek_candidates(request.timestamp) {
            if let Some(prepared) =
                self.prepare_manifest_seek_candidate(request, stable_public_tracks, manifest_point)?
            {
                return Ok(prepared);
            }
        }
        // Необычный HLS segment может не содержать RAP до target. Тогда сохраняем прежний
        // доказанный путь вместо потери seek capability или публикации выдуманного anchor-а.
        self.prepare_seek_replacement(request, stable_public_tracks)
    }

    /// Проверяет один immutable manifest candidate offside от active demuxer-а.
    fn prepare_manifest_seek_candidate(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
        manifest_point: HlsManifestSeekPoint,
    ) -> Result<Option<(HlsComponentDemuxer, DemuxSeekResult)>> {
        // Kind выбирается по уже опубликованному index-у: Accurate seek не должен случайно
        // переключиться с audio на video только из-за порядка packets в новом suffix-е.
        let required_kind = self.seek_index.lock().required_kind(request);
        let replacement_index =
            SharedHlsSeekIndex::new(self.policy.maximum_seek_index_entries.get());
        let mut replacement = HlsComponentDemuxer::open_from_manifest_seek_point(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            replacement_index,
            manifest_point,
        )?;
        replacement.public_tracks = stable_public_tracks.to_vec();
        let replacement_tracks = replacement.current.tracks().to_vec();
        replacement.refresh_track_mapping(&replacement_tracks)?;
        let Some(anchor) = replacement.position_at_first_manifest_anchor(request, required_kind)?
        else {
            return Ok(None);
        };
        let result = HlsSeekIndex::result_for_anchor(request, anchor);
        self.seek_index.lock().commit_proven_anchor(anchor);
        replacement.seek_index = self.seek_index.clone();
        Ok(Some((replacement, result)))
    }
}

impl HlsComponentDemuxer {
    /// Открывает manifest suffix без изменения active component и без fabricated packet evidence.
    #[allow(clippy::too_many_arguments)]
    fn open_from_manifest_seek_point(
        plan: HlsComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        seek_index: SharedHlsSeekIndex,
        point: HlsManifestSeekPoint,
    ) -> Result<Self> {
        let epoch = plan.manifest_restart_tail(point).ok_or_else(|| {
            anyhow::anyhow!("HLS manifest seek point отсутствует в immutable plan")
        })?;
        Self::open_from_epoch_plan(
            plan,
            http,
            generation,
            policy,
            registry,
            seek_index,
            point.epoch_index,
            epoch,
            None,
        )
    }

    /// Находит первый настоящий RAP/audio anchor не позже target внутри near-target suffix-а.
    fn position_at_first_manifest_anchor(
        &mut self,
        request: DemuxSeekRequest,
        required_kind: HlsSeekAnchorKind,
    ) -> Result<Option<HlsSeekAnchor>> {
        let mut inspected_events = 0_usize;
        let mut inspected_bytes = 0_usize;
        let mut retained_audio_packets = VecDeque::<Packet>::new();
        loop {
            let event = self.read_next_inner_event()?;
            inspected_events = inspected_events.saturating_add(1);
            inspected_bytes = inspected_bytes.saturating_add(event_encoded_bytes(&event));
            if inspected_events > self.policy.maximum_seek_replay_events.get()
                || inspected_bytes > self.policy.maximum_seek_replay_bytes.get()
            {
                return Ok(None);
            }
            match event {
                DemuxReadEvent::Packet(packet) => {
                    let matching_anchor = {
                        let seek_index = self.seek_index.lock();
                        seek_index
                            .anchor_of_kind_before(required_kind, request.timestamp)
                            .ok()
                            .filter(|anchor| packet_matches_anchor(&packet, *anchor))
                    };
                    if let Some(anchor) = matching_anchor {
                        self.replay_events.push_back(self.tracks_changed_event());
                        self.replay_events.push_back(DemuxReadEvent::Packet(packet));
                        self.replay_events.extend(
                            retained_audio_packets
                                .into_iter()
                                .filter(|audio| {
                                    MediaTime::from_duration(audio.pts) >= anchor.position
                                })
                                .map(DemuxReadEvent::Packet),
                        );
                        return Ok(Some(anchor));
                    }
                    if packet_passed_manifest_target(&packet, required_kind, request.timestamp) {
                        return Ok(None);
                    }
                    if required_kind == HlsSeekAnchorKind::VideoRandomAccessPoint
                        && packet.kind == TrackKind::Audio
                    {
                        retained_audio_packets.push_back(packet);
                    }
                }
                DemuxReadEvent::EndOfStream => return Ok(None),
                DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_)
                | DemuxReadEvent::TemporarilyUnavailable(_) => {}
            }
        }
    }
}

/// Определяет, что qualifying packet уже прошёл target и текущий candidate бесполезен.
fn packet_passed_manifest_target(
    packet: &Packet,
    required_kind: HlsSeekAnchorKind,
    target: Duration,
) -> bool {
    let kind_matches = match required_kind {
        HlsSeekAnchorKind::VideoRandomAccessPoint => {
            packet.kind == TrackKind::Video && packet.keyframe == PacketKeyframe::Keyframe
        }
        HlsSeekAnchorKind::AudioPacket => packet.kind == TrackKind::Audio,
    };
    kind_matches && packet.pts > target
}
