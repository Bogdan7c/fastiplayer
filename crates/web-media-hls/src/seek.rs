//! Bounded decode-safe VOD seek index из concrete demux packet evidence.

use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use media_core::{
    DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, MediaTime, Packet, PacketKeyframe, TrackKind,
};

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
    pub position: MediaTime,
    pub kind: HlsSeekAnchorKind,
}

/// Shared bounded index: worker добавляет evidence, player-owner только читает.
#[derive(Debug)]
pub(crate) struct HlsSeekIndex {
    maximum_entries: usize,
    anchors: Vec<HlsSeekAnchor>,
}

impl HlsSeekIndex {
    /// Создаёт пустой provider-owned index с caller-owned budget.
    pub(crate) fn new(maximum_entries: usize) -> Self {
        Self {
            maximum_entries,
            anchors: Vec::new(),
        }
    }

    /// Добавляет только concrete RAP/audio evidence и сохраняет chronological order.
    pub(crate) fn observe_packet(&mut self, epoch_index: usize, packet: &Packet) {
        let kind = match (packet.kind, packet.keyframe) {
            (TrackKind::Video, PacketKeyframe::Keyframe) => {
                HlsSeekAnchorKind::VideoRandomAccessPoint
            }
            (TrackKind::Audio, _) => HlsSeekAnchorKind::AudioPacket,
            (TrackKind::Video, PacketKeyframe::NotKeyframe | PacketKeyframe::Unknown) => return,
        };
        let anchor = HlsSeekAnchor {
            epoch_index,
            position: MediaTime::from_duration(packet.pts),
            kind,
        };
        if self.anchors.last().copied() == Some(anchor) {
            return;
        }
        if self.anchors.len() == self.maximum_entries {
            if self.anchors.iter().all(|existing| existing.kind != kind)
                && let Some(replace_index) = self
                    .anchors
                    .iter()
                    .rposition(|existing| existing.kind != kind)
            {
                self.anchors[replace_index] = anchor;
                self.anchors
                    .sort_by_key(|existing| (existing.position, existing.epoch_index));
            }
            // После сохранения первого anchor каждого присутствующего kind index
            // перестаёт расти и никогда не вытесняет universal fallback.
            return;
        }
        self.anchors.push(anchor);
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

    /// Строит exact neutral result для синхронного player-facing preview.
    pub(crate) fn preview(&self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let anchor = self.anchor_for(request)?;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: anchor.position,
            actual_track_timestamp: None,
        })
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
