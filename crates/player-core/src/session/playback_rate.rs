use std::time::{Duration, Instant};

use media_core::MediaTime;

use crate::{
    PlaybackRate, PlaybackState, PlayerCommandOutcome, PlayerCommandReject, PlayerSession,
};

/// Политика скорости при новом media load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackRateMediaLoadPolicy {
    /// V1 всегда возвращает новое media к прежнему `1.0x` поведению.
    ResetOnMediaLoad,

    /// Future extension point для playlist/session-preserve сценариев без TOML/UI wiring в S33.
    #[allow(dead_code)]
    PreserveAcrossPlaylist,
}

impl PlayerSession {
    /// Применяет checked playback rate только в состояниях, где S33 разрешает mutation.
    pub(super) fn set_playback_rate(
        &mut self,
        playback_rate: PlaybackRate,
    ) -> PlayerCommandOutcome {
        match self.playback_state() {
            PlaybackState::Playing => self.apply_playing_playback_rate(playback_rate),
            PlaybackState::Paused => self.apply_paused_playback_rate(playback_rate),
            state => PlayerCommandOutcome::Rejected(
                PlayerCommandReject::PlaybackRateUnavailableForState { state },
            ),
        }
    }

    /// Применяет rate во время playback без seek transaction и generation advance.
    fn apply_playing_playback_rate(&mut self, playback_rate: PlaybackRate) -> PlayerCommandOutcome {
        let observed_at = Instant::now();
        let current_media_position = self.presentation_clock_position_at(observed_at);

        self.snapshot.playback_rate = playback_rate;
        self.set_snapshot_position_for_playback_rate(current_media_position);

        if !self.pipeline.has_audio_clock() {
            self.pipeline.start_monotonic_media_clock(
                current_media_position,
                observed_at,
                playback_rate,
            );
        }

        PlayerCommandOutcome::Applied
    }

    /// Применяет rate на pause без движения media clock.
    fn apply_paused_playback_rate(&mut self, playback_rate: PlaybackRate) -> PlayerCommandOutcome {
        self.snapshot.playback_rate = playback_rate;
        PlayerCommandOutcome::Applied
    }

    /// Обновляет snapshot position без side-effect re-anchor внутри `update_current_position`.
    fn set_snapshot_position_for_playback_rate(&mut self, position: Duration) {
        self.snapshot
            .set_timeline_position(MediaTime::from_duration(position));
    }

    /// Возвращает текущую internal policy без чтения config/settings.
    fn playback_rate_media_load_policy(&self) -> PlaybackRateMediaLoadPolicy {
        PlaybackRateMediaLoadPolicy::ResetOnMediaLoad
    }

    /// Применяет media-load policy к session-owned snapshot storage.
    pub(super) fn apply_playback_rate_media_load_policy(&mut self) {
        match self.playback_rate_media_load_policy() {
            PlaybackRateMediaLoadPolicy::ResetOnMediaLoad => {
                self.snapshot.playback_rate = PlaybackRate::NORMAL;
            }
            PlaybackRateMediaLoadPolicy::PreserveAcrossPlaylist => {}
        }
    }
}
