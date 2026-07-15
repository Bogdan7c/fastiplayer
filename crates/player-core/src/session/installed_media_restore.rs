//! Player-owner применение exact-instance position/track restore intent-а.

use crate::media_install::InstalledMediaTargetMatch;
use crate::{
    InstalledMediaRelease, InstalledMediaReleaseOutcome, InstalledMediaRestoreFailureStage,
    InstalledMediaStateRestore, InstalledMediaStateRestoreOutcome, InstalledPositionRestore,
    InstalledPositionUnavailableReason, InstalledSubtitleRestore, InstalledTrackRestore,
    PlayerCommand, SeekRequest,
};

use super::PlayerSession;

impl PlayerSession {
    /// Освобождает current media только при exact request+instance совпадении.
    pub(crate) fn release_installed_media(
        &mut self,
        release: InstalledMediaRelease,
    ) -> InstalledMediaReleaseOutcome {
        match self
            .playback_intent_control
            .match_installed_target(release.request_id, release.media_instance_id)
        {
            InstalledMediaTargetMatch::Matching => {}
            InstalledMediaTargetMatch::NotInstalledYet
            | InstalledMediaTargetMatch::UnknownOrSupersededRequest => {
                return InstalledMediaReleaseOutcome::Absent;
            }
            InstalledMediaTargetMatch::StaleInstance => {
                return InstalledMediaReleaseOutcome::StaleInstance;
            }
        }

        if self.snapshot.media_instance_id != Some(release.media_instance_id) {
            return InstalledMediaReleaseOutcome::StaleInstance;
        }

        match self.stop() {
            Ok(_) => InstalledMediaReleaseOutcome::Applied {
                media_instance_id: release.media_instance_id,
            },
            Err(error) => InstalledMediaReleaseOutcome::Failed { error },
        }
    }

    /// Применяет restore одним owner turn-ом только к correlated current instance.
    pub(crate) fn restore_installed_media_state(
        &mut self,
        restore: InstalledMediaStateRestore,
    ) -> InstalledMediaStateRestoreOutcome {
        match self
            .playback_intent_control
            .match_installed_target(restore.request_id, restore.media_instance_id)
        {
            InstalledMediaTargetMatch::Matching => {}
            InstalledMediaTargetMatch::NotInstalledYet => {
                return InstalledMediaStateRestoreOutcome::NotInstalledYet;
            }
            InstalledMediaTargetMatch::UnknownOrSupersededRequest => {
                return InstalledMediaStateRestoreOutcome::UnknownOrSupersededRequest;
            }
            InstalledMediaTargetMatch::StaleInstance => {
                return InstalledMediaStateRestoreOutcome::StaleInstance;
            }
        }

        if self.snapshot.media_instance_id != Some(restore.media_instance_id) {
            return InstalledMediaStateRestoreOutcome::StaleInstance;
        }

        if let Err(outcome) = self.restore_video_track(restore.video_track) {
            return outcome;
        }
        if let Err(outcome) = self.restore_audio_track(restore.audio_track) {
            return outcome;
        }
        if let Err(outcome) = self.restore_subtitle_track(restore.subtitle_track) {
            return outcome;
        }
        if let Err(outcome) =
            self.restore_media_position(restore.media_instance_id, restore.position)
        {
            return outcome;
        }

        InstalledMediaStateRestoreOutcome::Applied {
            media_instance_id: restore.media_instance_id,
        }
    }

    fn restore_video_track(
        &mut self,
        restore: InstalledTrackRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let InstalledTrackRestore::Select(track_id) = restore else {
            return Ok(());
        };
        self.dispatch_command(PlayerCommand::SelectVideoTrack(track_id))
            .map(|_| ())
            .map_err(|error| InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::VideoTrack,
                error,
            })
    }

    fn restore_audio_track(
        &mut self,
        restore: InstalledTrackRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let InstalledTrackRestore::Select(track_id) = restore else {
            return Ok(());
        };
        self.dispatch_command(PlayerCommand::SelectAudioTrack(track_id))
            .map(|_| ())
            .map_err(|error| InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::AudioTrack,
                error,
            })
    }

    fn restore_subtitle_track(
        &mut self,
        restore: InstalledSubtitleRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let track_id = match restore {
            InstalledSubtitleRestore::KeepDefault => return Ok(()),
            InstalledSubtitleRestore::Disabled => None,
            InstalledSubtitleRestore::Select(track_id) => Some(track_id),
        };
        self.dispatch_command(PlayerCommand::SelectSubtitleTrack(track_id))
            .map(|_| ())
            .map_err(|error| InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::SubtitleTrack,
                error,
            })
    }

    fn restore_media_position(
        &mut self,
        media_instance_id: crate::MediaInstanceId,
        restore: InstalledPositionRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let InstalledPositionRestore::SeekTo(position) = restore else {
            return Ok(());
        };
        if !self.snapshot.timeline.seekable
            && self.snapshot.timeline.not_seekable_reason
                == Some(media_core::TimelineNotSeekableReason::SourceNotSeekable)
        {
            return Err(InstalledMediaStateRestoreOutcome::PositionUnavailable {
                media_instance_id,
                requested_position: position,
                available_position: self.snapshot.current_position,
                reason: InstalledPositionUnavailableReason::SourceNotSeekable,
            });
        }
        match self.dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(position.into()))) {
            Ok(_) => Ok(()),
            Err(error) => Err(InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::Position,
                error,
            }),
        }
    }
}
