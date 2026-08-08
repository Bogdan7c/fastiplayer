//! Bounded decode-safe VOD seek index из concrete demux packet evidence.

use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use media_core::{
    DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, MediaTime, Packet, PacketKeyframe, TrackKind,
};

use crate::plan::HlsSegmentRestartCoordinate;

/// Тип доказанной packet boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsSeekAnchorKind {
    /// Video packet с явным RAP/keyframe evidence.
    VideoRandomAccessPoint,
    /// Audio packet boundary для Accurate preroll.
    AudioPacket,
}

/// Один immutable anchor внутри discontinuity epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HlsSeekAnchor {
    pub epoch_index: usize,
    pub restart_segment: HlsSegmentRestartCoordinate,
    pub epoch_timestamp_origin: std::time::Duration,
    pub position: MediaTime,
    pub kind: HlsSeekAnchorKind,
}

/// Preview-pinned decision не позволяет растущему index-у менять worker landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HlsSeekDecision {
    request: DemuxSeekRequest,
    anchor: HlsSeekAnchor,
}

/// Shared bounded index: worker добавляет evidence, player-owner только читает.
#[derive(Debug)]
pub(crate) struct HlsSeekIndex {
    maximum_entries: usize,
    anchors: Vec<HlsSeekAnchor>,
    pending_preview: Option<HlsSeekDecision>,
}

impl HlsSeekIndex {
    /// Создаёт пустой provider-owned index с caller-owned budget.
    pub(crate) fn new(maximum_entries: usize) -> Self {
        Self {
            maximum_entries,
            anchors: Vec::new(),
            pending_preview: None,
        }
    }

    /// Добавляет первый concrete RAP/audio anchor каждого actual media segment-а.
    pub(crate) fn observe_packet(
        &mut self,
        epoch_index: usize,
        restart_segment: HlsSegmentRestartCoordinate,
        epoch_timestamp_origin: std::time::Duration,
        packet: &Packet,
    ) {
        let kind = match (packet.kind, packet.keyframe) {
            (TrackKind::Video, PacketKeyframe::Keyframe) => {
                HlsSeekAnchorKind::VideoRandomAccessPoint
            }
            (TrackKind::Audio, _) => HlsSeekAnchorKind::AudioPacket,
            (TrackKind::Video, PacketKeyframe::NotKeyframe | PacketKeyframe::Unknown) => return,
        };
        let anchor = HlsSeekAnchor {
            epoch_index,
            restart_segment,
            epoch_timestamp_origin,
            position: MediaTime::from_duration(packet.pts),
            kind,
        };
        if self.anchors.iter().any(|existing| {
            existing.epoch_index == epoch_index
                && existing.restart_segment == restart_segment
                && existing.kind == kind
        }) {
            return;
        }
        if self.anchors.len() == self.maximum_entries {
            return;
        }
        self.anchors.push(anchor);
        self.anchors.sort_by_key(|existing| {
            (
                existing.position,
                existing.epoch_index,
                existing.restart_segment.segment_index,
            )
        });
    }

    /// Возвращает anchor <= target; manifest segment boundaries здесь не участвуют.
    pub(crate) fn anchor_for(&self, request: DemuxSeekRequest) -> Result<HlsSeekAnchor> {
        let required_kind = match request.mode {
            DemuxSeekMode::DecodePointBefore | DemuxSeekMode::Preview => {
                HlsSeekAnchorKind::VideoRandomAccessPoint
            }
            DemuxSeekMode::Accurate => {
                if self
                    .anchors
                    .iter()
                    .any(|anchor| anchor.kind == HlsSeekAnchorKind::AudioPacket)
                {
                    HlsSeekAnchorKind::AudioPacket
                } else {
                    HlsSeekAnchorKind::VideoRandomAccessPoint
                }
            }
        };
        self.anchors
            .iter()
            .rev()
            .find(|anchor| {
                anchor.kind == required_kind && anchor.position.as_duration() <= request.timestamp
            })
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "HLS seek index не содержит доказанный {required_kind:?} anchor до {:?}",
                    request.timestamp
                )
            })
    }

    /// Публикует preview и atomically pin-ит тот же exact anchor для worker-а.
    pub(crate) fn preview_and_pin(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let anchor = self.anchor_for(request)?;
        self.pending_preview = Some(HlsSeekDecision { request, anchor });
        Ok(Self::result_for_anchor(request, anchor))
    }

    /// Worker переиспользует matching preview decision либо выбирает current anchor для иного seek.
    ///
    /// Decision остаётся доступным для повторной latest-only команды с тем же request:
    /// controller не видит worker generation и не должен терять pin при rapid duplicate seek.
    pub(crate) fn anchor_for_worker(&mut self, request: DemuxSeekRequest) -> Result<HlsSeekAnchor> {
        if let Some(decision) = self
            .pending_preview
            .filter(|decision| decision.request == request)
        {
            return Ok(decision.anchor);
        }
        self.anchor_for(request)
    }

    /// Один constructor гарантирует одинаковый neutral result для preview и worker-а.
    pub(crate) fn result_for_anchor(
        request: DemuxSeekRequest,
        anchor: HlsSeekAnchor,
    ) -> DemuxSeekResult {
        DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: anchor.position,
            actual_track_timestamp: None,
        }
    }

    /// Initial runtime seekable только после concrete required packet evidence.
    pub(crate) fn has_required_initial_anchor(&self, has_video: bool) -> bool {
        let required = if has_video {
            HlsSeekAnchorKind::VideoRandomAccessPoint
        } else {
            HlsSeekAnchorKind::AudioPacket
        };
        self.anchors.iter().any(|anchor| anchor.kind == required)
    }
}

/// Shared index handle, не раскрывающий mutable storage за пределы HLS runtime.
#[derive(Clone, Debug)]
pub(crate) struct SharedHlsSeekIndex(Arc<Mutex<HlsSeekIndex>>);

impl SharedHlsSeekIndex {
    /// Создаёт отдельный component index.
    pub(crate) fn new(maximum_entries: usize) -> Self {
        Self(Arc::new(Mutex::new(HlsSeekIndex::new(maximum_entries))))
    }

    /// Восстанавливает owned state после poison только для детерминированного shutdown/error path.
    pub(crate) fn lock(&self) -> MutexGuard<'_, HlsSeekIndex> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use media_core::{DemuxSeekRequest, Packet, PacketKeyframe, TrackId, TrackKind};

    use super::HlsSeekIndex;
    use crate::plan::HlsSegmentRestartCoordinate;

    fn audio_packet(pts: Duration) -> Packet {
        Packet::new_with_keyframe_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            pts,
            Some(pts),
            PacketKeyframe::Unknown,
            Bytes::from_static(b"aac"),
        )
    }

    #[test]
    fn dense_audio_packets_coalesce_to_first_anchor_per_restart_segment() {
        let mut index = HlsSeekIndex::new(2);
        let first_segment = HlsSegmentRestartCoordinate { segment_index: 0 };
        for second in 0..30 {
            index.observe_packet(
                0,
                first_segment,
                Duration::ZERO,
                &audio_packet(Duration::from_secs(second)),
            );
        }
        index.observe_packet(
            0,
            HlsSegmentRestartCoordinate { segment_index: 1 },
            Duration::ZERO,
            &audio_packet(Duration::from_secs(30)),
        );

        assert_eq!(index.anchors.len(), 2);
        assert_eq!(index.anchors[0].position.as_duration(), Duration::ZERO);
        assert_eq!(
            index.anchors[1].position.as_duration(),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn worker_consumes_preview_pinned_anchor_even_after_index_growth() {
        let mut index = HlsSeekIndex::new(8);
        for (segment_index, seconds) in [(0, 0), (1, 4)] {
            index.observe_packet(
                0,
                HlsSegmentRestartCoordinate { segment_index },
                Duration::ZERO,
                &audio_packet(Duration::from_secs(seconds)),
            );
        }
        let request = DemuxSeekRequest::accurate(Duration::from_secs(10));
        let preview = index.preview_and_pin(request).expect("pin HLS preview");
        assert_eq!(
            preview.actual_position.as_duration(),
            Duration::from_secs(4)
        );
        index.observe_packet(
            0,
            HlsSegmentRestartCoordinate { segment_index: 2 },
            Duration::ZERO,
            &audio_packet(Duration::from_secs(8)),
        );

        let pinned = index
            .anchor_for_worker(request)
            .expect("consume matching preview decision");
        assert_eq!(pinned.position.as_duration(), Duration::from_secs(4));
        let repeated = index
            .anchor_for_worker(request)
            .expect("rapid duplicate worker keeps the pinned decision");
        assert_eq!(repeated.position.as_duration(), Duration::from_secs(4));
        let different_request = DemuxSeekRequest::accurate(Duration::from_secs(9));
        let current = index
            .anchor_for_worker(different_request)
            .expect("different receipted request selects current index");
        assert_eq!(current.position.as_duration(), Duration::from_secs(8));
    }
}
