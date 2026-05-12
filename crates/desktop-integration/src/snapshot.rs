use media_core::{MediaDuration, MediaTime};
use player_core::{PlaybackState, PlayerSnapshot};

use crate::MPRIS_CURRENT_TRACK_ID;

/// MPRIS-compatible playback status без зависимости от zbus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPlaybackStatus {
    /// MPRIS `Playing`.
    Playing,

    /// MPRIS `Paused`.
    Paused,

    /// MPRIS `Stopped`.
    Stopped,
}

impl DesktopPlaybackStatus {
    /// Возвращает строку, которую ожидает MPRIS property `PlaybackStatus`.
    #[must_use]
    pub const fn as_mpris_str(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }
}

/// Metadata, которую platform backend может сериализовать в свой transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopMetadata {
    /// Stable track id текущего media для MPRIS SetPosition.
    pub track_id: Option<String>,

    /// Заголовок media для desktop widget.
    pub title: Option<String>,

    /// Человекочитаемый источник, если отдельного title нет.
    pub source_label: Option<String>,

    /// Длительность media; в MPRIS публикуется как `mpris:length`.
    pub duration: Option<MediaDuration>,
}

/// Compact view snapshot-а, который нужен desktop backend-ам.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSnapshotView {
    /// Текущее состояние playback в MPRIS-compatible форме.
    pub playback_status: DesktopPlaybackStatus,

    /// Metadata текущего media.
    pub metadata: DesktopMetadata,

    /// Текущая media-позиция.
    pub position: MediaTime,

    /// Можно ли принимать desktop seek commands.
    pub can_seek: bool,
}

impl DesktopSnapshotView {
    /// Строит desktop view только из public `PlayerSnapshot`.
    #[must_use]
    pub fn from_player_snapshot(snapshot: &PlayerSnapshot) -> Self {
        let duration = snapshot
            .timeline
            .duration
            .or_else(|| snapshot.duration.map(MediaDuration::from_duration));
        let source_label = snapshot.source_label.clone();
        let title = snapshot
            .media_title
            .clone()
            .or_else(|| source_label.clone());
        let has_media = source_label.is_some();

        Self {
            playback_status: map_playback_status(snapshot.playback_state),
            metadata: DesktopMetadata {
                track_id: has_media.then(|| MPRIS_CURRENT_TRACK_ID.to_string()),
                title,
                source_label,
                duration,
            },
            position: snapshot.timeline.current_position,
            can_seek: has_media && snapshot.timeline.seekable,
        }
    }

    /// Возвращает `true`, если metadata представляет открытый media.
    #[must_use]
    pub fn has_media(&self) -> bool {
        self.metadata.track_id.is_some()
    }
}

/// Набор MPRIS-signalled properties, изменившихся после нового snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopSnapshotChange {
    /// Изменился `PlaybackStatus`.
    pub playback_status_changed: bool,

    /// Изменилась `Metadata`, включая duration.
    pub metadata_changed: bool,

    /// Изменился `CanSeek`.
    pub can_seek_changed: bool,
}

impl DesktopSnapshotChange {
    /// Сравнивает два desktop view без учёта high-rate position.
    #[must_use]
    pub fn from_views(previous: &DesktopSnapshotView, current: &DesktopSnapshotView) -> Self {
        Self {
            playback_status_changed: previous.playback_status != current.playback_status,
            metadata_changed: previous.metadata != current.metadata,
            can_seek_changed: previous.can_seek != current.can_seek,
        }
    }

    /// Проверяет, есть ли properties для D-Bus notification.
    #[must_use]
    pub const fn has_property_changes(self) -> bool {
        self.playback_status_changed || self.metadata_changed || self.can_seek_changed
    }
}

/// Маппит rich playback state в три состояния MPRIS.
#[must_use]
fn map_playback_status(playback_state: PlaybackState) -> DesktopPlaybackStatus {
    match playback_state {
        PlaybackState::Playing
        | PlaybackState::Buffering
        | PlaybackState::Seeking
        | PlaybackState::Draining => DesktopPlaybackStatus::Playing,
        PlaybackState::Paused | PlaybackState::Ended => DesktopPlaybackStatus::Paused,
        PlaybackState::Idle
        | PlaybackState::Opening
        | PlaybackState::Stopped
        | PlaybackState::Failed => DesktopPlaybackStatus::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use media_core::{MediaDuration, MediaTime, TimelineSnapshot};
    use player_core::{PlaybackState, PlayerSnapshot};

    use super::*;

    #[test]
    fn maps_snapshot_metadata_duration_and_can_seek() {
        let mut snapshot = PlayerSnapshot::empty();
        snapshot.source_label = Some("sample.webm".to_string());
        snapshot.media_title = Some("Sample".to_string());
        snapshot.timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(90));

        let view = DesktopSnapshotView::from_player_snapshot(&snapshot);

        assert_eq!(
            view.metadata.track_id.as_deref(),
            Some(MPRIS_CURRENT_TRACK_ID)
        );
        assert_eq!(view.metadata.title.as_deref(), Some("Sample"));
        assert_eq!(view.metadata.duration, Some(MediaDuration::from_secs(90)));
        assert!(view.can_seek);
    }

    #[test]
    fn falls_back_to_source_label_as_title() {
        let mut snapshot = PlayerSnapshot::empty();
        snapshot.source_label = Some("fallback-title.webm".to_string());

        let view = DesktopSnapshotView::from_player_snapshot(&snapshot);

        assert_eq!(view.metadata.title.as_deref(), Some("fallback-title.webm"));
    }

    #[test]
    fn maps_playback_status_to_mpris_values() {
        let mut snapshot = PlayerSnapshot::empty();

        snapshot.playback_state = PlaybackState::Playing;
        assert_eq!(
            DesktopSnapshotView::from_player_snapshot(&snapshot).playback_status,
            DesktopPlaybackStatus::Playing
        );

        snapshot.playback_state = PlaybackState::Paused;
        assert_eq!(
            DesktopSnapshotView::from_player_snapshot(&snapshot).playback_status,
            DesktopPlaybackStatus::Paused
        );

        snapshot.playback_state = PlaybackState::Stopped;
        assert_eq!(
            DesktopSnapshotView::from_player_snapshot(&snapshot).playback_status,
            DesktopPlaybackStatus::Stopped
        );
    }

    #[test]
    fn snapshot_change_ignores_position_only_updates() {
        let mut previous_snapshot = PlayerSnapshot::empty();
        previous_snapshot.source_label = Some("sample.webm".to_string());
        previous_snapshot.timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(90));

        let mut current_snapshot = previous_snapshot.clone();
        current_snapshot.timeline.current_position = MediaTime::from_secs(10);

        let previous_view = DesktopSnapshotView::from_player_snapshot(&previous_snapshot);
        let current_view = DesktopSnapshotView::from_player_snapshot(&current_snapshot);
        let change = DesktopSnapshotChange::from_views(&previous_view, &current_view);

        assert!(!change.has_property_changes());
    }

    #[test]
    fn snapshot_change_tracks_metadata_duration_can_seek_and_status() {
        let mut previous_snapshot = PlayerSnapshot::empty();
        previous_snapshot.source_label = Some("sample.webm".to_string());
        previous_snapshot.playback_state = PlaybackState::Paused;

        let mut current_snapshot = previous_snapshot.clone();
        current_snapshot.playback_state = PlaybackState::Playing;
        current_snapshot.timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(90));

        let previous_view = DesktopSnapshotView::from_player_snapshot(&previous_snapshot);
        let current_view = DesktopSnapshotView::from_player_snapshot(&current_snapshot);
        let change = DesktopSnapshotChange::from_views(&previous_view, &current_view);

        assert!(change.playback_status_changed);
        assert!(change.metadata_changed);
        assert!(change.can_seek_changed);
        assert!(change.has_property_changes());
    }
}
