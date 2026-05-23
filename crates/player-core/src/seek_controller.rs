use crate::PlaybackState;

/// Намерение возобновления playback после обычного final seek transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackResumeIntent {
    /// Вернуться к paused-состоянию.
    Pause,

    /// Вернуться к playing-состоянию.
    Play,
}

impl PlaybackResumeIntent {
    /// Строит resume intent из состояния session на момент начала seek-а.
    #[must_use]
    pub const fn from_playback_state(playback_state: PlaybackState) -> Self {
        match playback_state {
            PlaybackState::Playing
            | PlaybackState::Buffering
            | PlaybackState::Seeking
            | PlaybackState::Draining => Self::Play,
            PlaybackState::Idle
            | PlaybackState::Opening
            | PlaybackState::Paused
            | PlaybackState::Ended
            | PlaybackState::Stopped
            | PlaybackState::Failed => Self::Pause,
        }
    }
}
