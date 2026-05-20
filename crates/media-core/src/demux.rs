use std::time::Duration;

use crate::{MediaTime, Packet, TimelineNotSeekableReason, TrackInfo, TrackTimestamp};

/// Результат container-level seek без привязки к конкретной реализации demuxer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxSeekResult {
    /// Позиция, которую player запросил на нормализованной media timeline.
    pub requested_position: MediaTime,

    /// Позиция, на которую container фактически переставил чтение.
    pub actual_position: MediaTime,

    /// Сырой timestamp track-а, который container/backend вернул после seek.
    pub actual_track_timestamp: Option<TrackTimestamp>,
}

/// Режим container-level seek-а без привязки к конкретному backend-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemuxSeekMode {
    /// Финальный seek: demuxer должен выбрать максимально точную позицию до target.
    Accurate,

    /// Seek к безопасной decode-точке не позже target для decoder-а после flush.
    DecodePointBefore,

    /// Preview seek: допустим более грубый, но быстрый прыжок к пригодной decode-точке.
    Preview,
}

/// Полный запрос seek-а для demuxer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxSeekRequest {
    /// Целевая позиция на media timeline.
    pub timestamp: Duration,

    /// Требуемый режим скорости/точности.
    pub mode: DemuxSeekMode,
}

impl DemuxSeekRequest {
    /// Создаёт финальный точный seek-запрос.
    #[must_use]
    pub const fn accurate(timestamp: Duration) -> Self {
        Self {
            timestamp,
            mode: DemuxSeekMode::Accurate,
        }
    }

    /// Создаёт seek-запрос к decode-safe точке до целевой позиции.
    #[must_use]
    pub const fn decode_point_before(timestamp: Duration) -> Self {
        Self {
            timestamp,
            mode: DemuxSeekMode::DecodePointBefore,
        }
    }

    /// Создаёт быстрый preview seek-запрос.
    #[must_use]
    pub const fn preview(timestamp: Duration) -> Self {
        Self {
            timestamp,
            mode: DemuxSeekMode::Preview,
        }
    }
}

/// Seekability container/source связки, нормализованная для player timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemuxSeekability {
    /// Demuxer может выполнять seek на media timeline.
    Seekable,

    /// Demuxer открыт для playback, но seek сейчас недоступен.
    NotSeekable {
        /// Нейтральная причина для UI/player diagnostics.
        reason: TimelineNotSeekableReason,
    },
}

/// Нейтральные ошибки общего demux contract-а.
///
/// Concrete demuxer-ы могут продолжать возвращать свои backend-specific ошибки,
/// но default methods в `media-core` не должны зависеть от контейнерных crate'ов.
#[derive(Debug, thiserror::Error)]
pub enum MediaDemuxError {
    /// Источник или контейнер не поддерживает seek для текущего media.
    #[error("Seek недоступен: {reason}")]
    SeekUnavailable {
        /// Человекочитаемая причина от container/source adapter-а.
        reason: String,
    },

    /// Запрошенный режим нельзя честно выполнить через legacy `seek(timestamp)`.
    #[error("Seek mode {mode:?} не поддерживается этой реализацией demuxer-а")]
    UnsupportedSeekMode {
        /// Container-level режим, который demuxer не умеет честно выполнить.
        mode: DemuxSeekMode,
    },
}

impl MediaDemuxError {
    /// Возвращает `true`, если ошибка означает отсутствие seek capability.
    #[must_use]
    pub const fn is_seek_unavailable(&self) -> bool {
        matches!(
            self,
            Self::SeekUnavailable { .. } | Self::UnsupportedSeekMode { .. }
        )
    }
}

/// Обновлённый список tracks после container-level discontinuity.
///
/// Demuxer отдаёт это как lifecycle event, когда backend сообщил, что старые
/// decoder configs больше нельзя считать валидными.
#[derive(Debug, Clone, PartialEq)]
pub struct DemuxTrackListUpdate {
    /// Новый public track list, который consumer должен использовать для выбора decoder configs.
    pub tracks: Vec<TrackInfo>,

    /// Новая длительность media timeline, если container смог её определить.
    pub duration: Option<Duration>,
}

impl DemuxTrackListUpdate {
    /// Создаёт явный update без раскрытия внутреннего storage demuxer-а.
    #[must_use]
    pub fn new(tracks: Vec<TrackInfo>, duration: Option<Duration>) -> Self {
        Self { tracks, duration }
    }
}

/// Событие чтения из demuxer-а без привязки к конкретному container backend-у.
#[derive(Debug, Clone, PartialEq)]
pub enum DemuxReadEvent {
    /// Demuxer прочитал следующий encoded media packet.
    Packet(Packet),

    /// Demuxer дошёл до конца stream-а.
    EndOfStream,

    /// Container изменил track list, и decoder configs нужно построить заново.
    TracksChanged(DemuxTrackListUpdate),
}

/// Trait, абстрагирующий источник media packets.
///
/// Позволяет заменить контейнерную реализацию без изменения consumer code,
/// который работает только с нейтральными media-core типами.
pub trait Demuxer: Send {
    /// Информация о всех треках, доступная после открытия media source.
    fn tracks(&self) -> &[TrackInfo];

    /// Длительность контента, если она известна из контейнера или manifest-а.
    fn duration(&self) -> Option<Duration>;

    /// Возвращает seekability текущего demuxer/source stack-а.
    fn seekability(&self) -> DemuxSeekability {
        if self.duration().is_some() {
            DemuxSeekability::Seekable
        } else {
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline,
            }
        }
    }

    /// Возвращает следующий packet или `None`, если demuxer дошёл до EOF.
    fn next_packet(&mut self) -> anyhow::Result<Option<Packet>>;

    /// Возвращает следующий packet-level или lifecycle event из demuxer-а.
    ///
    /// Default сохраняет legacy contract для demuxer-ов, которые не имеют
    /// lifecycle событий: packet остаётся packet-ом, `None` становится EOF.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        match self.next_packet()? {
            Some(packet) => Ok(DemuxReadEvent::Packet(packet)),
            None => Ok(DemuxReadEvent::EndOfStream),
        }
    }

    /// Выполняет legacy accurate seek к позиции на media timeline.
    ///
    /// Реализация возвращает фактическую container-позицию, потому что точный
    /// playback commit выполняется выше: player делает pre-roll/drop до target.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult>;

    /// Выполняет seek с явным режимом скорости/точности.
    ///
    /// Default поддерживает только legacy accurate seek и явно отклоняет режимы,
    /// которые нельзя честно свести к `seek(timestamp)`.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        match request.mode {
            DemuxSeekMode::Accurate => self.seek(request.timestamp),
            unsupported_mode => Err(MediaDemuxError::UnsupportedSeekMode {
                mode: unsupported_mode,
            }
            .into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    struct AccurateOnlyDemuxer {
        seek_log: Vec<Duration>,
        duration: Option<Duration>,
    }

    impl AccurateOnlyDemuxer {
        const fn with_duration(duration: Option<Duration>) -> Self {
            Self {
                seek_log: Vec::new(),
                duration,
            }
        }
    }

    impl Demuxer for AccurateOnlyDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            self.duration
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<Packet>> {
            Ok(None)
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            self.seek_log.push(timestamp);
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    struct PacketThenEofDemuxer {
        packet: Option<Packet>,
    }

    impl PacketThenEofDemuxer {
        fn new(packet: Packet) -> Self {
            Self {
                packet: Some(packet),
            }
        }
    }

    impl Demuxer for PacketThenEofDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<Packet>> {
            Ok(self.packet.take())
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    fn test_packet() -> Packet {
        Packet::new(
            crate::TrackId::new(7),
            crate::TrackKind::Audio,
            Duration::from_millis(25),
            None,
            false,
            Bytes::from_static(b"packet"),
        )
    }

    #[test]
    fn default_next_event_maps_legacy_packet_and_eof() {
        let mut demuxer = PacketThenEofDemuxer::new(test_packet());

        let first_event = demuxer
            .next_event()
            .expect("legacy packet должен стать read event");
        assert!(matches!(first_event, DemuxReadEvent::Packet(_)));

        let second_event = demuxer
            .next_event()
            .expect("legacy EOF должен стать read event");
        assert_eq!(second_event, DemuxReadEvent::EndOfStream);
    }

    #[test]
    fn default_seekability_is_seekable_when_duration_is_known() {
        let demuxer = AccurateOnlyDemuxer::with_duration(Some(Duration::from_secs(10)));

        assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
    }

    #[test]
    fn default_seekability_reports_unknown_timeline_without_duration() {
        let demuxer = AccurateOnlyDemuxer::with_duration(None);

        assert_eq!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline
            }
        );
    }

    #[test]
    fn default_seek_with_request_allows_accurate_legacy_seek() {
        let mut demuxer = AccurateOnlyDemuxer::with_duration(Some(Duration::from_secs(10)));

        let result = demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(3)))
            .expect("accurate fallback должен использовать legacy seek");

        assert_eq!(result.requested_position, MediaTime::from_secs(3));
        assert_eq!(demuxer.seek_log, vec![Duration::from_secs(3)]);
    }

    #[test]
    fn default_seek_with_request_rejects_preview_without_touching_seek_state() {
        let mut demuxer = AccurateOnlyDemuxer::with_duration(Some(Duration::from_secs(10)));
        let target_position = Duration::from_secs(3);

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::preview(target_position))
            .expect_err("default demuxer не должен молча игнорировать preview mode");
        let demux_error = error
            .downcast_ref::<MediaDemuxError>()
            .expect("ошибка должна оставаться typed MediaDemuxError");

        assert!(matches!(
            demux_error,
            MediaDemuxError::UnsupportedSeekMode {
                mode: DemuxSeekMode::Preview
            }
        ));
        assert!(demuxer.seek_log.is_empty());
    }

    #[test]
    fn default_seek_with_request_rejects_decode_point_before_without_touching_seek_state() {
        let mut demuxer = AccurateOnlyDemuxer::with_duration(Some(Duration::from_secs(10)));
        let target_position = Duration::from_secs(3);

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target_position))
            .expect_err("default demuxer не должен молча игнорировать decode-safe mode");
        let demux_error = error
            .downcast_ref::<MediaDemuxError>()
            .expect("ошибка должна оставаться typed MediaDemuxError");

        assert!(matches!(
            demux_error,
            MediaDemuxError::UnsupportedSeekMode {
                mode: DemuxSeekMode::DecodePointBefore
            }
        ));
        assert!(demuxer.seek_log.is_empty());
    }

    #[test]
    fn demux_seek_result_can_keep_signed_actual_track_timestamp() {
        let time_base = crate::TimeBase::new(1, 1_000).expect("valid time base");
        let actual_track_timestamp = TrackTimestamp::new(crate::TrackId::new(11), -40, time_base);

        let result = DemuxSeekResult {
            requested_position: MediaTime::from_millis(10),
            actual_position: actual_track_timestamp.to_media_time(),
            actual_track_timestamp: Some(actual_track_timestamp),
        };

        let stored_timestamp = result
            .actual_track_timestamp
            .expect("seek result должен хранить raw track timestamp");

        assert_eq!(stored_timestamp.units.get(), -40);
        assert_eq!(result.actual_position, MediaTime::ZERO);
    }
}
