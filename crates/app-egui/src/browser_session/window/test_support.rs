//! Общие заготовки тестов окна браузерного режима.

use std::time::Duration;

use media_core::{DemuxSeekResult, Demuxer};
use player_core::PreparedMedia;

/// Демуксер без дорожек: медиа для тестов, где плеер подделан и байты не читаются.
struct EmptyDemuxer;

impl Demuxer for EmptyDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Err(anyhow::anyhow!("the empty test demuxer does not seek"))
    }
}

/// Медиа «поколения потока» для тестов с подделанным плеером.
pub(super) fn stub_prepared_media() -> PreparedMedia {
    PreparedMedia::from_external_label("browser stream", Box::new(EmptyDemuxer))
}
