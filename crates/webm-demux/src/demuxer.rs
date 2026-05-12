use std::time::Duration;

use media_core::{MediaTime, Packet, TimelineNotSeekableReason, TrackInfo, TrackTimestamp};

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

    /// Safe source byte offset, если runtime index знает container boundary до target.
    pub byte_offset_hint: Option<u64>,
}

impl DemuxSeekRequest {
    /// Создаёт финальный точный seek-запрос.
    #[must_use]
    pub const fn accurate(timestamp: Duration) -> Self {
        Self {
            timestamp,
            mode: DemuxSeekMode::Accurate,
            byte_offset_hint: None,
        }
    }

    /// Создаёт быстрый preview seek-запрос.
    #[must_use]
    pub const fn preview(timestamp: Duration) -> Self {
        Self {
            timestamp,
            mode: DemuxSeekMode::Preview,
            byte_offset_hint: None,
        }
    }

    /// Возвращает request с безопасным source byte offset hint.
    #[must_use]
    pub const fn with_byte_offset_hint(mut self, byte_offset: u64) -> Self {
        self.byte_offset_hint = Some(byte_offset);
        self
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
    /// Default сохраняет совместимость для простых demuxer-ов и тестовых doubles.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.seek(request.timestamp)
    }
}
