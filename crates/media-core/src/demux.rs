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
}
