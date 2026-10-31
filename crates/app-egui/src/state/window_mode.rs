//! Режим окна `AppState`: вид интерфейса и маршрут команд транспорта.
//!
//! Один `AppState` рисует либо обычное окно, либо окно браузерного режима (мост,
//! решение 120: режим обычного окна плеера). Разница — два типизированных параметра
//! экземпляра, оба задаются при сборке и больше не меняются:
//! - вид ([`WindowViewProfile`]): какие регионы интерфейса есть;
//! - маршрут транспорта ([`TransportRoute`]): кто исполняет play/pause/перемотку/скорость.
//!
//! И одно сведение, которое приносит тот, кто ведёт транспорт (медиа-часть сеанса,
//! каждый проход окна): что у показанного медиа уже скачано ([`DownloadedRange`]) —
//! по нему таймлайн решает, куда живое перетаскивание может вести плеер без просьбы
//! странице (`LiveScrubScope::DownloadedRangeOnly`). Обычное окно его не получает.
//!
//! Здесь же — ЕДИНСТВЕННАЯ точка, где команды транспорта интерфейса (кнопки, таймлайн,
//! хоткеи оболочки и кадра) уходят плееру: [`AppState::send_transport_command`]. В
//! браузерном окне она превращает команду в намерение зрителя и кладёт его в ящик; окно
//! забирает ящик каждый проход цикла и отдаёт медиа-части сеанса.

use std::borrow::Cow;
use std::time::Duration;

use player_core::{PlaybackState, PlayerCommand, PlayerWorkerSendError};

use super::AppState;
use crate::downloaded_range::DownloadedRange;
use crate::page_label::PageLabel;
use crate::ui::player_controls::TransportControlAction;
use crate::viewer_transport::{
    TransportRoute, ViewerTransportIntent, ViewerTransportOutbox, viewer_intent_for_command,
};
use crate::window_view_profile::WindowViewProfile;

/// Название приложения в шапке окна, когда страницы-источника нет.
const APP_CHROME_TITLE: &str = "Fastiplayer";

/// Режим окна (см. описание модуля).
#[derive(Debug, Default)]
pub(crate) struct WindowMode {
    view: WindowViewProfile,
    transport: TransportRoute,
    /// Что скачано у показанного медиа (см. описание модуля).
    downloaded_range: DownloadedRange,
    /// Страница-источник (только окно браузерного режима; только в памяти, 04c).
    page: Option<PageLabel>,
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
            downloaded_range: DownloadedRange::Unknown,
            page: None,
        }
    }

    /// Сведение о скачанном от того, кто ведёт транспорт; обычное окно его не берёт.
    fn note_downloaded_range(&mut self, range: DownloadedRange) {
        if self.is_page_led() {
            self.downloaded_range = range;
        }
    }

    /// Текст шапки окна (см. `AppState::window_chrome_title`).
    fn chrome_title(&self) -> Cow<'_, str> {
        match self.page_label() {
            Some(page) => Cow::Owned(page.window_title()),
            None => Cow::Borrowed(APP_CHROME_TITLE),
        }
    }

    /// Страница-источник (обычное окно — `None`).
    pub(super) const fn page_label(&self) -> Option<&PageLabel> {
        self.page.as_ref()
    }

    /// Название страницы от того, кто ведёт транспорт; обычное окно его не берёт.
    fn note_page(&mut self, page: PageLabel) {
        if self.is_page_led() {
            self.page = Some(page);
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

    /// Медиа-часть сеанса сообщила, что скачано у показанного медиа (каждый проход
    /// окна; `Unknown` — нет показанного поколения потока, файл, эфир или открывается
    /// новое). Обычному окну сведение не нужно: его плеер достаёт байты сам.
    pub(crate) fn note_downloaded_range(&mut self, range: DownloadedRange) {
        self.window_mode.note_downloaded_range(range);
    }

    /// Что скачано у показанного медиа (обычное окно — всегда `Unknown`).
    pub(super) const fn downloaded_range(&self) -> &DownloadedRange {
        &self.window_mode.downloaded_range
    }

    /// Окно браузерного режима узнало страницу (новый сеанс или замена вкладки).
    /// Обычному окну страницы не называют: подпись игнорируется.
    pub(crate) fn note_page_label(&mut self, page: PageLabel) {
        self.window_mode.note_page(page);
    }

    /// Текст в нарисованной шапке окна: в браузерном окне — «<заголовок> — <домен>»
    /// страницы-источника, пока подписи нет и в обычном окне — название приложения.
    /// Тот же текст, что `set_title` отдаёт системе, чтобы шапка и панель задач не
    /// расходились.
    pub(crate) fn window_chrome_title(&self) -> Cow<'_, str> {
        self.window_mode.chrome_title()
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

    /// Заголовок шапки: браузерное окно с подписью показывает страницу, без подписи и
    /// обычное окно (даже если ему назвали страницу) — название приложения.
    #[test]
    fn the_chrome_title_follows_the_page_only_in_the_browser_window() {
        let mut browser = WindowMode::browser_session();
        assert_eq!(browser.chrome_title(), "Fastiplayer");
        browser.note_page(PageLabel::new("Cats", "video.example"));
        assert_eq!(browser.chrome_title(), "Cats — video.example");

        let mut standard = WindowMode::standard();
        standard.note_page(PageLabel::new("Cats", "video.example"));
        assert_eq!(standard.chrome_title(), "Fastiplayer");
    }

    fn routed_to_player(route: CommandRoute) -> PlayerCommand {
        match route {
            CommandRoute::ToPlayer(command) => command,
            CommandRoute::ToSession => panic!("expected the command to reach the player"),
        }
    }

    /// Страница называется только в окне браузерного режима, а замена вкладки
    /// переименовывает её.
    #[test]
    fn only_the_browser_window_keeps_the_page_and_a_new_page_replaces_it() {
        let mut standard = WindowMode::standard();
        standard.note_page(PageLabel::new("T", "a.example"));
        assert_eq!(standard.page, None);

        let mut browser = WindowMode::browser_session();
        assert_eq!(browser.page, None);
        browser.note_page(PageLabel::new("First", "a.example"));
        browser.note_page(PageLabel::new("Second", "b.example"));
        assert_eq!(browser.page, Some(PageLabel::new("Second", "b.example")));
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

    /// Сведение о скачанном берёт только окно, которое ведёт страница: у обычного окна
    /// перетаскивание не зависит от скачанного, и чужое сведение его не меняет.
    #[test]
    fn only_the_page_led_window_keeps_the_downloaded_range() {
        let (_spans, range) =
            crate::downloaded_range::test_support::FakeDownloadedSpans::range(vec![
                Duration::ZERO..POSITION,
            ]);
        let mut standard = WindowMode::standard();
        standard.note_downloaded_range(range.clone());
        assert!(!standard.downloaded_range.is_known());

        let mut browser = WindowMode::browser_session();
        browser.note_downloaded_range(range);
        assert!(browser.downloaded_range.covers(Duration::from_secs(1)));
        browser.note_downloaded_range(DownloadedRange::Unknown);
        assert!(!browser.downloaded_range.is_known());
    }
}
