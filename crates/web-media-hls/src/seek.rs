//! Bounded decode-safe VOD seek index из concrete demux packet evidence.

use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use media_core::{
    DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, MediaTime, Packet, PacketKeyframe, TrackKind,
};

use crate::plan::{HlsManifestSeekPoint, HlsSegmentRestartCoordinate};

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
    /// Exact manifest segment, plaintext bytes которого породили landing packet.
    pub manifest_segment: HlsManifestSeekPoint,
    /// Presentation timeline origin, которому соответствует `epoch_timestamp_origin`.
    pub timeline_origin: std::time::Duration,
    pub epoch_timestamp_origin: std::time::Duration,
    /// PTS landing packet-а, который честно публикуется в seek receipt.
    pub position: MediaTime,
    /// DTS decode point-а; для audio/packet без DTS он совпадает с PTS.
    pub decode_position: MediaTime,
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
    pub(crate) fn observe_manifest_packet(
        &mut self,
        manifest_segment: HlsManifestSeekPoint,
        timeline_origin: std::time::Duration,
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
            epoch_index: manifest_segment.epoch_index,
            restart_segment: manifest_segment.restart_segment,
            manifest_segment,
            timeline_origin,
            epoch_timestamp_origin,
            position: MediaTime::from_duration(packet.pts),
            decode_position: MediaTime::from_duration(packet.dts.unwrap_or(packet.pts)),
            kind,
        };
        self.insert_proven_anchor(anchor);
    }

    /// Focused index fixtures не строят manifest plan, поэтому дают безопасную synthetic identity.
    #[cfg(test)]
    fn observe_packet(
        &mut self,
        epoch_index: usize,
        restart_segment: HlsSegmentRestartCoordinate,
        timeline_origin: std::time::Duration,
        epoch_timestamp_origin: std::time::Duration,
        packet: &Packet,
    ) {
        let manifest_segment = HlsManifestSeekPoint {
            media_sequence: restart_segment.segment_index as u64,
            discontinuity_sequence: epoch_index as u64,
            manifest_segment_index: restart_segment.segment_index,
            epoch_index,
            restart_segment,
            timeline_start: timeline_origin,
            timeline_end: timeline_origin.saturating_add(std::time::Duration::from_secs(1)),
        };
        self.observe_manifest_packet(
            manifest_segment,
            timeline_origin,
            epoch_timestamp_origin,
            packet,
        );
    }

    /// Коммитит anchor, уже доказанный внутри offside manifest replacement-а.
    pub(crate) fn commit_proven_anchor(&mut self, anchor: HlsSeekAnchor) {
        self.insert_proven_anchor(anchor);
    }

    /// Возвращает первый packet-derived anchor initial preflight-а нужной topology.
    pub(crate) fn initial_anchor(&self, has_video: bool) -> Option<HlsSeekAnchor> {
        let required_kind = if has_video {
            HlsSeekAnchorKind::VideoRandomAccessPoint
        } else {
            HlsSeekAnchorKind::AudioPacket
        };
        self.anchors
            .iter()
            .copied()
            .find(|anchor| anchor.kind == required_kind)
    }

    /// Один owner-path сохраняет dedup, ordering и bounded compaction для любого evidence source.
    fn insert_proven_anchor(&mut self, anchor: HlsSeekAnchor) {
        if self.anchors.iter().any(|existing| {
            existing.epoch_index == anchor.epoch_index
                && existing.restart_segment == anchor.restart_segment
                && existing.kind == anchor.kind
        }) {
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
        self.compact_to_budget();
    }

    /// Сжимает index до caller-owned budget, не позволяя поздней границе замереть.
    ///
    /// Video RAP и audio packet имеют разную seek-семантику, поэтому делят budget
    /// независимо. При наличии обоих видов video получает нечётный остаток: без RAP
    /// decode-safe seek невозможен, тогда как Accurate может использовать video fallback.
    /// Неиспользованная доля одного вида остаётся доступной другому, чтобы редкий audio
    /// packet не выбрасывал тысячи полезных video anchors (и наоборот).
    fn compact_to_budget(&mut self) {
        let video_count = self
            .anchors
            .iter()
            .filter(|anchor| anchor.kind == HlsSeekAnchorKind::VideoRandomAccessPoint)
            .count();
        let audio_count = self
            .anchors
            .iter()
            .filter(|anchor| anchor.kind == HlsSeekAnchorKind::AudioPacket)
            .count();
        let (video_budget, audio_budget) =
            Self::kind_budgets(self.maximum_entries, video_count, audio_count);

        self.compact_kind_to_budget(HlsSeekAnchorKind::VideoRandomAccessPoint, video_budget);
        self.compact_kind_to_budget(HlsSeekAnchorKind::AudioPacket, audio_budget);
        debug_assert!(self.anchors.len() <= self.maximum_entries);
    }

    /// Делит общий budget справедливо, но не резервирует пустые места заранее.
    fn kind_budgets(
        maximum_entries: usize,
        video_count: usize,
        audio_count: usize,
    ) -> (usize, usize) {
        if video_count == 0 {
            return (0, audio_count.min(maximum_entries));
        }
        if audio_count == 0 {
            return (video_count.min(maximum_entries), 0);
        }

        let fair_video_budget = maximum_entries / 2 + maximum_entries % 2;
        let fair_audio_budget = maximum_entries / 2;
        let mut video_budget = video_count.min(fair_video_budget);
        let mut audio_budget = audio_count.min(fair_audio_budget);
        let mut unassigned_budget = maximum_entries.saturating_sub(video_budget + audio_budget);

        let additional_video_budget = unassigned_budget.min(video_count - video_budget);
        video_budget += additional_video_budget;
        unassigned_budget -= additional_video_budget;
        audio_budget += unassigned_budget.min(audio_count - audio_budget);

        (video_budget, audio_budget)
    }

    /// Оставляет временно равномерное покрытие одного вида anchor-а.
    ///
    /// При budget >= 2 первый и самый свежий anchors сохраняются обязательно.
    /// Внутренние точки выбираются ближе всего к равномерным временным целям между
    /// краями. При budget == 1 сохраняется свежая точка: это намеренно продолжает
    /// позднее покрытие вместо прежней необратимой заморозки на старте media.
    fn compact_kind_to_budget(&mut self, kind: HlsSeekAnchorKind, budget: usize) {
        let kind_indices = self
            .anchors
            .iter()
            .enumerate()
            .filter_map(|(index, anchor)| (anchor.kind == kind).then_some(index))
            .collect::<Vec<_>>();
        if kind_indices.len() <= budget {
            return;
        }
        if budget == 0 {
            self.anchors.retain(|anchor| anchor.kind != kind);
            return;
        }

        let retained_kind_offsets =
            Self::evenly_spaced_kind_offsets(&self.anchors, &kind_indices, budget);
        let mut next_retained_offset = 0;
        let mut current_kind_offset = 0;
        self.anchors.retain(|anchor| {
            if anchor.kind != kind {
                return true;
            }
            let retain_current = retained_kind_offsets.get(next_retained_offset).copied()
                == Some(current_kind_offset);
            current_kind_offset += 1;
            if retain_current {
                next_retained_offset += 1;
            }
            retain_current
        });
    }

    /// Выбирает offsets ближайших anchors к равномерным целям за один линейный проход.
    fn evenly_spaced_kind_offsets(
        anchors: &[HlsSeekAnchor],
        kind_indices: &[usize],
        budget: usize,
    ) -> Vec<usize> {
        let last_offset = kind_indices.len() - 1;
        if budget == 1 {
            return vec![last_offset];
        }

        let first_position = anchors[kind_indices[0]].position.as_duration().as_nanos();
        let last_position = anchors[kind_indices[last_offset]]
            .position
            .as_duration()
            .as_nanos();
        let position_span = last_position.saturating_sub(first_position);
        let mut retained_offsets = Vec::with_capacity(budget);
        retained_offsets.push(0);
        let mut search_start = 1;

        for slot in 1..budget - 1 {
            let future_slot_count = budget - 1 - slot;
            let search_end = last_offset - future_slot_count;
            let target_position =
                first_position + position_span.saturating_mul(slot as u128) / (budget - 1) as u128;
            let mut best_offset = search_start;
            let mut best_distance = u128::MAX;

            for candidate_offset in search_start..=search_end {
                let candidate_position = anchors[kind_indices[candidate_offset]]
                    .position
                    .as_duration()
                    .as_nanos();
                let candidate_distance = candidate_position.abs_diff(target_position);
                if candidate_distance < best_distance {
                    best_offset = candidate_offset;
                    best_distance = candidate_distance;
                } else if candidate_position > target_position {
                    break;
                }
            }

            retained_offsets.push(best_offset);
            search_start = best_offset + 1;
        }

        retained_offsets.push(last_offset);
        retained_offsets
    }

    /// Возвращает anchor <= target; manifest segment boundaries здесь не участвуют.
    pub(crate) fn anchor_for(&self, request: DemuxSeekRequest) -> Result<HlsSeekAnchor> {
        let required_kind = self.required_kind(request);
        self.anchor_of_kind_before(required_kind, request.timestamp)
    }

    /// Возвращает последний anchor с decode point не позже target.
    ///
    /// У reordered video PTS первого представимого RAP может быть немного позже
    /// target, хотя его настоящий DTS уже принадлежит requested decode boundary.
    pub(crate) fn anchor_of_kind_before(
        &self,
        required_kind: HlsSeekAnchorKind,
        target: std::time::Duration,
    ) -> Result<HlsSeekAnchor> {
        self.anchors
            .iter()
            .rev()
            .find(|anchor| {
                anchor.kind == required_kind && anchor.decode_position.as_duration() <= target
            })
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "HLS seek index не содержит доказанный {required_kind:?} anchor до {:?}",
                    target
                )
            })
    }

    /// Возвращает первый anchor, чей представимый timestamp не раньше target-а.
    pub(crate) fn anchor_of_kind_at_or_after(
        &self,
        required_kind: HlsSeekAnchorKind,
        target: std::time::Duration,
    ) -> Result<HlsSeekAnchor> {
        self.anchors
            .iter()
            .find(|anchor| anchor.kind == required_kind && anchor.position.as_duration() >= target)
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "HLS seek index не содержит доказанный {required_kind:?} anchor после {:?}",
                    target
                )
            })
    }

    /// Выбирает обязательный evidence kind без дублирования Accurate fallback policy.
    pub(crate) fn required_kind(&self, request: DemuxSeekRequest) -> HlsSeekAnchorKind {
        match request.mode {
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
        }
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

    use super::{HlsSeekAnchorKind, HlsSeekIndex};
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

    /// Строит доказанный video RAP, пригодный для DecodePointBefore.
    fn video_keyframe(pts: Duration) -> Packet {
        Packet::new_with_keyframe_unbounded(
            TrackId::new(2),
            TrackKind::Video,
            pts,
            Some(pts),
            PacketKeyframe::Keyframe,
            Bytes::from_static(b"idr"),
        )
    }

    /// Возвращает retained positions одного семантического вида в timeline order.
    fn retained_positions(index: &HlsSeekIndex, kind: HlsSeekAnchorKind) -> Vec<Duration> {
        index
            .anchors
            .iter()
            .filter_map(|anchor| (anchor.kind == kind).then_some(anchor.position.as_duration()))
            .collect()
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
                Duration::ZERO,
                &audio_packet(Duration::from_secs(second)),
            );
        }
        index.observe_packet(
            0,
            HlsSegmentRestartCoordinate { segment_index: 1 },
            Duration::ZERO,
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
    fn bounded_compaction_keeps_early_and_late_coverage_for_audio_and_video() {
        let mut index = HlsSeekIndex::new(4);
        for segment_index in 0..6 {
            let position = Duration::from_secs(segment_index as u64 * 30);
            let restart_segment = HlsSegmentRestartCoordinate { segment_index };
            index.observe_packet(
                0,
                restart_segment,
                Duration::ZERO,
                Duration::ZERO,
                &video_keyframe(position),
            );
            index.observe_packet(
                0,
                restart_segment,
                Duration::ZERO,
                Duration::ZERO,
                &audio_packet(position),
            );
        }

        assert_eq!(index.anchors.len(), 4);
        assert_eq!(
            retained_positions(&index, HlsSeekAnchorKind::VideoRandomAccessPoint),
            vec![Duration::ZERO, Duration::from_secs(150)]
        );
        assert_eq!(
            retained_positions(&index, HlsSeekAnchorKind::AudioPacket),
            vec![Duration::ZERO, Duration::from_secs(150)]
        );
        let late_video = index
            .anchor_for(DemuxSeekRequest::decode_point_before(Duration::from_secs(
                155,
            )))
            .expect("late decode-safe seek keeps the newest video RAP");
        assert_eq!(late_video.position.as_duration(), Duration::from_secs(150));
        let early_audio = index
            .anchor_for(DemuxSeekRequest::accurate(Duration::from_secs(15)))
            .expect("early accurate seek keeps the first audio boundary");
        assert_eq!(early_audio.position.as_duration(), Duration::ZERO);
    }

    #[test]
    fn intermediate_audio_anchors_remain_spread_across_observed_timeline() {
        let mut index = HlsSeekIndex::new(4);
        for segment_index in 0..8 {
            index.observe_packet(
                0,
                HlsSegmentRestartCoordinate { segment_index },
                Duration::ZERO,
                Duration::ZERO,
                &audio_packet(Duration::from_secs(segment_index as u64 * 10)),
            );
        }

        let positions = retained_positions(&index, HlsSeekAnchorKind::AudioPacket);
        assert_eq!(positions.len(), 4);
        assert_eq!(positions.first(), Some(&Duration::ZERO));
        assert_eq!(positions.last(), Some(&Duration::from_secs(70)));
        assert!(
            positions
                .windows(2)
                .all(|pair| pair[1].saturating_sub(pair[0]) <= Duration::from_secs(30)),
            "compaction должен сохранять полезные промежуточные точки: {positions:?}"
        );
    }

    #[test]
    fn one_entry_budget_prefers_fresh_video_rap_over_audio() {
        let mut index = HlsSeekIndex::new(1);
        for segment_index in 0..2 {
            let position = Duration::from_secs(segment_index as u64 * 30);
            let restart_segment = HlsSegmentRestartCoordinate { segment_index };
            index.observe_packet(
                0,
                restart_segment,
                Duration::ZERO,
                Duration::ZERO,
                &audio_packet(position),
            );
            index.observe_packet(
                0,
                restart_segment,
                Duration::ZERO,
                Duration::ZERO,
                &video_keyframe(position),
            );
        }

        assert_eq!(index.anchors.len(), 1);
        assert_eq!(
            index.anchors[0].kind,
            HlsSeekAnchorKind::VideoRandomAccessPoint
        );
        assert_eq!(
            index.anchors[0].position.as_duration(),
            Duration::from_secs(30)
        );
        let late_video = index
            .anchor_for(DemuxSeekRequest::decode_point_before(Duration::from_secs(
                35,
            )))
            .expect("minimal budget keeps a usable fresh video RAP");
        assert_eq!(late_video.position.as_duration(), Duration::from_secs(30));
    }

    #[test]
    fn scarce_audio_anchor_does_not_leave_video_budget_unused() {
        let mut index = HlsSeekIndex::new(6);
        for segment_index in 0..6 {
            index.observe_packet(
                0,
                HlsSegmentRestartCoordinate { segment_index },
                Duration::ZERO,
                Duration::ZERO,
                &video_keyframe(Duration::from_secs(segment_index as u64 * 10)),
            );
        }
        index.observe_packet(
            0,
            HlsSegmentRestartCoordinate { segment_index: 5 },
            Duration::ZERO,
            Duration::ZERO,
            &audio_packet(Duration::from_secs(50)),
        );

        assert_eq!(index.anchors.len(), 6);
        assert_eq!(
            retained_positions(&index, HlsSeekAnchorKind::VideoRandomAccessPoint).len(),
            5
        );
        assert_eq!(
            retained_positions(&index, HlsSeekAnchorKind::AudioPacket),
            vec![Duration::from_secs(50)]
        );
    }

    #[test]
    fn worker_consumes_preview_pinned_anchor_even_after_index_growth() {
        let mut index = HlsSeekIndex::new(2);
        for (segment_index, seconds) in [(0, 0), (1, 4)] {
            index.observe_packet(
                0,
                HlsSegmentRestartCoordinate { segment_index },
                Duration::ZERO,
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
