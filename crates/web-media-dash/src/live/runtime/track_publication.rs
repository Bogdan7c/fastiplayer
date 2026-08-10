//! Публикация track-контракта live-сессии без ложных decoder reset-ов.

use media_core::{DemuxTrackListUpdate, TrackInfo};

/// Последний track-контракт, который увидел владелец player pipeline.
///
/// Каждый immutable DASH snapshot открывает новые component demuxer-ы. Они
/// закономерно повторяют начальный список треков, но такое повторение не
/// означает смену кодека и не должно пересоздавать decoder/audio output.
#[derive(Debug)]
pub(super) struct DashLiveTrackPublication {
    published: Vec<TrackInfo>,
}

impl DashLiveTrackPublication {
    /// Принимает уже нормализованный initial contract live-сессии.
    pub(super) fn new(initial_tracks: Vec<TrackInfo>) -> Self {
        Self {
            published: initial_tracks,
        }
    }

    /// Возвращает ровно тот контракт, который считается публичным.
    pub(super) fn tracks(&self) -> &[TrackInfo] {
        &self.published
    }

    /// Публикует только реальную смену track-контракта.
    ///
    /// Сравнение намеренно точное: codec private, time base, audio/video
    /// metadata и track id являются частью decoder boundary. Snapshot-local
    /// durations удаляются раньше на session-timeline boundary.
    pub(super) fn publish_if_changed(
        &mut self,
        update: DemuxTrackListUpdate,
    ) -> Option<DemuxTrackListUpdate> {
        if update.tracks == self.published {
            return None;
        }
        self.published = update.tracks.clone();
        Some(update)
    }
}

#[cfg(test)]
mod tests {
    use media_core::{DemuxTrackListUpdate, TrackId, TrackInfo, TrackKind};

    use super::DashLiveTrackPublication;

    /// Test track содержит все decoder-relevant поля в явном виде.
    fn audio_track(codec_id: &str) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(1),
            kind: TrackKind::Audio,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: None,
            duration: None,
            sample_rate: Some(48_000),
            channels: Some(2),
            video: None,
        }
    }

    /// Идентичный snapshot не выдаёт ложный TracksChanged наружу.
    #[test]
    fn identical_track_contract_is_not_republished() {
        let track = audio_track("A_AAC");
        let mut publication = DashLiveTrackPublication::new(vec![track.clone()]);

        let duplicate =
            publication.publish_if_changed(DemuxTrackListUpdate::new(vec![track], None));

        assert!(duplicate.is_none());
        assert_eq!(publication.tracks()[0].codec_id, "A_AAC");
    }

    /// Настоящая смена codec contract публикуется и становится authoritative.
    #[test]
    fn changed_codec_contract_is_published() {
        let mut publication = DashLiveTrackPublication::new(vec![audio_track("A_AAC")]);

        let changed = publication
            .publish_if_changed(DemuxTrackListUpdate::new(vec![audio_track("A_OPUS")], None))
            .expect("codec change must be published");

        assert_eq!(changed.tracks[0].codec_id, "A_OPUS");
        assert_eq!(publication.tracks()[0].codec_id, "A_OPUS");
    }
}
