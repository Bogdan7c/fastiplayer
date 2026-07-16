use media_core::{MediaDuration, MediaTime};

use crate::{DesktopLoopStatus, DesktopTrackKey, EffectiveVolume, TimelineSeekRequestId};

/// MPRIS-compatible playback status без зависимости от zbus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl DesktopPlaybackStatus {
    #[must_use]
    pub const fn as_mpris_str(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }
}

/// Монотонная committed revision process-lifetime transport snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DesktopSnapshotRevision(u64);

impl DesktopSnapshotRevision {
    pub const INITIAL: Self = Self(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Динамические capability свойства MPRIS Player.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopCapabilities {
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
}

/// Metadata активного media; track key кодируется в object path только Linux adapter-ом.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopMetadata {
    pub track_key: Option<DesktopTrackKey>,
    pub title: Option<String>,
    pub source_label: Option<String>,
    pub duration: Option<MediaDuration>,
}

/// Matching Applied seek, который backend должен сигналить ровно один раз.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopSeeked {
    pub request_id: TimelineSeekRequestId,
    pub position: MediaTime,
}

/// Полный process-lifetime transport snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopSnapshotView {
    pub revision: DesktopSnapshotRevision,
    pub playback_status: DesktopPlaybackStatus,
    pub metadata: DesktopMetadata,
    pub position: MediaTime,
    pub capabilities: DesktopCapabilities,
    pub loop_status: DesktopLoopStatus,
    pub shuffle: bool,
    pub volume: EffectiveVolume,
    pub seeked: Option<DesktopSeeked>,
}

impl DesktopSnapshotView {
    #[must_use]
    pub fn neutral(volume: EffectiveVolume) -> Self {
        Self {
            revision: DesktopSnapshotRevision::INITIAL,
            playback_status: DesktopPlaybackStatus::Stopped,
            metadata: DesktopMetadata::default(),
            position: MediaTime::ZERO,
            capabilities: DesktopCapabilities::default(),
            loop_status: DesktopLoopStatus::None,
            shuffle: false,
            volume,
            seeked: None,
        }
    }

    #[must_use]
    pub const fn has_media(&self) -> bool {
        self.metadata.track_key.is_some()
    }
}

/// Diff хранит только protocol-level факты; payload всегда читается из latest revision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopSnapshotChange {
    pub dynamic_properties_changed: bool,
    pub seeked: Option<DesktopSeeked>,
}

impl DesktopSnapshotChange {
    #[must_use]
    pub fn from_views(previous: &DesktopSnapshotView, current: &DesktopSnapshotView) -> Self {
        let dynamic_properties_changed = previous.playback_status != current.playback_status
            || previous.metadata != current.metadata
            || previous.capabilities != current.capabilities
            || previous.loop_status != current.loop_status
            || previous.shuffle != current.shuffle
            || previous.volume != current.volume;
        let seeked = (previous.seeked != current.seeked)
            .then_some(current.seeked)
            .flatten();
        Self {
            dynamic_properties_changed,
            seeked,
        }
    }

    #[must_use]
    pub const fn has_notifications(self) -> bool {
        self.dynamic_properties_changed || self.seeked.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume() -> EffectiveVolume {
        EffectiveVolume::from_player(1.0).expect("test volume")
    }

    #[test]
    fn position_and_fixed_contract_do_not_create_properties_changed() {
        let previous = DesktopSnapshotView::neutral(volume());
        let mut current = previous.clone();
        current.position = MediaTime::from_secs(42);
        current.revision = DesktopSnapshotRevision::new(1);

        assert_eq!(
            DesktopSnapshotChange::from_views(&previous, &current),
            DesktopSnapshotChange::default()
        );
    }

    #[test]
    fn every_dynamic_capability_change_requests_full_property_publication() {
        let previous = DesktopSnapshotView::neutral(volume());
        let mut current = previous.clone();
        current.capabilities.can_play = true;

        assert!(DesktopSnapshotChange::from_views(&previous, &current).dynamic_properties_changed);
    }
}
