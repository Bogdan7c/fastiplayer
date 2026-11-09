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

use self::ad_gate::{AdTransportGate, GateVerdict};
use self::quality_menu::QualityLabelJournal;
use super::AppState;
use crate::ad_plate::{AdActionProgress, AdPlate, AdPlateAction};
use crate::downloaded_range::DownloadedRange;
use crate::page_label::PageLabel;
use crate::page_quality::PageQualityFacts;
use crate::page_variants::PageVariantFacts;
use crate::ui::player_controls::TransportControlAction;
use crate::viewer_transport::{
    TransportRoute, ViewerTransportIntent, ViewerTransportOutbox, viewer_intent_for_command,
};
use crate::window_view_profile::WindowViewProfile;

mod ad_gate;
mod page_quality;
mod page_subtitle_choice;
mod page_variants;
mod quality_menu;

use self::page_variants::NextActivation;
pub(crate) use self::page_variants::PageVariantSelectOutcome;
pub(crate) use self::quality_menu::QualityMenuView;

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
    /// Плашка рекламы и запрет перемотки со скоростью, пока она на экране (09d).
    ad_gate: AdTransportGate,
    /// Факты сеанса о качестве для кнопки качества (13c, [`page_quality`]).
    page_quality: PageQualityFacts,
    /// Помнит последнюю записанную в журнал подпись качества.
    quality_journal: QualityLabelJournal,
    /// Факты сеанса о вариантах страницы: меню озвучек и качеств, следующая серия (14a).
    page_variants: PageVariantFacts,
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
            ad_gate: AdTransportGate::default(),
            page_quality: PageQualityFacts::default(),
            quality_journal: QualityLabelJournal::default(),
            page_variants: PageVariantFacts::default(),
        }
    }

    /// Слово о рекламе от того, кто ведёт транспорт; обычное окно его не берёт.
    /// `true` — плашка изменилась.
    fn note_ad_plate(&mut self, plate: Option<AdPlate>) -> bool {
        self.is_page_led() && self.ad_gate.note_plate(plate)
    }

    /// Намерение зрителя по кнопке плашки; `false` — в этом окне плашке не к кому идти.
    fn request_ad_action(&mut self, action: AdPlateAction) -> bool {
        let TransportRoute::PageLed(outbox) = &mut self.transport else {
            return false;
        };
        outbox.push(match action {
            AdPlateAction::Skip => ViewerTransportIntent::SkipAd,
        });
        true
    }

    /// Плашка предлагает кнопку, и она свободна (команда за ней ещё не ушла).
    fn ad_offer_is_free(&self) -> bool {
        self.ad_gate.plate().is_some_and(|plate| {
            plate.offered_action().is_some() && plate.progress == AdActionProgress::Idle
        })
    }

    /// Нажатие «следующего»: то же, что клик по кнопке плашки, если она предложена.
    fn activate_ad_offer(&mut self) -> AdOfferActivation {
        let Some(plate) = self.ad_gate.plate().copied() else {
            return AdOfferActivation::NothingOffered;
        };
        match (plate.offered_action(), plate.progress) {
            (Some(action), AdActionProgress::Idle) => {
                if self.request_ad_action(action) {
                    AdOfferActivation::Requested
                } else {
                    AdOfferActivation::NothingOffered
                }
            }
            (Some(_), AdActionProgress::InFlight) => AdOfferActivation::AlreadyInFlight,
            (None, _) => AdOfferActivation::NothingOffered,
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
        // Решение 206: пока реклама на экране, перемотка и скорость зрителя не идут никуда.
        if self.ad_gate.judge(&command) == GateVerdict::Refused {
            return CommandRoute::RefusedDuringAd;
        }
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
    /// Отклонена: на экране реклама (перемотка и скорость, решение 206).
    RefusedDuringAd,
}

/// Что вышло с «следующим» зрителя, когда плашка рекламы может предлагать кнопку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdOfferActivation {
    /// Намерение «пропустить» или «вывести страницу» ушло в ящик.
    Requested,
    /// Предыдущее нажатие ещё в пути: второе не отправлено.
    AlreadyInFlight,
    /// Плашка ничего не предлагает (рекламы нет, отсчёт, обычное окно).
    NothingOffered,
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
            CommandRoute::ToSession | CommandRoute::RefusedDuringAd => Ok(()),
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
            // «Следующее» в этом окне — кнопка плашки рекламы, если плашка её предлагает
            // (хоткей N и медиаклавиша «следующая»), вне рекламы — следующая серия страницы
            // (14a, решение 283); очереди здесь нет.
            TransportControlAction::Next
                if self.window_mode.activate_next() != NextActivation::NothingToDo =>
            {
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

    /// Слово медиа-части о рекламе (каждый проход окна): плашка рисуется, перемотка и
    /// скорость запираются. `true` — плашка изменилась, окну нужен кадр. Обычному окну
    /// плашка не нужна: сведение игнорируется.
    pub(crate) fn note_ad_plate(&mut self, plate: Option<AdPlate>) -> bool {
        let changed = self.window_mode.note_ad_plate(plate);
        if changed {
            // Во время рекламы субтитры не рисуются (решение 274), после неё — снова.
            self.follow_ad_plate_for_subtitles();
        }
        changed
    }

    /// Плашка рекламы для рисования (`None` — рекламы нет).
    pub(crate) fn ad_plate(&self) -> Option<&AdPlate> {
        self.window_mode.ad_gate.plate()
    }

    /// Зритель нажал кнопку плашки: намерение уходит медиа-части, а та решает, уйдёт ли
    /// команда странице (защита от повторного клика — у неё).
    pub(super) fn request_ad_action(&mut self, action: AdPlateAction) {
        if !self.window_mode.request_ad_action(action) {
            tracing::debug!(?action, "Ad plate action ignored: not a page-led window");
        }
    }

    /// Плашка предлагает кнопку, и она свободна: «Next» её нажмёт (MPRIS `CanGoNext`).
    pub(crate) fn ad_offer_is_free(&self) -> bool {
        self.window_mode.ad_offer_is_free()
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
mod ad_tests;
#[cfg(test)]
mod page_variant_menus_tests;

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
            CommandRoute::ToSession | CommandRoute::RefusedDuringAd => {
                panic!("expected the command to reach the player")
            }
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
