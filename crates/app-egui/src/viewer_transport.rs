//! Намерения зрителя по транспорту и маршрут команд транспорта окна.
//!
//! В обычном окне кнопки, таймлайн и хоткеи командуют плееру напрямую. В окне
//! браузерного режима транспорт ведёт страницу (мост, сессия 04a): пауза, play,
//! перемотка и скорость зрителя исполняются у нас и параллельно уходят странице
//! командой протокола. Владелец плеера и исходящих странице в этом окне — медиа-часть
//! сеанса (`browser_session::session_media::SessionMedia`), поэтому окно не командует
//! плееру само, а отдаёт ей **намерение зрителя** ([`ViewerTransportIntent`]):
//!
//! ```text
//!  кнопка / таймлайн / хоткей ─► AppState::send_transport_command (одна точка)
//!        TransportRoute::LocalPlayer ─► PlayerCommand ─► плеер          (обычное окно)
//!        TransportRoute::PageLed     ─► ViewerTransportOutbox ─► окно забирает каждый
//!                                       проход ─► SessionMedia: плеер + команда странице
//! ```
//!
//! Модуль нейтрален: ничего не знает ни о мосте, ни об окне — только словарь намерений,
//! перевод команды транспорта в намерение и ящик.

use std::time::Duration;

use media_core::MediaTime;
use player_core::{PlaybackRate, PlaybackState, PlayerCommand};

/// Что зритель сделал с транспортом (уже разрешённое: «переключить» стало play/pause,
/// относительная перемотка — абсолютной целью).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ViewerTransportIntent {
    /// Играть.
    Play,
    /// Пауза.
    Pause,
    /// Точная перемотка к цели.
    Seek(Duration),
    /// Скорость (плеер сохраняет тон).
    SetRate(PlaybackRate),
}

/// Сколько намерений держит ящик между проходами цикла окна.
///
/// Окно забирает ящик каждый проход, а человек за кадр делает единицы действий;
/// 64 — это много проходов подряд без забора (зависший цикл). Переполнение не теряет
/// последнюю волю зрителя молча: выбрасывается самое старое намерение, а в журнал
/// уходит предупреждение (последнее действие важнее устаревших).
pub(crate) const VIEWER_TRANSPORT_OUTBOX_CAPACITY: usize = 64;

/// Ящик намерений зрителя, ждущих медиа-часть сеанса.
#[derive(Debug, Default)]
pub(crate) struct ViewerTransportOutbox {
    intents: Vec<ViewerTransportIntent>,
}

impl ViewerTransportOutbox {
    /// Кладёт намерение в порядке действий зрителя.
    pub(crate) fn push(&mut self, intent: ViewerTransportIntent) {
        if self.intents.len() >= VIEWER_TRANSPORT_OUTBOX_CAPACITY {
            tracing::warn!("The viewer transport outbox is full; the oldest action is dropped");
            self.intents.remove(0);
        }
        self.intents.push(intent);
    }

    /// Забирает все намерения в порядке подачи.
    pub(crate) fn take(&mut self) -> Vec<ViewerTransportIntent> {
        std::mem::take(&mut self.intents)
    }
}

/// Кто исполняет команды транспорта окна.
#[derive(Debug, Default)]
pub(crate) enum TransportRoute {
    /// Обычное окно: команды идут плееру напрямую.
    #[default]
    LocalPlayer,
    /// Окно браузерного режима: команды транспорта становятся намерениями зрителя, их
    /// исполняет медиа-часть сеанса (плеер + страница).
    PageLed(ViewerTransportOutbox),
}

/// Команда плееру → намерение зрителя; `None` — команда не транспортная (громкость,
/// субтитры, scrub-маршрут и т. п.) и идёт плееру как есть.
///
/// `state` и `position` — то, что видит зритель сейчас: «переключить» решается по
/// состоянию плеера, относительная перемотка — от текущей позиции.
pub(crate) fn viewer_intent_for_command(
    command: &PlayerCommand,
    state: PlaybackState,
    position: Duration,
) -> Option<ViewerTransportIntent> {
    match command {
        PlayerCommand::Play => Some(ViewerTransportIntent::Play),
        PlayerCommand::Pause => Some(ViewerTransportIntent::Pause),
        PlayerCommand::TogglePlayback => Some(if plays_for_the_viewer(state) {
            ViewerTransportIntent::Pause
        } else {
            ViewerTransportIntent::Play
        }),
        PlayerCommand::Seek(request) => {
            let target = request
                .target
                .resolve(MediaTime::from_duration(position))
                .as_duration();
            Some(ViewerTransportIntent::Seek(target))
        }
        PlayerCommand::SetPlaybackRate(rate) => Some(ViewerTransportIntent::SetRate(*rate)),
        _ => None,
    }
}

/// Видит ли зритель воспроизведение (тогда «переключить» = пауза).
///
/// Ожидание данных и перемотка — тоже «играет»: зритель нажал play, и плеер идёт к
/// кадру; пауза в этот момент — его новая воля.
const fn plays_for_the_viewer(state: PlaybackState) -> bool {
    matches!(
        state,
        PlaybackState::Playing
            | PlaybackState::Opening
            | PlaybackState::Buffering
            | PlaybackState::Seeking
            | PlaybackState::Scrubbing
            | PlaybackState::Draining
    )
}

#[cfg(test)]
mod tests {
    use player_core::SeekRequest;

    use super::*;

    #[test]
    fn toggle_becomes_pause_while_playing_and_play_otherwise() {
        let toggle = PlayerCommand::TogglePlayback;
        for state in [
            PlaybackState::Playing,
            PlaybackState::Buffering,
            PlaybackState::Seeking,
            PlaybackState::Opening,
        ] {
            assert_eq!(
                viewer_intent_for_command(&toggle, state, Duration::ZERO),
                Some(ViewerTransportIntent::Pause),
                "{state:?}"
            );
        }
        for state in [
            PlaybackState::Paused,
            PlaybackState::Ended,
            PlaybackState::Idle,
        ] {
            assert_eq!(
                viewer_intent_for_command(&toggle, state, Duration::ZERO),
                Some(ViewerTransportIntent::Play),
                "{state:?}"
            );
        }
    }

    #[test]
    fn seeks_become_absolute_targets_and_rates_pass_through() {
        let position = Duration::from_secs(10);
        let absolute = PlayerCommand::Seek(SeekRequest::accurate(Duration::from_secs(3)));
        assert_eq!(
            viewer_intent_for_command(&absolute, PlaybackState::Playing, position),
            Some(ViewerTransportIntent::Seek(Duration::from_secs(3)))
        );
        let relative = PlayerCommand::Seek(SeekRequest::relative(Duration::from_secs(5)));
        assert_eq!(
            viewer_intent_for_command(&relative, PlaybackState::Playing, position),
            Some(ViewerTransportIntent::Seek(Duration::from_secs(15)))
        );
        let rate = PlaybackRate::new(1.5).expect("valid rate");
        assert_eq!(
            viewer_intent_for_command(
                &PlayerCommand::SetPlaybackRate(rate),
                PlaybackState::Paused,
                position
            ),
            Some(ViewerTransportIntent::SetRate(rate))
        );
    }

    #[test]
    fn non_transport_commands_stay_with_the_local_player() {
        assert_eq!(
            viewer_intent_for_command(
                &PlayerCommand::SetVolume(0.5),
                PlaybackState::Playing,
                Duration::ZERO
            ),
            None
        );
    }

    #[test]
    fn a_full_outbox_keeps_the_newest_action() {
        let mut outbox = ViewerTransportOutbox::default();
        for _ in 0..VIEWER_TRANSPORT_OUTBOX_CAPACITY {
            outbox.push(ViewerTransportIntent::Play);
        }
        outbox.push(ViewerTransportIntent::Pause);
        let taken = outbox.take();
        assert_eq!(taken.len(), VIEWER_TRANSPORT_OUTBOX_CAPACITY);
        assert_eq!(taken.last(), Some(&ViewerTransportIntent::Pause));
        assert!(outbox.take().is_empty(), "taking empties the outbox");
    }
}
