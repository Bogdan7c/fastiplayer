//! Typed admission обычного seek-а и recovery протухшей live-позиции.

use media_core::{MediaTime, TimelineNotSeekableReason};

use crate::seek_state::SeekTargetRetention;
use crate::{PlayerError, PlayerErrorKind};

use super::PlayerSession;

/// Причина, по которой owner просит войти в общий seek lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeekTimelineAdmission {
    /// Пользовательский seek требует packet-proven public range.
    PublicSeekableRange,
    /// Play відновлює позицію, яку authoritative availability уже витіснила.
    ExpiredLiveAvailability,
}

impl SeekTimelineAdmission {
    /// Не теряет semantic intent после initial admission и worker enqueue.
    pub(super) const fn target_retention(self) -> SeekTargetRetention {
        match self {
            Self::PublicSeekableRange => SeekTargetRetention::ExactPublicRange,
            Self::ExpiredLiveAvailability => SeekTargetRetention::LiveAvailability,
        }
    }
}

impl PlayerSession {
    /// Возвращает typed rejection, не смешивая две разные гарантии timeline.
    pub(super) fn seek_timeline_admission_error(
        &self,
        target_position: MediaTime,
        admission: SeekTimelineAdmission,
    ) -> Option<PlayerError> {
        match admission {
            SeekTimelineAdmission::PublicSeekableRange => {
                if self.snapshot.timeline.seekable {
                    return None;
                }
                let reason = self
                    .snapshot
                    .timeline
                    .not_seekable_reason
                    .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
                Some(PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    format!("Seek невозможен: timeline не seekable ({reason:?})"),
                ))
            }
            SeekTimelineAdmission::ExpiredLiveAvailability => {
                if self.expired_live_resume_target() == Some(target_position) {
                    return None;
                }
                Some(PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    "Expired live recovery target no longer matches authoritative availability",
                ))
            }
        }
    }
}
