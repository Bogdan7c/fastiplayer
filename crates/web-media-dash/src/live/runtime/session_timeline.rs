//! Стабильная граница между source-native DASH time и локальной шкалой сессии.

use std::time::Duration;

use media_core::{
    DemuxSeekRequest, DemuxSeekResult, DemuxTrackListUpdate, MediaTime, Packet, TimelineRange,
};
use thiserror::Error;

use crate::live::{DashLiveAvailability, DashLiveSnapshot};
use crate::plan::DashPlanError;

/// Неизменяемый origin одной live-сессии.
///
/// Raw DASH timestamps остаются точными внутри planner/availability/refresh.
/// Только public demux/timeline boundary переводится в компактную шкалу сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DashLiveSessionTimeline {
    source_origin: Duration,
}

/// Typed failure не позволяет молча saturate-ить несовместимую timeline.
#[derive(Debug, Error)]
pub(super) enum DashLiveSessionTimelineError {
    /// Initial plan не содержит доказанной media boundary.
    #[error("DASH live session origin is absent from the initial plan")]
    InitialPlan(#[source] DashPlanError),
    /// Accepted source time оказался раньше неизменяемого origin сессии.
    #[error("DASH live {field} precedes the fixed session origin")]
    BeforeSessionOrigin {
        /// Нейтральное имя boundary без URL/секретов.
        field: &'static str,
    },
    /// Local→source преобразование переполнило Duration.
    #[error("DASH live {field} overflows the source timeline")]
    SourceTimelineOverflow {
        /// Нейтральное имя boundary без URL/секретов.
        field: &'static str,
    },
}

impl DashLiveSessionTimeline {
    /// Фиксирует origin по самой ранней доказанной media boundary первого snapshot-а.
    pub(super) fn from_initial_snapshot(
        snapshot: &DashLiveSnapshot,
    ) -> Result<Self, DashLiveSessionTimelineError> {
        let source_origin = snapshot
            .plan
            .earliest_planned_media_start()
            .map_err(DashLiveSessionTimelineError::InitialPlan)?;
        let timeline = Self { source_origin };
        // Initial availability обязана целиком представляться в выбранной шкале.
        timeline.availability_to_session(&snapshot.availability)?;
        Ok(timeline)
    }

    /// Переводит manifest cap/live edge в public session coordinate space.
    pub(super) fn availability_to_session(
        self,
        availability: &DashLiveAvailability,
    ) -> Result<DashLiveAvailability, DashLiveSessionTimelineError> {
        let live_edge =
            self.source_to_session(availability.live_edge.as_duration(), "live edge")?;
        let range_start = self.source_to_session(
            availability.manifest_range.start.as_duration(),
            "manifest range start",
        )?;
        let range_end = self.source_to_session(
            availability.manifest_range.end.as_duration(),
            "manifest range end",
        )?;
        Ok(DashLiveAvailability {
            live_edge: MediaTime::from_duration(live_edge),
            manifest_range: TimelineRange {
                start: MediaTime::from_duration(range_start),
                end: MediaTime::from_duration(range_end),
            },
        })
    }

    /// Переводит public seek request обратно во внутреннюю source-native шкалу.
    pub(super) fn seek_request_to_source(
        self,
        request: DemuxSeekRequest,
    ) -> Result<DemuxSeekRequest, DashLiveSessionTimelineError> {
        Ok(DemuxSeekRequest {
            timestamp: self.session_to_source(request.timestamp, "seek target")?,
            mode: request.mode,
        })
    }

    /// Возвращает seek receipt в public session coordinate space.
    pub(super) fn seek_result_to_session(
        self,
        source_result: DemuxSeekResult,
        requested_session_time: Duration,
    ) -> Result<DemuxSeekResult, DashLiveSessionTimelineError> {
        let actual_position =
            self.source_to_session(source_result.actual_position.as_duration(), "seek result")?;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(requested_session_time),
            actual_position: MediaTime::from_duration(actual_position),
            // Raw track timestamp принадлежит codec/container clock и не rebased.
            actual_track_timestamp: source_result.actual_track_timestamp,
        })
    }

    /// Переводит packet PTS/DTS, сохраняя raw track timestamps и encoded payload.
    pub(super) fn packet_to_session(
        self,
        mut packet: Packet,
    ) -> Result<Packet, DashLiveSessionTimelineError> {
        packet.pts = self.source_to_session(packet.pts, "packet PTS")?;
        packet.dts = packet
            .dts
            .map(|timestamp| self.source_to_session(timestamp, "packet DTS"))
            .transpose()?;
        Ok(packet)
    }

    /// Убирает snapshot-local durations из live track update.
    ///
    /// Public live horizon принадлежит dynamic timeline port; перенос внутренней
    /// component duration создал бы вторую, противоречивую систему координат.
    pub(super) fn track_list_update_to_session(
        self,
        mut update: DemuxTrackListUpdate,
    ) -> DemuxTrackListUpdate {
        update.duration = None;
        for track in &mut update.tracks {
            track.duration = None;
        }
        update
    }

    /// Source→session subtraction всегда checked: отрицательное время не маскируется.
    fn source_to_session(
        self,
        source_time: Duration,
        field: &'static str,
    ) -> Result<Duration, DashLiveSessionTimelineError> {
        source_time
            .checked_sub(self.source_origin)
            .ok_or(DashLiveSessionTimelineError::BeforeSessionOrigin { field })
    }

    /// Session→source addition всегда checked: overflow остаётся typed failure.
    fn session_to_source(
        self,
        session_time: Duration,
        field: &'static str,
    ) -> Result<Duration, DashLiveSessionTimelineError> {
        self.source_origin
            .checked_add(session_time)
            .ok_or(DashLiveSessionTimelineError::SourceTimelineOverflow { field })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use bytes::Bytes;
    use media_core::{
        DemuxSeekMode, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration,
        PacketKeyframe, TrackId, TrackKind,
    };

    use super::*;
    use crate::live::runtime::timeline::DashLiveTimelineCoordinator;

    /// Реальный packet boundary получает компактные PTS/DTS без mutation track clocks/payload.
    #[test]
    fn epoch_sized_packet_and_seek_round_trip_through_fixed_session_origin() {
        let origin = Duration::from_secs(1_786_370_000);
        let timeline = DashLiveSessionTimeline {
            source_origin: origin,
        };
        let packet = Packet::new_with_keyframe_unbounded(
            TrackId::new(7),
            TrackKind::Video,
            origin + Duration::from_secs(52),
            Some(origin + Duration::from_millis(51_960)),
            PacketKeyframe::from_known(true),
            Bytes::from_static(b"encoded"),
        )
        .with_duration(Duration::from_secs(1));

        let translated = timeline
            .packet_to_session(packet)
            .expect("epoch-sized packet must be representable in session time");
        assert_eq!(translated.pts, Duration::from_secs(52));
        assert_eq!(translated.dts, Some(Duration::from_millis(51_960)));
        assert_eq!(translated.data, Bytes::from_static(b"encoded"));

        let session_availability = timeline
            .availability_to_session(&DashLiveAvailability {
                live_edge: MediaTime::from_duration(origin + Duration::from_secs(60)),
                manifest_range: TimelineRange {
                    start: MediaTime::from_duration(origin),
                    end: MediaTime::from_duration(origin + Duration::from_secs(60)),
                },
            })
            .expect("epoch-sized availability must map to session time");
        let (coordinator, port) = DashLiveTimelineCoordinator::new(
            session_availability,
            true,
            false,
            DynamicMediaTimelinePortGeneration::new(
                NonZeroU64::new(1).expect("test generation must be non-zero"),
            ),
            DynamicMediaTimelineEpoch::new(0),
        )
        .expect("valid session availability");
        coordinator
            .observe_packet(&translated)
            .expect("translated packet must publish session-local evidence");
        let public_timeline = port.observe().snapshot.state;
        assert_eq!(
            public_timeline.live_edge(),
            MediaTime::from_duration(Duration::from_secs(60))
        );
        assert_eq!(
            public_timeline.seekable_range(),
            Some(TimelineRange {
                start: MediaTime::from_duration(Duration::from_secs(52)),
                end: MediaTime::from_duration(Duration::from_secs(53)),
            })
        );

        let track_update = timeline.track_list_update_to_session(DemuxTrackListUpdate {
            tracks: vec![media_core::TrackInfo {
                id: TrackId::new(1),
                kind: TrackKind::Audio,
                codec_id: "A_AAC".to_owned(),
                codec_private: None,
                time_base: None,
                duration: Some(origin + Duration::from_secs(60)),
                sample_rate: Some(48_000),
                channels: Some(2),
                video: None,
            }],
            duration: Some(origin + Duration::from_secs(60)),
        });
        assert_eq!(track_update.duration, None);
        assert_eq!(track_update.tracks[0].duration, None);

        let public_request = DemuxSeekRequest {
            timestamp: Duration::from_secs(45),
            mode: DemuxSeekMode::DecodePointBefore,
        };
        let source_request = timeline
            .seek_request_to_source(public_request)
            .expect("session seek must map to source time");
        assert_eq!(source_request.timestamp, origin + Duration::from_secs(45));
        let public_result = timeline
            .seek_result_to_session(
                DemuxSeekResult {
                    requested_position: MediaTime::from_duration(source_request.timestamp),
                    actual_position: MediaTime::from_duration(origin + Duration::from_secs(44)),
                    actual_track_timestamp: None,
                },
                public_request.timestamp,
            )
            .expect("source seek receipt must map back to session time");
        assert_eq!(
            public_result.requested_position.as_duration(),
            Duration::from_secs(45)
        );
        assert_eq!(
            public_result.actual_position.as_duration(),
            Duration::from_secs(44)
        );
    }

    /// Manifest cap и packet до fixed origin являются typed continuity failure, не saturation.
    #[test]
    fn source_time_before_origin_is_rejected_without_mutating_coordinate_space() {
        let timeline = DashLiveSessionTimeline {
            source_origin: Duration::from_secs(100),
        };
        let availability = DashLiveAvailability {
            live_edge: MediaTime::from_duration(Duration::from_secs(120)),
            manifest_range: TimelineRange {
                start: MediaTime::from_duration(Duration::from_secs(99)),
                end: MediaTime::from_duration(Duration::from_secs(120)),
            },
        };
        assert!(matches!(
            timeline.availability_to_session(&availability),
            Err(DashLiveSessionTimelineError::BeforeSessionOrigin {
                field: "manifest range start"
            })
        ));
    }
}
