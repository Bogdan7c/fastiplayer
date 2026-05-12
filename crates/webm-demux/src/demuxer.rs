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
}
