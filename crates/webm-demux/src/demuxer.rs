use std::time::Duration;

use media_core::{MediaTime, Packet, TimelineNotSeekableReason, TrackInfo, TrackTimestamp};

use crate::error::DemuxError;

/// Результат container-level seek без привязки к конкретной реализации demuxer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxSeekResult {
    /// Позиция, которую запросил player на нормализованной media timeline.
    pub requested_position: MediaTime,

    /// Позиция, на которую container фактически переставил чтение.
    pub actual_position: MediaTime,

    /// Сырой timestamp track-а, который Symphonia вернула после seek.
    pub actual_track_timestamp: Option<TrackTimestamp>,
}

/// Режим container-level seek-а без привязки к конкретному backend-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemuxSeekMode {
    /// Финальный seek: демультиплексор должен выбрать максимально точную позицию до target.
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

/// Trait, абстрагирующий источник media packets.
///
/// Позволяет заменить реализацию (symphonia → matroska → streaming)
/// без изменения consumer code (audio/video pipeline).
pub trait Demuxer: Send {
    /// Информация о всех треках (доступна после open()).
    fn tracks(&self) -> &[TrackInfo];

    /// Длительность контента, если известна из контейнера.
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

    /// Следующий packet — None при EOF.
    fn next_packet(&mut self) -> anyhow::Result<Option<Packet>>;

    /// Seek к позиции на media timeline.
    ///
    /// Реализация должна вернуть фактическую container-позицию, потому что точный
    /// playback commit выполняется выше: player делает pre-roll/drop до target.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult>;

    /// Seek с явным режимом скорости/точности.
    ///
    /// Default поддерживает только legacy accurate seek и явно отклоняет режимы,
    /// которые нельзя честно свести к `seek(timestamp)`.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        match request.mode {
            DemuxSeekMode::Accurate => self.seek(request.timestamp),
            unsupported_mode => Err(DemuxError::UnsupportedSeekMode {
                mode: unsupported_mode,
            }
            .into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AccurateOnlyDemuxer {
        seek_log: Vec<Duration>,
    }

    impl AccurateOnlyDemuxer {
        const fn new() -> Self {
            Self {
                seek_log: Vec::new(),
            }
        }
    }

    impl Demuxer for AccurateOnlyDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(10))
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

    #[test]
    fn default_seek_with_request_allows_accurate_legacy_seek() {
        let mut demuxer = AccurateOnlyDemuxer::new();

        let result = demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(3)))
            .expect("accurate fallback должен использовать legacy seek");

        assert_eq!(result.requested_position, MediaTime::from_secs(3));
        assert_eq!(demuxer.seek_log, vec![Duration::from_secs(3)]);
    }

    #[test]
    fn default_seek_with_request_rejects_non_accurate_modes() {
        let mut demuxer = AccurateOnlyDemuxer::new();

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(3)))
            .expect_err("default demuxer не должен молча игнорировать mode");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("ошибка должна оставаться typed DemuxError");

        assert!(matches!(
            demux_error,
            DemuxError::UnsupportedSeekMode {
                mode: DemuxSeekMode::Preview
            }
        ));
        assert!(demuxer.seek_log.is_empty());
    }
}
