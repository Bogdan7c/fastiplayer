//! Чистые преобразования типов протокола в типы источника и плеера (тот же номер, то же
//! время, без состояния).
//!
//! Вынесено из `translator.rs` (предел 800 строк) без изменения поведения.

use browser_bridge_protocol::session::page::{PagePlaybackRequest, RemovalEnd};
use browser_bridge_protocol::{MediaDuration, MediaTime, SourceBufferIndex};
use pushed_media_source::{LaneId, PageRemovedRange, PageRemovedRangeEnd, PushedDuration};

use super::PageIntent;

/// SourceBuffer протокола → полоса источника (тот же номер).
pub(super) const fn lane_of(source_buffer: SourceBufferIndex) -> LaneId {
    LaneId::new(source_buffer.get())
}

/// Время протокола (целые мкс) → время источника.
pub(super) const fn media_time_as_duration(time: MediaTime) -> std::time::Duration {
    std::time::Duration::from_micros(time.as_micros())
}

/// Стёртый диапазон `remove(start, end)` протокола → диапазон источника. Время
/// протокола — уже время показа после `timestampOffset`, как у полки источника.
pub(super) const fn page_removed_range(start: MediaTime, end: RemovalEnd) -> PageRemovedRange {
    let end = match end {
        RemovalEnd::At { time_us } => PageRemovedRangeEnd::At(media_time_as_duration(time_us)),
        RemovalEnd::EndOfTimeline {} => PageRemovedRangeEnd::EndOfTimeline,
    };
    PageRemovedRange::new(media_time_as_duration(start), end)
}

/// Длительность протокола → длительность источника.
pub(super) const fn pushed_duration(duration: MediaDuration) -> PushedDuration {
    match duration {
        MediaDuration::Unknown {} => PushedDuration::Unknown,
        MediaDuration::Live {} => PushedDuration::Live,
        MediaDuration::Finite { duration_us } => {
            PushedDuration::Finite(std::time::Duration::from_micros(duration_us.as_micros()))
        }
    }
}

/// `page_playback` → намерение для плеера.
pub(super) const fn page_intent(request: PagePlaybackRequest) -> PageIntent {
    match request {
        PagePlaybackRequest::Play {} => PageIntent::Play,
        PagePlaybackRequest::Pause {} => PageIntent::Pause,
        PagePlaybackRequest::SetRate { rate } => PageIntent::SetRate(rate),
    }
}
