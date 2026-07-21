//! Replay wrapper для событий, прочитанных во время staged media preflight.

use std::collections::VecDeque;
use std::time::Duration;

use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaMetadata,
    TrackInfo,
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
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::EndOfStream
            | DemuxReadEvent::TemporarilyUnavailable(_) => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    use media_core::{DemuxRetryHint, MediaTime};

    /// Finite inner demuxer доказывает переход replay wrapper-а к underlying source.
    struct FiniteInnerDemuxer {
        /// Scripted события после исчерпания replay queue.
        events: VecDeque<DemuxReadEvent>,
    }

    impl Demuxer for FiniteInnerDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(99))
        }

        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            Ok(self
                .events
                .pop_front()
                .unwrap_or(DemuxReadEvent::EndOfStream))
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    #[test]
    fn temporary_replay_event_preserves_visible_snapshots_and_identity() {
        let retry_hint = DemuxRetryHint::new(Duration::from_millis(25))
            .expect("focused retry delay должен быть допустим");
        let temporary_event = DemuxReadEvent::TemporarilyUnavailable(retry_hint);
        let inner = FiniteInnerDemuxer {
            events: VecDeque::from([DemuxReadEvent::EndOfStream]),
        };
        let mut demuxer = PrefetchedDemuxer::new(
            Box::new(inner),
            Vec::new(),
            Some(Duration::from_secs(10)),
            DemuxSeekability::Seekable,
            None,
            VecDeque::from([temporary_event.clone()]),
        );

        let replayed_event = demuxer
            .next_event()
            .expect("temporary replay event не должен становиться error");

        assert_eq!(replayed_event, temporary_event);
        assert_eq!(demuxer.duration(), Some(Duration::from_secs(10)));
        assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
        assert!(demuxer.tracks().is_empty());
        assert_eq!(
            demuxer
                .next_event()
                .expect("после replay wrapper должен продолжить inner source"),
            DemuxReadEvent::EndOfStream
        );
    }
}
