use std::time::Duration;

use bytes::Bytes;

use crate::{TrackDuration, TrackId, TrackKind, TrackTimestamp};

/// Минимальная единица codec data, которую demuxer передаёт pipeline-у.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// ID трека, к которому относится packet.
    pub track_id: TrackId,

    /// Тип трека для быстрой маршрутизации packet-а.
    pub kind: TrackKind,

    /// Presentation timestamp: когда кадр показывать или audio packet играть.
    pub pts: Duration,

    /// Исходный signed PTS в track time base до нормализации в media timeline.
    pub track_pts: Option<TrackTimestamp>,

    /// Decode timestamp, если контейнер сообщает DTS отдельно от PTS.
    pub dts: Option<Duration>,

    /// Исходный signed DTS, который container/backend отдал на packet boundary.
    pub track_dts: Option<TrackTimestamp>,

    /// Длительность packet-а, если контейнер смог её сообщить.
    pub duration: Option<Duration>,

    /// Исходная duration в track time base до нормализации в media timeline.
    pub track_duration: Option<TrackDuration>,

    /// Безопасная container/source byte-позиция для повторного demux seek-а.
    pub byte_offset: Option<u64>,

    /// Признак ключевого кадра для video packets.
    pub keyframe: bool,

    /// Сырые codec bytes: VP9 frame, Opus packet и т.д.
    pub data: Bytes,
}

impl Packet {
    /// Создаёт packet с явными timestamp-ами и codec bytes.
    #[must_use]
    pub const fn new(
        track_id: TrackId,
        kind: TrackKind,
        pts: Duration,
        dts: Option<Duration>,
        keyframe: bool,
        data: Bytes,
    ) -> Self {
        Self {
            track_id,
            kind,
            pts,
            track_pts: None,
            dts,
            track_dts: None,
            duration: None,
            track_duration: None,
            byte_offset: None,
            keyframe,
            data,
        }
    }

    /// Создаёт копию packet-а с исходными signed timestamp-ами container track-а.
    #[must_use]
    pub const fn with_track_timestamps(
        mut self,
        track_pts: Option<TrackTimestamp>,
        track_dts: Option<TrackTimestamp>,
    ) -> Self {
        self.track_pts = match track_pts {
            Some(track_pts) => Some(track_pts.with_track_id(self.track_id)),
            None => None,
        };
        self.track_dts = match track_dts {
            Some(track_dts) => Some(track_dts.with_track_id(self.track_id)),
            None => None,
        };
        self
    }

    /// Создаёт копию packet-а с новым track id и согласованными raw timestamp track id.
    #[must_use]
    pub const fn with_track_id(mut self, track_id: TrackId) -> Self {
        self.track_id = track_id;

        if let Some(track_pts) = self.track_pts {
            self.track_pts = Some(track_pts.with_track_id(track_id));
        }

        if let Some(track_dts) = self.track_dts {
            self.track_dts = Some(track_dts.with_track_id(track_id));
        }

        if let Some(track_duration) = self.track_duration {
            self.track_duration = Some(track_duration.with_track_id(track_id));
        }

        self
    }

    /// Сравнивает presentation order без преждевременного clamp-а отрицательных raw PTS.
    #[must_use]
    pub fn presentation_order_cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.track_pts, other.track_pts) {
            (Some(left_timestamp), Some(right_timestamp)) => {
                left_timestamp.cmp_timeline_position(right_timestamp)
            }
            _ => self.pts.cmp(&other.pts),
        }
    }

    /// Создаёт копию packet-а с container duration.
    #[must_use]
    pub const fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Создаёт копию packet-а с исходной duration container track-а.
    #[must_use]
    pub const fn with_track_duration(mut self, track_duration: TrackDuration) -> Self {
        self.track_duration = Some(track_duration.with_track_id(self.track_id));
        self
    }

    /// Создаёт копию packet-а с безопасной byte-позицией контейнера.
    #[must_use]
    pub const fn with_byte_offset(mut self, byte_offset: u64) -> Self {
        self.byte_offset = Some(byte_offset);
        self
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use crate::{Packet, TimeBase, TrackDuration, TrackId, TrackKind, TrackTimestamp};

    #[test]
    fn packet_keeps_track_timestamp_and_payload() {
        let packet = Packet::new(
            TrackId::new(7),
            TrackKind::Video,
            Duration::from_millis(42),
            None,
            true,
            Bytes::from_static(b"vp9"),
        );

        assert_eq!(packet.track_id, TrackId::new(7));
        assert_eq!(packet.byte_offset, None);
        assert_eq!(packet.pts, Duration::from_millis(42));
        assert_eq!(packet.track_pts, None);
        assert_eq!(packet.duration, None);
        assert_eq!(packet.track_duration, None);
        assert!(packet.keyframe);
        assert_eq!(&packet.data[..], b"vp9");
    }

    #[test]
    fn packet_can_keep_container_duration() {
        let packet = Packet::new(
            TrackId::new(7),
            TrackKind::Audio,
            Duration::from_millis(42),
            None,
            false,
            Bytes::from_static(b"audio"),
        )
        .with_duration(Duration::from_millis(20));

        assert_eq!(packet.duration, Some(Duration::from_millis(20)));
    }

    #[test]
    fn packet_can_keep_raw_track_duration_next_to_media_duration() {
        let time_base = TimeBase::new(1, 48_000).expect("valid time base");
        let track_duration = TrackDuration::new(TrackId::new(7), 960, time_base);

        let packet = Packet::new(
            TrackId::new(7),
            TrackKind::Audio,
            Duration::from_millis(42),
            None,
            false,
            Bytes::from_static(b"audio"),
        )
        .with_duration(Duration::from_millis(20))
        .with_track_duration(track_duration);

        assert_eq!(packet.duration, Some(Duration::from_millis(20)));
        assert_eq!(packet.track_duration, Some(track_duration));
        assert_eq!(
            packet
                .track_duration
                .expect("raw duration should be present")
                .to_media_duration()
                .as_duration(),
            Duration::from_millis(20)
        );
    }

    #[test]
    fn packet_keeps_raw_track_timestamps_next_to_media_time() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let track_pts = TrackTimestamp::new(TrackId::new(7), -25, time_base);
        let track_dts = TrackTimestamp::new(TrackId::new(7), -50, time_base);

        let packet = Packet::new(
            TrackId::new(7),
            TrackKind::Video,
            Duration::ZERO,
            None,
            true,
            Bytes::from_static(b"vp9"),
        )
        .with_track_timestamps(Some(track_pts), Some(track_dts));

        assert_eq!(packet.pts, Duration::ZERO);
        assert_eq!(packet.track_pts, Some(track_pts));
        assert_eq!(packet.track_dts, Some(track_dts));
    }

    #[test]
    fn packet_track_id_remap_updates_raw_timestamp_owner() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let packet = Packet::new(
            TrackId::new(1),
            TrackKind::Audio,
            Duration::from_millis(10),
            None,
            false,
            Bytes::from_static(b"audio"),
        )
        .with_track_timestamps(
            Some(TrackTimestamp::new(TrackId::new(1), 10, time_base)),
            Some(TrackTimestamp::new(TrackId::new(1), 10, time_base)),
        );

        let remapped_packet = packet.with_track_id(TrackId::new(2));

        assert_eq!(remapped_packet.track_id, TrackId::new(2));
        assert_eq!(
            remapped_packet
                .track_pts
                .expect("raw pts should be remapped")
                .track_id,
            TrackId::new(2)
        );
        assert_eq!(
            remapped_packet
                .track_dts
                .expect("raw dts should be remapped")
                .track_id,
            TrackId::new(2)
        );
    }

    #[test]
    fn packet_track_id_remap_updates_raw_duration_owner() {
        let time_base = TimeBase::new(1, 48_000).expect("valid time base");
        let packet = Packet::new(
            TrackId::new(1),
            TrackKind::Audio,
            Duration::from_millis(10),
            None,
            false,
            Bytes::from_static(b"audio"),
        )
        .with_track_duration(TrackDuration::new(TrackId::new(1), 960, time_base));

        let remapped_packet = packet.with_track_id(TrackId::new(2));

        assert_eq!(remapped_packet.track_id, TrackId::new(2));
        assert_eq!(
            remapped_packet
                .track_duration
                .expect("raw duration should be remapped")
                .track_id,
            TrackId::new(2)
        );
    }

    #[test]
    fn packet_presentation_order_uses_raw_signed_timestamps_when_available() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let earlier_packet = Packet::new(
            TrackId::new(1),
            TrackKind::Video,
            Duration::ZERO,
            None,
            true,
            Bytes::from_static(b"earlier"),
        )
        .with_track_timestamps(
            Some(TrackTimestamp::new(TrackId::new(1), -25, time_base)),
            None,
        );
        let later_packet = Packet::new(
            TrackId::new(2),
            TrackKind::Audio,
            Duration::ZERO,
            None,
            false,
            Bytes::from_static(b"later"),
        )
        .with_track_timestamps(
            Some(TrackTimestamp::new(TrackId::new(2), -10, time_base)),
            None,
        );

        assert_eq!(
            earlier_packet.presentation_order_cmp(&later_packet),
            std::cmp::Ordering::Less
        );
    }
}
