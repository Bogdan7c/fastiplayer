use std::time::Duration;

use bytes::Bytes;

use crate::{TrackId, TrackKind};

/// Минимальная единица codec data, которую demuxer передаёт pipeline-у.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// ID трека, к которому относится packet.
    pub track_id: TrackId,

    /// Тип трека для быстрой маршрутизации packet-а.
    pub kind: TrackKind,

    /// Presentation timestamp: когда кадр показывать или audio packet играть.
    pub pts: Duration,

    /// Decode timestamp, если контейнер сообщает DTS отдельно от PTS.
    pub dts: Option<Duration>,

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
            dts,
            byte_offset: None,
            keyframe,
            data,
        }
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

    use crate::{Packet, TrackId, TrackKind};

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
        assert!(packet.keyframe);
        assert_eq!(&packet.data[..], b"vp9");
    }
}
