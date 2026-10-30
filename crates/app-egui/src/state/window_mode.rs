//! Режим окна `AppState`: вид интерфейса и маршрут команд транспорта.
//!
//! Один `AppState` рисует либо обычное окно, либо окно браузерного режима (мост,
//! решение 120: режим обычного окна плеера). Разница — два типизированных параметра
//! экземпляра, оба задаются при сборке и больше не меняются:
//! - вид ([`WindowViewProfile`]): какие регионы интерфейса есть;
//! - маршрут транспорта ([`TransportRoute`]): кто исполняет play/pause/перемотку/скорость.
//!
//! Здесь же — ЕДИНСТВЕННАЯ точка, где команды транспорта интерфейса (кнопки, таймлайн,
//! хоткеи оболочки и кадра) уходят плееру: [`AppState::send_transport_command`]. В
//! браузерном окне она превращает команду в намерение зрителя и кладёт его в ящик; окно
//! забирает ящик каждый проход цикла и отдаёт медиа-части сеанса.

use std::time::Duration;

use player_core::{PlaybackState, PlayerCommand, PlayerWorkerSendError};

use super::AppState;
use crate::ui::player_controls::TransportControlAction;
use crate::viewer_transport::{
    TransportRoute, ViewerTransportIntent, ViewerTransportOutbox, viewer_intent_for_command,
};
use crate::window_view_profile::WindowViewProfile;

/// Режим окна (см. описание модуля).
#[derive(Debug, Default)]
pub(crate) struct WindowMode {
    view: WindowViewProfile,
    transport: TransportRoute,
}

impl WindowMode {
    /// Обычное окно: полный интерфейс, команды транспорта — плееру напрямую.
    pub(crate) fn standard() -> Self {
        Self::default()
    }

    /// Окно браузерного режима: интерфейс без очереди и открытия, транспорт ведёт
    /// медиа-часть сеанса (плеер + страница).
    pub(crate) fn browser_session() -> Self {
        Self {
            view: WindowViewProfile::BrowserSession,
            transport: TransportRoute::PageLed(ViewerTransportOutbox::default()),
        }
    }

    /// Ведёт ли транспорт страница (команды транспорта — намерения зрителя).
    fn is_page_led(&self) -> bool {
        matches!(self.transport, TransportRoute::PageLed(_))
    }

    /// Куда идёт команда транспорта интерфейса. `state`/`position` — что видит зритель:
    /// по ним «переключить» становится play/pause, относительная перемотка — абсолютной.
    fn route_command(
        &mut self,
        command: PlayerCommand,
        state: PlaybackState,
        position: Duration,
    ) -> CommandRoute {
        let TransportRoute::PageLed(outbox) = &mut self.transport else {
            return CommandRoute::ToPlayer(command);
        };
        match viewer_intent_for_command(&command, state, position) {
            Some(intent) => {
                outbox.push(intent);
                CommandRoute::ToSession
            }
            None => CommandRoute::ToPlayer(command),
        }
    }
}

/// Куда ушла команда транспорта.
#[derive(Debug)]
enum CommandRoute {
    /// Плееру напрямую (обычное окно или не транспортная команда).
    ToPlayer(PlayerCommand),
    /// Намерением зрителя в ящик медиа-части сеанса.
    ToSession,
}

/// Что стало с действием транспорта очереди в окне, где транспорт ведёт страница.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageLedTransportAction {
    /// Окно обычное: действие исполняет очередь как всегда.
    NotPageLed,
    /// Действие стало намерением зрителя (play/pause).
    SentToSession,
    /// Навигации по очереди в этом окне нет (Previous/Next/Shuffle/Repeat): действие
    /// не исполняется.
    NoQueueInThisWindow,
}

impl AppState {
    /// Вид окна этого `AppState`.
    pub(crate) const fn view_profile(&self) -> WindowViewProfile {
        self.window_mode.view
    }

    /// Единая точка отправки команд транспорта интерфейса плееру.
    ///
    /// Обычное окно — команда уходит плееру как есть. Окно браузерного режима —
    /// транспортная команда (play/pause/переключить, перемотка, скорость) становится
    /// намерением зрителя: «переключить» разрешается по состоянию плеера, относительная
    /// перемотка — от текущей позиции; остальные команды идут плееру как есть.
    ///
    /// # Errors
    /// Очередь команд плеера отказала (только для команд, ушедших плееру).
    pub(crate) fn send_transport_command(
        &mut self,
        command: PlayerCommand,
    ) -> Result<(), PlayerWorkerSendError> {
        if !self.window_mode.is_page_led() {
            return self.player_worker.try_send_command(command);
        }
        let snapshot = self.refresh_player_snapshot();
        let route = self.window_mode.route_command(
            command,
            snapshot.playback_state,
            snapshot.current_position,
        );
        match route {
            CommandRoute::ToPlayer(command) => self.player_worker.try_send_command(command),
            CommandRoute::ToSession => Ok(()),
        }
    }

    /// Действие транспорта очереди (`TransportControlAction`) в окне, где транспорт ведёт
    /// страница: play/pause — намерение зрителя, навигации по очереди нет.
    pub(crate) fn route_page_led_transport_action(
        &mut self,
        action: TransportControlAction,
    ) -> PageLedTransportAction {
        if !self.window_mode.is_page_led() {
            return PageLedTransportAction::NotPageLed;
        }
        match action {
            TransportControlAction::TogglePlayback => {
                if let Err(error) = self.send_transport_command(PlayerCommand::TogglePlayback) {
                    tracing::warn!(%error, "Could not toggle playback");
                }
                PageLedTransportAction::SentToSession
            }
            TransportControlAction::Previous
            | TransportControlAction::Next
            | TransportControlAction::SetShuffleEnabled { .. }
            | TransportControlAction::SetRepeatMode { .. }
            | TransportControlAction::CancelNavigation => {
                tracing::debug!(
                    ?action,
                    "Queue navigation is not available in a page-led window"
                );
                PageLedTransportAction::NoQueueInThisWindow
            }
        }
    }

    /// Забирает намерения зрителя для медиа-части сеанса (обычное окно — всегда пусто).
    pub(crate) fn take_viewer_transport_intents(&mut self) -> Vec<ViewerTransportIntent> {
        match &mut self.window_mode.transport {
            TransportRoute::LocalPlayer => Vec::new(),
            TransportRoute::PageLed(outbox) => outbox.take(),
        }
    }
}

#[cfg(test)]
mod tests {
    use player_core::{PlaybackRate, SeekRequest};

    use super::*;

    const POSITION: Duration = Duration::from_secs(20);

    fn routed_to_player(route: CommandRoute) -> PlayerCommand {
        match route {
            CommandRoute::ToPlayer(command) => command,
            CommandRoute::ToSession => panic!("expected the command to reach the player"),
        }
    }

    /// Обычное окно: транспорт по-прежнему командует плееру, ящик пуст.
    #[test]
    fn the_standard_window_sends_transport_commands_to_its_player() {
        let mut mode = WindowMode::standard();
        let command = routed_to_player(mode.route_command(
            PlayerCommand::TogglePlayback,
            PlaybackState::Playing,
            POSITION,
        ));
        assert!(matches!(command, PlayerCommand::TogglePlayback));
        assert!(!mode.is_page_led());
        assert_eq!(mode.view, WindowViewProfile::Standard);
    }

    /// Браузерное окно: пробел, стрелки, скорость становятся намерениями зрителя в
    /// порядке действий; плеер их напрямую не получает. Громкость — плееру как была.
    #[test]
    fn the_browser_window_turns_transport_into_viewer_intents() {
        let mut mode = WindowMode::browser_session();
        let rate = PlaybackRate::new(1.25).expect("valid rate");
        for command in [
            PlayerCommand::TogglePlayback,
            PlayerCommand::Seek(SeekRequest::relative(Duration::from_secs(5))),
            PlayerCommand::SetPlaybackRate(rate),
        ] {
            assert!(matches!(
                mode.route_command(command, PlaybackState::Playing, POSITION),
                CommandRoute::ToSession
            ));
        }
        let volume = routed_to_player(mode.route_command(
            PlayerCommand::SetVolume(0.3),
            PlaybackState::Playing,
            POSITION,
        ));
        assert!(matches!(volume, PlayerCommand::SetVolume(_)));
        let TransportRoute::PageLed(outbox) = &mut mode.transport else {
            panic!("the browser window is page-led");
        };
        assert_eq!(
            outbox.take(),
            [
                ViewerTransportIntent::Pause,
                ViewerTransportIntent::Seek(Duration::from_secs(25)),
                ViewerTransportIntent::SetRate(rate),
            ]
        );
    }
}
