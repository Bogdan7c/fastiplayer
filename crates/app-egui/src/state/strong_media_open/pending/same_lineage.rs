//! Same-lineage barrier snapshot и commit policy общего strong-open envelope-а.

use super::{
    AppState, PendingStrongMediaOpen, PlaybackIntent, PlaylistRuntime, StrongMediaOpenError,
};

/// Commit policy явно отделяет новую lineage от S25 same-item rebind-а.
pub(super) enum PendingStrongLineageCommit {
    /// Обычный queue/external install использует существующий registration route.
    NewLineageOrQueue,
    /// S25 сохраняет exact app lineage и свежий playback restore snapshot.
    SameLineage {
        expected_active: crate::playlist_runtime::ActiveMediaIdentity,
        restore: Option<SameLineageRestoreSnapshot>,
        /// Visual checkpoint снимается в последнем pre-barrier состоянии старого media.
        video_swap_checkpoint: Option<Box<crate::state::BackendSwapVideoCheckpoint>>,
    },
}

/// Свежие controls снимаются у старого instance непосредственно перед barrier-ом.
#[derive(Debug, Clone)]
pub(super) struct SameLineageRestoreSnapshot {
    pub(super) position: std::time::Duration,
    pub(super) volume: f32,
    pub(super) selected_tracks: player_core::TrackSelectionSnapshot,
}

impl AppState {
    /// Снимает fresh controls у старого instance только в последнем pre-barrier состоянии.
    pub(super) fn capture_same_lineage_restore_before_barrier(
        &mut self,
        playlist_runtime: &PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
    ) -> Result<(), StrongMediaOpenError> {
        let PendingStrongLineageCommit::SameLineage {
            expected_active,
            restore,
            video_swap_checkpoint,
            ..
        } = &mut pending.lineage_commit
        else {
            return Ok(());
        };
        let current_active = playlist_runtime.playlist_view_snapshot().active_media();
        let snapshot = self.refresh_player_snapshot();
        if current_active != Some(*expected_active)
            || snapshot.media_instance_id != Some(expected_active.media_instance_id())
        {
            return Err(StrongMediaOpenError::SameLineageStale);
        }
        let visual_checkpoint = self.capture_backend_swap_video_checkpoint();
        let (fresh_intent, fresh_restore) = same_lineage_controls_from_snapshot(snapshot);
        pending.intent = fresh_intent;
        let next_revision = pending
            .intent_revision
            .get()
            .checked_add(1)
            .and_then(std::num::NonZeroU64::new)
            .map(player_core::PlaybackIntentRevision::from_non_zero)
            .ok_or(StrongMediaOpenError::PendingPhaseStateLost)?;
        playlist_runtime
            .update_media_open_playback_intent(pending.request_id, next_revision, fresh_intent)
            .map_err(StrongMediaOpenError::Command)?;
        pending.intent_revision = next_revision;
        *restore = Some(fresh_restore);
        *video_swap_checkpoint = Some(Box::new(visual_checkpoint));
        Ok(())
    }
}

/// Чистая граница фиксирует, какие fresh controls пересекают S25 barrier.
fn same_lineage_controls_from_snapshot(
    snapshot: player_core::PlayerSnapshot,
) -> (PlaybackIntent, SameLineageRestoreSnapshot) {
    (
        super::super::super::playback_intent_from_snapshot(&snapshot),
        SameLineageRestoreSnapshot {
            position: snapshot.current_position,
            volume: snapshot.volume,
            selected_tracks: snapshot.selected_tracks,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barrier_captures_fresh_playing_and_paused_controls() {
        let mut playing = player_core::PlayerSnapshot::empty();
        playing.playback_state = player_core::PlaybackState::Playing;
        playing.current_position = std::time::Duration::from_secs(17);
        playing.volume = 0.41;
        playing.selected_tracks.video_track = Some(media_core::TrackId::new(3));
        let (playing_intent, playing_restore) = same_lineage_controls_from_snapshot(playing);

        assert_eq!(playing_intent, player_core::PlaybackIntent::StartPlaying);
        assert_eq!(playing_restore.position, std::time::Duration::from_secs(17));
        assert!((playing_restore.volume - 0.41).abs() < f32::EPSILON);
        assert_eq!(
            playing_restore.selected_tracks.video_track,
            Some(media_core::TrackId::new(3))
        );

        let mut paused = player_core::PlayerSnapshot::empty();
        paused.playback_state = player_core::PlaybackState::Paused;
        paused.current_position = std::time::Duration::from_secs(29);
        paused.volume = 0.73;
        let (paused_intent, paused_restore) = same_lineage_controls_from_snapshot(paused);

        assert_eq!(paused_intent, player_core::PlaybackIntent::StartPaused);
        assert_eq!(paused_restore.position, std::time::Duration::from_secs(29));
        assert!((paused_restore.volume - 0.73).abs() < f32::EPSILON);
    }
}
