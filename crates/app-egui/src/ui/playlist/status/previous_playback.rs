//! Что сейчас происходит с ранее открытым воспроизведением.
//!
//! Строка статуса после неудачного ручного выбора элемента должна честно сказать, продолжает
//! ли что-то играть: нельзя утверждать «предыдущий трек продолжает играть», когда плеер в
//! `Idle`/`Failed`/`Ended`. Источник правды — состояние плеера в кадре, а не память очереди:
//! очередь не знает, что воспроизведение упало или остановлено.

use player_core::PlaybackState;

/// Состояние ранее открытого воспроизведения на момент показа строки статуса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviousPlayback {
    /// Что-то воспроизводится (в том числе буферизуется или перематывается).
    Playing,
    /// Медиа открыто, но стоит на паузе.
    Paused,
    /// Ничего не открыто или воспроизведение закончилось/упало.
    NothingPlaying,
}

impl PreviousPlayback {
    /// Сводит состояние плеера к трём фактам, важным для текста статуса.
    ///
    /// `match` исчерпывающий намеренно: новое состояние плеера не скомпилируется, пока для
    /// него не решено, что сказать пользователю.
    pub(crate) const fn from_playback_state(state: PlaybackState) -> Self {
        match state {
            PlaybackState::Playing
            | PlaybackState::Buffering
            | PlaybackState::Seeking
            | PlaybackState::Scrubbing
            | PlaybackState::Draining => Self::Playing,
            PlaybackState::Paused => Self::Paused,
            PlaybackState::Idle
            | PlaybackState::Opening
            | PlaybackState::Ended
            | PlaybackState::Stopped
            | PlaybackState::Failed => Self::NothingPlaying,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_player_state_maps_to_what_the_user_actually_sees() {
        let expectations = [
            (PlaybackState::Playing, PreviousPlayback::Playing),
            (PlaybackState::Buffering, PreviousPlayback::Playing),
            (PlaybackState::Seeking, PreviousPlayback::Playing),
            (PlaybackState::Scrubbing, PreviousPlayback::Playing),
            (PlaybackState::Draining, PreviousPlayback::Playing),
            (PlaybackState::Paused, PreviousPlayback::Paused),
            (PlaybackState::Idle, PreviousPlayback::NothingPlaying),
            (PlaybackState::Opening, PreviousPlayback::NothingPlaying),
            (PlaybackState::Ended, PreviousPlayback::NothingPlaying),
            (PlaybackState::Stopped, PreviousPlayback::NothingPlaying),
            (PlaybackState::Failed, PreviousPlayback::NothingPlaying),
        ];
        for (state, expected) in expectations {
            assert_eq!(PreviousPlayback::from_playback_state(state), expected);
        }
    }
}
