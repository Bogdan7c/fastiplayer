//! Replay wrapper для событий, прочитанных во время staged media preflight.

use std::collections::VecDeque;
use std::time::Duration;

use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaMetadata,
    Packet, TrackInfo,
};

/// Сохраняет наблюдаемый demux order после чтения candidate header-а до commit-а.
pub(super) struct PrefetchedDemuxer {
    /// Реальный container demuxer, уже продвинутый staged preflight-ом.
    inner: Box<dyn Demuxer + Send>,

    /// События между исходной позицией и текущей позицией `inner`.
    replay_events: VecDeque<DemuxReadEvent>,

    /// Track snapshot, видимый consumer-у на текущей replay позиции.
    visible_tracks: Vec<TrackInfo>,

    /// Duration snapshot, видимый consumer-у на текущей replay позиции.
    visible_duration: Option<Duration>,

    /// Seekability snapshot до завершения replay.
    visible_seekability: DemuxSeekability,

    /// Media metadata, видимая consumer-у на текущей replay позиции.
    visible_media_metadata: Option<MediaMetadata>,
}

impl PrefetchedDemuxer {
    /// Создаёт wrapper с исходными snapshots и уже прочитанными событиями.
    pub(super) fn new(
        inner: Box<dyn Demuxer + Send>,
        visible_tracks: Vec<TrackInfo>,
        visible_duration: Option<Duration>,
        visible_seekability: DemuxSeekability,
        visible_media_metadata: Option<MediaMetadata>,
        replay_events: VecDeque<DemuxReadEvent>,
    ) -> Self {
        Self {
            inner,
            replay_events,
            visible_tracks,
            visible_duration,
            visible_seekability,
            visible_media_metadata,
        }
    }

    /// Применяет lifecycle event только в момент, когда его увидел consumer.
    fn apply_visible_lifecycle_event(&mut self, event: &DemuxReadEvent) {
        match event {
            DemuxReadEvent::TracksChanged(track_update) => {
                self.visible_tracks = track_update.tracks.clone();
                self.visible_duration = track_update.duration;
            }
            DemuxReadEvent::MediaMetadataChanged(metadata) => {
                self.visible_media_metadata = Some(metadata.clone());
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::EndOfStream => {}
        }
    }

    /// После seek replay больше не соответствует позиции и должен быть отброшен.
    fn refresh_visible_snapshots_after_seek(&mut self) {
        self.replay_events.clear();
        // Initial track order может содержать user codec preference. Обычный seek
        // не является track-list change, поэтому сохраняем порядок до явного event-а.
        self.visible_duration = self.inner.duration();
        self.visible_seekability = self.inner.seekability();
        self.visible_media_metadata = self.inner.media_metadata();
    }
}

impl Demuxer for PrefetchedDemuxer {
    /// Возвращает track snapshot с учётом уже воспроизведённых lifecycle events.
    fn tracks(&self) -> &[TrackInfo] {
        &self.visible_tracks
    }

    /// Возвращает duration с той же логической позиции replay stream-а.
    fn duration(&self) -> Option<Duration> {
        self.visible_duration
    }

    /// Возвращает metadata с той же логической позиции replay stream-а.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.visible_media_metadata.clone()
    }

    /// Не раскрывает преждевременно seekability продвинутого `inner` demuxer-а.
    fn seekability(&self) -> DemuxSeekability {
        self.visible_seekability
    }

    /// Сохраняет legacy packet-only contract, пропуская lifecycle events после их применения.
    fn next_packet(&mut self) -> anyhow::Result<Option<Packet>> {
        loop {
            match self.next_event()? {
                DemuxReadEvent::Packet(packet) => return Ok(Some(packet)),
                DemuxReadEvent::EndOfStream => return Ok(None),
                DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            }
        }
    }

    /// Сначала возвращает prefetched events, затем продолжает реальный demuxer.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let event = match self.replay_events.pop_front() {
            Some(event) => event,
            None => self.inner.next_event()?,
        };
        self.apply_visible_lifecycle_event(&event);
        Ok(event)
    }

    /// Seek инвалидирует replay queue и возвращает snapshots реальной новой позиции.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        let seek_result = self.inner.seek(timestamp)?;
        self.refresh_visible_snapshots_after_seek();
        Ok(seek_result)
    }

    /// Сохраняет typed seek mode вместо сведения всех запросов к legacy accurate seek.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        let seek_result = self.inner.seek_with_request(request)?;
        self.refresh_visible_snapshots_after_seek();
        Ok(seek_result)
    }
}
