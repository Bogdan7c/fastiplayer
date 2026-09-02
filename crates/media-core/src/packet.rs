use std::time::Duration;

use bytes::Bytes;

use crate::{
    ExactPresentationWindow, PacketPresentationWindow, PacketPresentationWindowAssignmentError,
    TrackDuration, TrackId, TrackKind, TrackTimestamp,
    presentation_window::validate_packet_track_clock,
};

/// Keyframe-классификация video packet-а на границе demux -> player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketKeyframe {
    /// Packet точно является keyframe/decode-start.
    Keyframe,

    /// Packet точно не является keyframe и требует предыдущих reference frames.
    NotKeyframe,

    /// Demuxer не смог надёжно классифицировать packet.
    Unknown,
}

impl PacketKeyframe {
    /// Строит typed состояние из старого container/probe bool, когда он известен.
    #[must_use]
    pub const fn from_known(is_keyframe: bool) -> Self {
        if is_keyframe {
            Self::Keyframe
        } else {
            Self::NotKeyframe
        }
    }

    /// Возвращает `Some(bool)` только для надёжно классифицированного packet-а.
    #[must_use]
    pub const fn as_known_bool(self) -> Option<bool> {
        match self {
            Self::Keyframe => Some(true),
            Self::NotKeyframe => Some(false),
            Self::Unknown => None,
        }
    }

    /// Проверяет, что packet точно является keyframe.
    #[must_use]
    pub const fn is_known_keyframe(self) -> bool {
        matches!(self, Self::Keyframe)
    }
}

impl From<bool> for PacketKeyframe {
    /// Сохраняет compatibility с call-site-ами, где keyframe уже точно известен.
    fn from(is_keyframe: bool) -> Self {
        Self::from_known(is_keyframe)
    }
}

/// Источник decoder configuration, доступный рядом с video decode-start packet-ом.
///
/// `PacketKeyframe` доказывает random-access picture, а этот контракт отдельно
/// сообщает, переживёт ли packet decoder reset без ранее принятого codec config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketDecodeStartInitialization {
    /// Decoder должен получить configuration из track metadata или прошлых packet-ов.
    RequiresTrackConfiguration,

    /// Packet содержит required configuration перед собственным decode-start picture.
    IncludesInBandConfiguration,
}

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

    /// Exact optional logical-input position, где начинается encoded packet/sample.
    ///
    /// Coordinate space задаёт backend; значение само по себе не доказывает RAP,
    /// initialization context или допустимую standalone seek boundary.
    pub byte_offset: Option<u64>,

    /// Явная keyframe-классификация для video packets.
    pub keyframe: PacketKeyframe,

    /// Отдельное доказательство self-contained decoder initialization.
    decode_start_initialization: PacketDecodeStartInitialization,

    /// Точная граница показа, которой владеет neutral packet boundary.
    presentation_window: PacketPresentationWindow,

    /// Сырые codec bytes: VP9 frame, Opus packet и т.д.
    pub data: Bytes,
}

impl Packet {
    /// Создаёт packet без точного ограничения presentation interval.
    #[must_use]
    pub const fn new_unbounded(
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
            keyframe: PacketKeyframe::from_known(keyframe),
            decode_start_initialization:
                PacketDecodeStartInitialization::RequiresTrackConfiguration,
            presentation_window: PacketPresentationWindow::Unbounded,
            data,
        }
    }

    /// Создаёт unbounded packet с явной трёхсостоянийной keyframe-классификацией.
    #[must_use]
    pub const fn new_with_keyframe_unbounded(
        track_id: TrackId,
        kind: TrackKind,
        pts: Duration,
        dts: Option<Duration>,
        keyframe: PacketKeyframe,
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
            decode_start_initialization:
                PacketDecodeStartInitialization::RequiresTrackConfiguration,
            presentation_window: PacketPresentationWindow::Unbounded,
            data,
        }
    }

    /// Возвращает точное presentation-ограничение без раскрытия внутреннего хранения.
    #[must_use]
    pub const fn presentation_window(&self) -> PacketPresentationWindow {
        self.presentation_window
    }

    /// Возвращает typed evidence о configuration для decoder reset boundary.
    #[must_use]
    pub const fn decode_start_initialization(&self) -> PacketDecodeStartInitialization {
        self.decode_start_initialization
    }

    /// Присоединяет доказательство владельца codec/container packetization.
    #[must_use]
    pub const fn with_decode_start_initialization(
        mut self,
        initialization: PacketDecodeStartInitialization,
    ) -> Self {
        self.decode_start_initialization = initialization;
        self
    }

    /// Присоединяет заранее проверенное exact-окно к согласованному packet track clock.
    pub fn try_with_bounded_presentation_window(
        mut self,
        window: ExactPresentationWindow,
    ) -> Result<Self, PacketPresentationWindowAssignmentError> {
        let window_track_id = window.start().track_id;
        if self.track_id != window_track_id {
            return Err(PacketPresentationWindowAssignmentError::TrackMismatch {
                packet_track_id: self.track_id,
                window_track_id,
            });
        }

        let track_pts = self
            .track_pts
            .ok_or(PacketPresentationWindowAssignmentError::MissingPacketPresentationTimestamp)?;
        validate_packet_track_clock(
            self.track_id,
            track_pts.track_id,
            track_pts.time_base,
            window,
        )?;

        if let Some(track_dts) = self.track_dts {
            validate_packet_track_clock(
                self.track_id,
                track_dts.track_id,
                track_dts.time_base,
                window,
            )?;
        }

        if let Some(track_duration) = self.track_duration {
            validate_packet_track_clock(
                self.track_id,
                track_duration.track_id,
                track_duration.time_base,
                window,
            )?;
        }

        self.presentation_window = PacketPresentationWindow::Bounded(window);
        Ok(self)
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

        self.presentation_window = self.presentation_window.with_track_id(track_id);

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

    /// Добавляет доказанную backend-ом logical-input позицию packet/sample origin.
    ///
    /// Метод не превращает origin в самостоятельную seek boundary: required init,
    /// decoder context и random-access evidence остаются ответственностью владельца container-а.
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

    use crate::{
        ExactPresentationWindow, Packet, PacketDecodeStartInitialization, PacketKeyframe,
        PacketPresentationWindow, PacketPresentationWindowAssignmentError, TimeBase, TrackDuration,
        TrackId, TrackKind, TrackTimestamp,
    };

    #[test]
    fn packet_keeps_track_timestamp_and_payload() {
        let packet = Packet::new_unbounded(
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
        assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
        assert_eq!(&packet.data[..], b"vp9");
    }

    #[test]
    fn packet_can_keep_unknown_keyframe_classification() {
        let packet = Packet::new_with_keyframe_unbounded(
            TrackId::new(7),
            TrackKind::Video,
            Duration::from_millis(42),
            None,
            PacketKeyframe::Unknown,
            Bytes::from_static(b"vp9"),
        );

        assert_eq!(packet.keyframe, PacketKeyframe::Unknown);
        assert_eq!(packet.keyframe.as_known_bool(), None);
    }

    #[test]
    fn packet_keeps_decode_start_initialization_separate_from_keyframe() {
        let packet = Packet::new_with_keyframe_unbounded(
            TrackId::new(7),
            TrackKind::Video,
            Duration::from_millis(42),
            None,
            PacketKeyframe::Keyframe,
            Bytes::from_static(b"annex-b"),
        );
        assert_eq!(
            packet.decode_start_initialization(),
            PacketDecodeStartInitialization::RequiresTrackConfiguration
        );

        let self_contained_packet = packet.with_decode_start_initialization(
            PacketDecodeStartInitialization::IncludesInBandConfiguration,
        );
        assert_eq!(
            self_contained_packet.decode_start_initialization(),
            PacketDecodeStartInitialization::IncludesInBandConfiguration
        );
        assert_eq!(self_contained_packet.keyframe, PacketKeyframe::Keyframe);
    }

    #[test]
    fn packet_can_keep_container_duration() {
        let packet = Packet::new_unbounded(
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

        let packet = Packet::new_unbounded(
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

        let packet = Packet::new_unbounded(
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
        let packet = Packet::new_unbounded(
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
        let packet = Packet::new_unbounded(
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
        let earlier_packet = Packet::new_unbounded(
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
        let later_packet = Packet::new_unbounded(
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

    #[test]
    fn unbounded_constructor_records_explicit_unbounded_window() {
        let packet = Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            Duration::from_millis(10),
            None,
            true,
            Bytes::from_static(b"video"),
        );

        assert_eq!(
            packet.presentation_window(),
            PacketPresentationWindow::Unbounded
        );
    }

    #[test]
    fn packet_accepts_bounded_window_for_same_track_clock() {
        let track_id = TrackId::new(1);
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let window = ExactPresentationWindow::new(
            TrackTimestamp::new(track_id, 10, time_base),
            TrackTimestamp::new(track_id, 20, time_base),
        )
        .expect("test window should be valid");
        let packet = Packet::new_unbounded(
            track_id,
            TrackKind::Video,
            Duration::from_millis(10),
            None,
            true,
            Bytes::from_static(b"video"),
        )
        .with_track_timestamps(Some(TrackTimestamp::new(track_id, 10, time_base)), None)
        .try_with_bounded_presentation_window(window)
        .expect("matching packet and window should be accepted");

        assert_eq!(
            packet.presentation_window(),
            PacketPresentationWindow::Bounded(window)
        );
    }

    #[test]
    fn packet_without_raw_pts_rejects_bounded_window_and_reusable_value_stays_unbounded() {
        let track_id = TrackId::new(1);
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let window = ExactPresentationWindow::new(
            TrackTimestamp::new(track_id, 10, time_base),
            TrackTimestamp::new(track_id, 20, time_base),
        )
        .expect("test window should be valid");
        let packet = Packet::new_unbounded(
            track_id,
            TrackKind::Video,
            Duration::from_millis(10),
            None,
            true,
            Bytes::from_static(b"video"),
        );
        let reusable_packet = packet.clone();

        let error = packet
            .try_with_bounded_presentation_window(window)
            .expect_err("packet without raw PTS should be rejected");

        assert_eq!(
            error,
            PacketPresentationWindowAssignmentError::MissingPacketPresentationTimestamp
        );
        assert_eq!(
            reusable_packet.presentation_window(),
            PacketPresentationWindow::Unbounded
        );

        let bounded_packet = reusable_packet
            .with_track_timestamps(Some(TrackTimestamp::new(track_id, 10, time_base)), None)
            .try_with_bounded_presentation_window(window)
            .expect("the same packet value with raw PTS should be accepted");
        assert_eq!(
            bounded_packet.presentation_window(),
            PacketPresentationWindow::Bounded(window)
        );
    }

    #[test]
    fn packet_rejects_bounded_window_for_different_track() {
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let window = ExactPresentationWindow::new(
            TrackTimestamp::new(TrackId::new(2), 10, time_base),
            TrackTimestamp::new(TrackId::new(2), 20, time_base),
        )
        .expect("test window should be valid");
        let packet = Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            Duration::from_millis(10),
            None,
            true,
            Bytes::from_static(b"video"),
        );

        let error = packet
            .try_with_bounded_presentation_window(window)
            .expect_err("different packet and window tracks should be rejected");

        assert!(matches!(
            error,
            PacketPresentationWindowAssignmentError::TrackMismatch { .. }
        ));
    }

    #[test]
    fn packet_rejects_bounded_window_for_different_raw_time_base() {
        let track_id = TrackId::new(1);
        let packet_time_base = TimeBase::new(1, 90_000).expect("valid packet time base");
        let window_time_base = TimeBase::new(1, 1_000).expect("valid window time base");
        let window = ExactPresentationWindow::new(
            TrackTimestamp::new(track_id, 10, window_time_base),
            TrackTimestamp::new(track_id, 20, window_time_base),
        )
        .expect("test window should be valid");
        let packet = Packet::new_unbounded(
            track_id,
            TrackKind::Video,
            Duration::from_millis(10),
            None,
            true,
            Bytes::from_static(b"video"),
        )
        .with_track_timestamps(
            Some(TrackTimestamp::new(track_id, 900, packet_time_base)),
            None,
        );

        let error = packet
            .try_with_bounded_presentation_window(window)
            .expect_err("different packet and window time bases should be rejected");

        assert!(matches!(
            error,
            PacketPresentationWindowAssignmentError::TimeBaseMismatch { .. }
        ));
    }

    #[test]
    fn packet_track_id_remap_preserves_payload_timing_and_bounded_window() {
        let original_track_id = TrackId::new(1);
        let remapped_track_id = TrackId::new(2);
        let time_base = TimeBase::new(1, 1_000).expect("valid time base");
        let window = ExactPresentationWindow::new(
            TrackTimestamp::new(original_track_id, 10, time_base),
            TrackTimestamp::new(original_track_id, 20, time_base),
        )
        .expect("test window should be valid");
        let original_packet = Packet::new_with_keyframe_unbounded(
            original_track_id,
            TrackKind::Video,
            Duration::from_millis(10),
            Some(Duration::from_millis(8)),
            PacketKeyframe::Unknown,
            Bytes::from_static(b"video"),
        )
        .with_track_timestamps(
            Some(TrackTimestamp::new(original_track_id, 10, time_base)),
            Some(TrackTimestamp::new(original_track_id, 8, time_base)),
        )
        .with_duration(Duration::from_millis(4))
        .with_track_duration(TrackDuration::new(original_track_id, 4, time_base))
        .with_byte_offset(42)
        .try_with_bounded_presentation_window(window)
        .expect("matching packet and window should be accepted");

        let remapped_packet = original_packet.clone().with_track_id(remapped_track_id);
        let PacketPresentationWindow::Bounded(remapped_window) =
            remapped_packet.presentation_window()
        else {
            panic!("bounded window should remain bounded after track remap");
        };

        assert_eq!(remapped_packet.track_id, remapped_track_id);
        assert_eq!(remapped_window.start().track_id, remapped_track_id);
        assert_eq!(remapped_window.end_exclusive().track_id, remapped_track_id);
        assert_eq!(remapped_window.start().units, window.start().units);
        assert_eq!(
            remapped_window.end_exclusive().units,
            window.end_exclusive().units
        );
        assert_eq!(remapped_packet.kind, original_packet.kind);
        assert_eq!(remapped_packet.pts, original_packet.pts);
        assert_eq!(remapped_packet.dts, original_packet.dts);
        assert_eq!(remapped_packet.duration, original_packet.duration);
        assert_eq!(remapped_packet.byte_offset, original_packet.byte_offset);
        assert_eq!(remapped_packet.keyframe, original_packet.keyframe);
        assert_eq!(remapped_packet.data, original_packet.data);
    }
}
