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
            PlaybackState::Playing | PlaybackState::Paused => {
                self.snapshot.playback_rate = playback_rate;
                PlayerCommandOutcome::Applied
            }
            state => PlayerCommandOutcome::Rejected(
                PlayerCommandReject::PlaybackRateUnavailableForState { state },
            ),
        }
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
