//! Bounded readiness/lead accounting для neutral composite demuxer-а.

use std::cmp::Ordering;
use std::time::Duration;

use anyhow::Result;
use media_core::{DemuxReadEvent, DemuxRetryHint, Packet};

use super::{
    CompositeAvDemuxer, CompositeComponent, CompositePendingPacketTooLargeError,
    PendingFillOutcome, PostSeekAudioBootstrap,
};

/// Bounded progress одной component side относительно ещё не стартовавшего peer-а.
#[derive(Debug, Default)]
pub(super) struct ComponentLeadProgress {
    /// PTS последнего packet-а, который composite уже отдал consumer-у.
    pub(super) last_emitted_pts: Option<Duration>,
    /// Число bootstrap packets, выданных до первого comparable peer timestamp.
    pub(super) bootstrap_packets: usize,
    /// Суммарные bootstrap payload bytes до первого comparable peer timestamp.
    pub(super) bootstrap_bytes: usize,
}

impl CompositeAvDemuxer {
    /// Валидирует payload ceiling до сохранения selected packet в pending slot.
    pub(super) fn store_pending_packet(
        &mut self,
        component: CompositeComponent,
        packet: Packet,
    ) -> Result<()> {
        let maximum_bytes = self.lead_policy.bootstrap_byte_limit();
        let packet_bytes = packet.data.len();
        if packet_bytes > maximum_bytes {
            return Err(CompositePendingPacketTooLargeError {
                component,
                packet_bytes,
                maximum_bytes,
            }
            .into());
        }
        match component {
            CompositeComponent::Video => self.pending_video_packet = Some(packet),
            CompositeComponent::Audio => self.pending_audio_packet = Some(packet),
        }
        Ok(())
    }

    /// Проверяет, может ли ready side продвинуться без required lagging peer-а.
    pub(super) fn can_emit_while_peer_unavailable(
        &self,
        component: CompositeComponent,
        packet: &Packet,
    ) -> bool {
        let (progress, peer_progress, peer_eof) = match component {
            CompositeComponent::Video => (
                &self.video_lead_progress,
                &self.audio_lead_progress,
                self.audio_eof,
            ),
            CompositeComponent::Audio => (
                &self.audio_lead_progress,
                &self.video_lead_progress,
                self.video_eof,
            ),
        };
        if peer_eof {
            return true;
        }
        if let Some(peer_pts) = peer_progress.last_emitted_pts {
            let maximum_pts = peer_pts.saturating_add(self.lead_policy.max_timestamp_lead());
            return packet.pts <= maximum_pts;
        }
        let next_packet_count = progress.bootstrap_packets.saturating_add(1);
        let next_byte_count = progress.bootstrap_bytes.saturating_add(packet.data.len());
        next_packet_count <= self.lead_policy.bootstrap_packet_limit()
            && next_byte_count <= self.lead_policy.bootstrap_byte_limit()
    }

    /// Фиксирует только composite-owned lead accounting после фактической выдачи packet-а.
    fn record_emitted_packet(&mut self, component: CompositeComponent, packet: &Packet) {
        let peer_has_comparable_timestamp = match component {
            CompositeComponent::Video => self.audio_lead_progress.last_emitted_pts.is_some(),
            CompositeComponent::Audio => self.video_lead_progress.last_emitted_pts.is_some(),
        };
        let progress = match component {
            CompositeComponent::Video => &mut self.video_lead_progress,
            CompositeComponent::Audio => &mut self.audio_lead_progress,
        };
        progress.last_emitted_pts = Some(packet.pts);
        if !peer_has_comparable_timestamp {
            progress.bootstrap_packets = progress.bootstrap_packets.saturating_add(1);
            progress.bootstrap_bytes = progress.bootstrap_bytes.saturating_add(packet.data.len());
        }
        if self.video_lead_progress.last_emitted_pts.is_some()
            && self.audio_lead_progress.last_emitted_pts.is_some()
        {
            self.video_lead_progress.bootstrap_packets = 0;
            self.video_lead_progress.bootstrap_bytes = 0;
            self.audio_lead_progress.bootstrap_packets = 0;
            self.audio_lead_progress.bootstrap_bytes = 0;
        }
    }

    /// Возвращает remapped packet и атомарно обновляет lead accounting.
    pub(super) fn emitted_packet_event(
        &mut self,
        component: CompositeComponent,
        packet: Packet,
    ) -> DemuxReadEvent {
        self.record_emitted_packet(component, &packet);
        DemuxReadEvent::Packet(packet)
    }

    /// Выполняет один non-blocking composite read с lifecycle/readiness ordering.
    pub(super) fn read_next_composite_event(&mut self) -> Result<DemuxReadEvent> {
        let video_retry_hint = match self.fill_pending_video_event()? {
            PendingFillOutcome::TracksChanged(update) => {
                return Ok(DemuxReadEvent::TracksChanged(update));
            }
            PendingFillOutcome::MediaMetadataChanged(metadata) => {
                return Ok(DemuxReadEvent::MediaMetadataChanged(metadata));
            }
            PendingFillOutcome::TemporarilyUnavailable(hint) => Some(hint),
            PendingFillOutcome::Ready => None,
        };
        let audio_retry_hint = match self.fill_pending_audio_event()? {
            PendingFillOutcome::TracksChanged(update) => {
                return Ok(DemuxReadEvent::TracksChanged(update));
            }
            PendingFillOutcome::MediaMetadataChanged(metadata) => {
                return Ok(DemuxReadEvent::MediaMetadataChanged(metadata));
            }
            PendingFillOutcome::TemporarilyUnavailable(hint) => Some(hint),
            PendingFillOutcome::Ready => None,
        };

        if self.post_seek_audio_bootstrap == PostSeekAudioBootstrap::DecodePointBeforePending {
            if let Some(audio_packet) = self.take_post_seek_audio_bootstrap_packet() {
                return Ok(self.emitted_packet_event(CompositeComponent::Audio, audio_packet));
            }
            if let Some(audio_retry_hint) = audio_retry_hint {
                return Ok(DemuxReadEvent::TemporarilyUnavailable(audio_retry_hint));
            }
            if self.audio_eof {
                self.post_seek_audio_bootstrap = PostSeekAudioBootstrap::Inactive;
            }
        }

        if let (Some(video_packet), Some(audio_packet)) = (
            self.pending_video_packet.as_ref(),
            self.pending_audio_packet.as_ref(),
        ) {
            let emit_video = video_packet.presentation_order_cmp(audio_packet) != Ordering::Greater;
            let component = if emit_video {
                CompositeComponent::Video
            } else {
                CompositeComponent::Audio
            };
            let packet = match component {
                CompositeComponent::Video => self.pending_video_packet.take(),
                CompositeComponent::Audio => self.pending_audio_packet.take(),
            };
            let Some(packet) = packet else {
                anyhow::bail!("selected pending packet исчез до composite emission");
            };
            return Ok(self.emitted_packet_event(component, packet));
        }

        if let Some(video_packet) = self.pending_video_packet.as_ref() {
            let can_emit_video = self.audio_eof
                || self.can_emit_while_peer_unavailable(CompositeComponent::Video, video_packet);
            if can_emit_video {
                let Some(video_packet) = self.pending_video_packet.take() else {
                    anyhow::bail!("video pending packet исчез до composite emission");
                };
                return Ok(self.emitted_packet_event(CompositeComponent::Video, video_packet));
            }
            if let Some(audio_retry_hint) = audio_retry_hint {
                return Ok(DemuxReadEvent::TemporarilyUnavailable(audio_retry_hint));
            }
            anyhow::bail!("audio component не имеет packet, EOF или retry hint");
        }

        if let Some(audio_packet) = self.pending_audio_packet.as_ref() {
            let can_emit_audio = self.video_eof
                || self.can_emit_while_peer_unavailable(CompositeComponent::Audio, audio_packet);
            if can_emit_audio {
                let Some(audio_packet) = self.pending_audio_packet.take() else {
                    anyhow::bail!("audio pending packet исчез до composite emission");
                };
                return Ok(self.emitted_packet_event(CompositeComponent::Audio, audio_packet));
            }
            if let Some(video_retry_hint) = video_retry_hint {
                return Ok(DemuxReadEvent::TemporarilyUnavailable(video_retry_hint));
            }
            anyhow::bail!("video component не имеет packet, EOF или retry hint");
        }

        if self.video_eof && self.audio_eof {
            return Ok(DemuxReadEvent::EndOfStream);
        }
        if let Some(retry_hint) = minimum_retry_hint(video_retry_hint, audio_retry_hint) {
            return Ok(DemuxReadEvent::TemporarilyUnavailable(retry_hint));
        }
        anyhow::bail!("composite read не имеет pending packet, terminal state или retry hint")
    }
}

/// Выбирает самый ранний retry среди required unavailable component-ов.
pub(super) fn minimum_retry_hint(
    video_hint: Option<DemuxRetryHint>,
    audio_hint: Option<DemuxRetryHint>,
) -> Option<DemuxRetryHint> {
    match (video_hint, audio_hint) {
        (Some(video_hint), Some(audio_hint)) => Some(video_hint.min(audio_hint)),
        (Some(video_hint), None) => Some(video_hint),
        (None, Some(audio_hint)) => Some(audio_hint),
        (None, None) => None,
    }
}
