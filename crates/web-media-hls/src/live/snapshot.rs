use std::collections::{HashMap, HashSet};
use std::time::Duration;

use hls_playlist_core::{MediaPlaylist, MediaSegment};
use media_core::{MediaTime, Packet, PacketKeyframe, TimelineRange, TrackKind};

use crate::plan::HlsComponentPlan;

/// Выбранный component, которому принадлежит independent sequence space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HlsLiveComponentKind {
    Main,
    AlternateAudio,
}

/// Stable identity segment-а внутри одного rendition lineage.
///
/// Media sequence нельзя сравнивать между renditions, поэтому component kind
/// всегда является частью ключа на coordinator boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct HlsLiveSegmentIdentity {
    pub media_sequence: u64,
    pub discontinuity_sequence: u64,
}

impl HlsLiveSegmentIdentity {
    const fn from_segment(segment: &MediaSegment) -> Self {
        Self {
            media_sequence: segment.media_sequence,
            discontinuity_sequence: segment.discontinuity_sequence,
        }
    }
}

/// Один retained segment вместе с exact segment-scoped demux epoch.
#[derive(Clone, Debug)]
pub(crate) struct HlsLiveSegmentSnapshot {
    pub identity: HlsLiveSegmentIdentity,
    pub timeline_start: Duration,
    pub timeline_end: Duration,
    pub epoch: crate::plan::HlsEpochPlan,
    continuity: HlsLiveSegmentContinuity,
}

/// Поля, которые RFC запрещает менять у уже известной media sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HlsLiveSegmentContinuity {
    uri: hls_playlist_core::ExactReference,
    byte_range: Option<hls_playlist_core::ByteRange>,
    duration: hls_playlist_core::HlsDuration,
    key: Option<hls_playlist_core::HlsKeyDeclaration>,
    initialization_map: Option<hls_playlist_core::InitializationMap>,
}

/// Immutable accepted snapshot одного выбранного rendition.
#[derive(Clone, Debug)]
pub(crate) struct HlsLiveComponentSnapshot {
    pub kind: HlsLiveComponentKind,
    pub target_duration: Duration,
    pub manifest_live_edge: Duration,
    pub end_list: bool,
    pub segments: Vec<HlsLiveSegmentSnapshot>,
}

impl HlsLiveComponentSnapshot {
    /// Принимает initial manifest и правым краем совмещает его с общей live edge.
    pub fn initial(
        kind: HlsLiveComponentKind,
        media: &MediaPlaylist,
        plan: HlsComponentPlan,
        manifest_live_edge: Duration,
    ) -> Result<Self, HlsLiveRefreshError> {
        let total_duration = plan.duration;
        let timeline_origin = manifest_live_edge
            .checked_sub(total_duration)
            .ok_or(HlsLiveRefreshError::TimelineUnderflow)?;
        Self::from_plan(kind, media, plan, timeline_origin, manifest_live_edge)
    }

    /// Валидирует RFC continuity и переносит новую sliding window на старую timeline.
    pub fn refreshed(
        &self,
        media: &MediaPlaylist,
        plan: HlsComponentPlan,
    ) -> Result<Self, HlsLiveRefreshError> {
        if media.media_sequence < self.first_media_sequence()
            || media.discontinuity_sequence < self.first_discontinuity_sequence()
        {
            return Err(HlsLiveRefreshError::SequenceRegressed);
        }

        let old_by_identity = self
            .segments
            .iter()
            .map(|segment| (segment.identity, segment))
            .collect::<HashMap<_, _>>();
        let mut new_local_start = Duration::ZERO;
        let mut temporal_anchor = None;
        for segment in &media.segments {
            let identity = HlsLiveSegmentIdentity::from_segment(segment);
            if let Some(previous) = old_by_identity.get(&identity) {
                let continuity = HlsLiveSegmentContinuity::from(segment);
                if continuity != previous.continuity {
                    return Err(HlsLiveRefreshError::RetainedSegmentMutated);
                }
                let origin = previous
                    .timeline_start
                    .checked_sub(new_local_start)
                    .ok_or(HlsLiveRefreshError::TimelineUnderflow)?;
                if temporal_anchor
                    .replace(origin)
                    .is_some_and(|other| other != origin)
                {
                    return Err(HlsLiveRefreshError::TemporalOverlapMismatch);
                }
            }
            new_local_start = new_local_start
                .checked_add(hls_duration(segment)?)
                .ok_or(HlsLiveRefreshError::TimelineOverflow)?;
        }
        let timeline_origin = temporal_anchor.ok_or(HlsLiveRefreshError::RefreshGap)?;
        let live_edge = timeline_origin
            .checked_add(plan.duration)
            .ok_or(HlsLiveRefreshError::TimelineOverflow)?;
        Self::from_plan(self.kind, media, plan, timeline_origin, live_edge)
    }

    fn from_plan(
        kind: HlsLiveComponentKind,
        media: &MediaPlaylist,
        plan: HlsComponentPlan,
        timeline_origin: Duration,
        manifest_live_edge: Duration,
    ) -> Result<Self, HlsLiveRefreshError> {
        if plan.epochs.len() != media.segments.len() {
            return Err(HlsLiveRefreshError::SegmentScopedPlanMismatch);
        }
        let mut segments = Vec::with_capacity(media.segments.len());
        let mut timeline_start = timeline_origin;
        let mut identities = HashSet::with_capacity(media.segments.len());
        for (segment, mut epoch) in media.segments.iter().zip(plan.epochs) {
            let identity = HlsLiveSegmentIdentity::from_segment(segment);
            if !identities.insert(identity) {
                return Err(HlsLiveRefreshError::DuplicateSegmentIdentity);
            }
            let duration = hls_duration(segment)?;
            let timeline_end = timeline_start
                .checked_add(duration)
                .ok_or(HlsLiveRefreshError::TimelineOverflow)?;
            epoch.timeline_start = timeline_start;
            segments.push(HlsLiveSegmentSnapshot {
                identity,
                timeline_start,
                timeline_end,
                epoch,
                continuity: HlsLiveSegmentContinuity::from(segment),
            });
            timeline_start = timeline_end;
        }
        if timeline_start != manifest_live_edge {
            return Err(HlsLiveRefreshError::ManifestDurationMismatch);
        }
        Ok(Self {
            kind,
            target_duration: Duration::from_secs(media.target_duration_seconds),
            manifest_live_edge,
            end_list: media.end_list,
            segments,
        })
    }

    fn first_media_sequence(&self) -> u64 {
        self.segments
            .first()
            .map_or(0, |segment| segment.identity.media_sequence)
    }

    fn first_discontinuity_sequence(&self) -> u64 {
        self.segments
            .first()
            .map_or(0, |segment| segment.identity.discontinuity_sequence)
    }

    pub fn retained_identities(&self) -> HashSet<HlsLiveSegmentIdentity> {
        self.segments
            .iter()
            .map(|segment| segment.identity)
            .collect()
    }
}

impl From<&MediaSegment> for HlsLiveSegmentContinuity {
    fn from(segment: &MediaSegment) -> Self {
        Self {
            uri: segment.uri.clone(),
            byte_range: segment.byte_range,
            duration: segment.duration.clone(),
            key: segment.key.clone(),
            initialization_map: segment.initialization_map.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SegmentPacketEvidence {
    decode_anchor: Option<Duration>,
    packet_end: Option<Duration>,
}

/// Packet-level proof, достаточный для video decoder restart после seek flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsLiveVideoDecodeStartEvidence {
    Proven,
    NotProven,
}

/// Rolling packet/RAP evidence, жёстко привязанное к retained segment identity.
#[derive(Debug)]
pub(crate) struct HlsLiveTimelineEvidence {
    requires_video_random_access: bool,
    by_segment: HashMap<HlsLiveSegmentIdentity, SegmentPacketEvidence>,
}

impl HlsLiveTimelineEvidence {
    pub fn new(requires_video_random_access: bool) -> Self {
        Self {
            requires_video_random_access,
            by_segment: HashMap::new(),
        }
    }

    /// Запоминает только packet, который segment-scoped demux однозначно атрибутировал.
    pub fn observe_packet(&mut self, identity: HlsLiveSegmentIdentity, packet: &Packet) {
        let video_decode_start = if packet.keyframe == PacketKeyframe::Keyframe {
            HlsLiveVideoDecodeStartEvidence::Proven
        } else {
            HlsLiveVideoDecodeStartEvidence::NotProven
        };
        self.observe_packet_with_video_decode_start(identity, packet, video_decode_start);
    }

    /// Принимает codec/container-aware proof от component demux owner-а.
    pub fn observe_packet_with_video_decode_start(
        &mut self,
        identity: HlsLiveSegmentIdentity,
        packet: &Packet,
        video_decode_start: HlsLiveVideoDecodeStartEvidence,
    ) {
        let evidence = self.by_segment.entry(identity).or_default();
        let is_decode_anchor = match packet.kind {
            TrackKind::Video => video_decode_start == HlsLiveVideoDecodeStartEvidence::Proven,
            TrackKind::Audio => !self.requires_video_random_access,
        };
        if is_decode_anchor {
            evidence.decode_anchor = Some(
                evidence
                    .decode_anchor
                    .map_or(packet.pts, |current| current.min(packet.pts)),
            );
        }
        let packet_end = packet
            .duration
            .and_then(|duration| packet.pts.checked_add(duration))
            .unwrap_or(packet.pts);
        evidence.packet_end = Some(
            evidence
                .packet_end
                .map_or(packet_end, |current| current.max(packet_end)),
        );
    }

    /// Удаляет evidence сразу после manifest eviction или observed segment expiry.
    pub fn retain_snapshot(&mut self, snapshot: &HlsLiveComponentSnapshot) {
        let retained = snapshot.retained_identities();
        self.by_segment
            .retain(|identity, _| retained.contains(identity));
    }

    pub fn expire(&mut self, identity: HlsLiveSegmentIdentity) {
        self.by_segment.remove(&identity);
    }

    /// Возвращает только непрерывный proven range; manifest coverage является cap-ом.
    pub fn proven_range(&self, snapshot: &HlsLiveComponentSnapshot) -> Option<TimelineRange> {
        let mut active_start: Option<Duration> = None;
        let mut active_end: Option<Duration> = None;
        let mut best: Option<(Duration, Duration)> = None;
        for segment in &snapshot.segments {
            let Some(evidence) = self.by_segment.get(&segment.identity) else {
                active_start = None;
                active_end = None;
                continue;
            };
            let Some(anchor) = evidence.decode_anchor else {
                active_start = None;
                active_end = None;
                continue;
            };
            let Some(packet_end) = evidence.packet_end else {
                active_start = None;
                active_end = None;
                continue;
            };
            let segment_start = anchor.max(segment.timeline_start);
            let segment_end = packet_end.min(segment.timeline_end);
            if segment_start >= segment_end {
                active_start = None;
                active_end = None;
                continue;
            }
            match (active_start, active_end) {
                (Some(start), Some(end)) if segment.timeline_start <= end => {
                    active_end = Some(end.max(segment_end));
                    best = Some((start, end.max(segment_end)));
                }
                _ => {
                    active_start = Some(segment_start);
                    active_end = Some(segment_end);
                    best = Some((segment_start, segment_end));
                }
            }
        }
        best.map(|(start, end)| TimelineRange {
            start: MediaTime::from_duration(start),
            end: MediaTime::from_duration(end),
        })
    }

    /// Находит latest retained decode anchor не позже target.
    pub fn anchor_for(
        &self,
        snapshot: &HlsLiveComponentSnapshot,
        target: Duration,
    ) -> Option<(HlsLiveSegmentIdentity, Duration)> {
        snapshot
            .segments
            .iter()
            .filter_map(|segment| {
                let anchor = self.by_segment.get(&segment.identity)?.decode_anchor?;
                (anchor <= target).then_some((segment.identity, anchor))
            })
            .max_by_key(|(_, anchor)| *anchor)
    }
}

/// Typed refresh rejection; ни один variant не содержит manifest URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HlsLiveRefreshError {
    #[error("HLS live sequence уменьшился")]
    SequenceRegressed,
    #[error("retained HLS media sequence изменила URI/range/duration")]
    RetainedSegmentMutated,
    #[error("HLS refresh не содержит temporal overlap")]
    RefreshGap,
    #[error("overlap segments дают противоречивую temporal mapping")]
    TemporalOverlapMismatch,
    #[error("HLS live timeline переполнился")]
    TimelineOverflow,
    #[error("HLS live timeline потребовал отрицательный origin")]
    TimelineUnderflow,
    #[error("segment-scoped plan не совпал с manifest segments")]
    SegmentScopedPlanMismatch,
    #[error("HLS live snapshot содержит duplicate segment identity")]
    DuplicateSegmentIdentity,
    #[error("segment-scoped plan duration не совпала с manifest")]
    ManifestDurationMismatch,
    #[error("HLS duration нарушила parser invariant")]
    InvalidDuration,
}

fn hls_duration(segment: &MediaSegment) -> Result<Duration, HlsLiveRefreshError> {
    crate::plan::parse_hls_duration(&segment.duration)
        .map_err(|_| HlsLiveRefreshError::InvalidDuration)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use hls_playlist_core::{HlsParseRequest, HlsParserLimits, HlsPlaylist, parse_hls_playlist};
    use media_core::{Packet, PacketKeyframe, TrackId, TrackKind};

    use super::*;
    use crate::HlsRequiredContainer;
    use crate::plan::{HlsComponentPlan, HlsEpochPlan};

    fn media(text: &str) -> MediaPlaylist {
        let parsed = parse_hls_playlist(HlsParseRequest::new(
            text.as_bytes(),
            Some("https://live.example.invalid/channel/index.m3u8"),
            HlsParserLimits::default(),
        ))
        .expect("valid synthetic live playlist");
        let HlsPlaylist::Media(media) = parsed else {
            panic!("expected media playlist");
        };
        media
    }

    fn segment_plan(media: &MediaPlaylist) -> HlsComponentPlan {
        let mut timeline_start = Duration::ZERO;
        let epochs = media
            .segments
            .iter()
            .map(|segment| {
                let epoch = HlsEpochPlan {
                    resources: Vec::new(),
                    timeline_start,
                };
                timeline_start = timeline_start
                    .checked_add(hls_duration(segment).expect("valid test duration"))
                    .expect("test timeline does not overflow");
                epoch
            })
            .collect();
        HlsComponentPlan::test_without_media_resources(
            HlsRequiredContainer::TransportStream,
            epochs,
            timeline_start,
        )
    }

    fn video_packet(pts: Duration, keyframe: PacketKeyframe) -> Packet {
        Packet::new_with_keyframe_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            pts,
            Some(pts),
            keyframe,
            Bytes::new(),
        )
        .with_duration(Duration::from_secs(1))
    }

    #[test]
    fn dvr_requires_retained_segment_and_proven_random_access() {
        let media = media(
            "#EXTM3U\n\
             #EXT-X-TARGETDURATION:6\n\
             #EXT-X-MEDIA-SEQUENCE:10\n\
             #EXTINF:6,\n\
             a.ts\n\
             #EXTINF:6,\n\
             b.ts\n\
             #EXTINF:6,\n\
             c.ts\n",
        );
        let snapshot = HlsLiveComponentSnapshot::initial(
            HlsLiveComponentKind::Main,
            &media,
            segment_plan(&media),
            Duration::from_secs(18),
        )
        .expect("valid initial snapshot");
        let identity = snapshot.segments[0].identity;
        let mut evidence = HlsLiveTimelineEvidence::new(true);

        assert!(evidence.proven_range(&snapshot).is_none());
        evidence.observe_packet(
            identity,
            &video_packet(Duration::from_secs(1), PacketKeyframe::NotKeyframe),
        );
        assert!(evidence.proven_range(&snapshot).is_none());
        let keyframe_packet = video_packet(Duration::from_secs(1), PacketKeyframe::Keyframe);
        evidence.observe_packet_with_video_decode_start(
            identity,
            &keyframe_packet,
            HlsLiveVideoDecodeStartEvidence::NotProven,
        );
        assert!(evidence.proven_range(&snapshot).is_none());
        evidence.observe_packet_with_video_decode_start(
            identity,
            &keyframe_packet,
            HlsLiveVideoDecodeStartEvidence::Proven,
        );
        assert!(evidence.proven_range(&snapshot).is_some());

        evidence.expire(identity);
        assert!(evidence.proven_range(&snapshot).is_none());
    }

    #[test]
    fn sliding_refresh_expires_old_evidence_and_advances_live_edge() {
        let initial_media = media(
            "#EXTM3U\n\
             #EXT-X-TARGETDURATION:6\n\
             #EXT-X-MEDIA-SEQUENCE:10\n\
             #EXTINF:6,\n\
             a.ts\n\
             #EXTINF:6,\n\
             b.ts\n\
             #EXTINF:6,\n\
             c.ts\n",
        );
        let initial = HlsLiveComponentSnapshot::initial(
            HlsLiveComponentKind::Main,
            &initial_media,
            segment_plan(&initial_media),
            Duration::from_secs(18),
        )
        .expect("valid initial snapshot");
        let expired_identity = initial.segments[0].identity;
        let mut evidence = HlsLiveTimelineEvidence::new(true);
        evidence.observe_packet(
            expired_identity,
            &video_packet(Duration::from_secs(1), PacketKeyframe::Keyframe),
        );

        let refreshed_media = media(
            "#EXTM3U\n\
             #EXT-X-TARGETDURATION:6\n\
             #EXT-X-MEDIA-SEQUENCE:11\n\
             #EXTINF:6,\n\
             b.ts\n\
             #EXTINF:6,\n\
             c.ts\n\
             #EXTINF:6,\n\
             d.ts\n",
        );
        let refreshed = initial
            .refreshed(&refreshed_media, segment_plan(&refreshed_media))
            .expect("overlapping sliding refresh");
        assert_eq!(refreshed.manifest_live_edge, Duration::from_secs(24));

        evidence.retain_snapshot(&refreshed);
        assert!(!evidence.by_segment.contains_key(&expired_identity));
        assert!(evidence.proven_range(&refreshed).is_none());
    }

    #[test]
    fn refresh_rejects_sequence_race_and_retained_key_mutation() {
        let initial_media = media(
            "#EXTM3U\n\
             #EXT-X-TARGETDURATION:6\n\
             #EXT-X-MEDIA-SEQUENCE:20\n\
             #EXT-X-KEY:METHOD=AES-128,URI=\"key-a.bin\"\n\
             #EXTINF:6,\n\
             a.ts\n\
             #EXTINF:6,\n\
             b.ts\n",
        );
        let initial = HlsLiveComponentSnapshot::initial(
            HlsLiveComponentKind::Main,
            &initial_media,
            segment_plan(&initial_media),
            Duration::from_secs(12),
        )
        .expect("valid encrypted initial snapshot");

        let regressed = media(
            "#EXTM3U\n\
             #EXT-X-TARGETDURATION:6\n\
             #EXT-X-MEDIA-SEQUENCE:19\n\
             #EXTINF:6,\n\
             stale.ts\n",
        );
        assert!(matches!(
            initial.refreshed(&regressed, segment_plan(&regressed)),
            Err(HlsLiveRefreshError::SequenceRegressed)
        ));

        let mutated_key = media(
            "#EXTM3U\n\
             #EXT-X-TARGETDURATION:6\n\
             #EXT-X-MEDIA-SEQUENCE:21\n\
             #EXT-X-KEY:METHOD=AES-128,URI=\"key-b.bin\"\n\
             #EXTINF:6,\n\
             b.ts\n\
             #EXTINF:6,\n\
             c.ts\n",
        );
        assert!(matches!(
            initial.refreshed(&mutated_key, segment_plan(&mutated_key)),
            Err(HlsLiveRefreshError::RetainedSegmentMutated)
        ));
    }
}
