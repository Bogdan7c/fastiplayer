//! MPRIS окна браузерного режима: `org.mpris.MediaPlayer2.fastiplayer.browser` (04c).
//!
//! Медиа-виджет рабочего стола (Plasma, GNOME, `playerctl`) видит окно браузерного
//! режима как отдельного игрока рядом с обычным Fastiplayer. Команды виджета идут тем же
//! путём, что кнопки окна: `AppState::send_transport_command` → намерение зрителя →
//! медиа-часть сеанса → страница. Обычный плеер и его имя на шине (`…fastiplayer`) эта
//! часть не трогает: пауза обычного плеера при открытии сеанса (решение 89) бьёт строго
//! в его имя и браузерное окно не задевает.
//!
//! ```text
//!  шина (поток desktop-mpris) ─► почтовый ящик + пробуждение цикла
//!  проход цикла окна:  pump()  ─► план (plan.rs) ─► команда окна / просьба Raise, Quit
//!                      publish() ◄─ плеер окна (позиция, статус, громкость)
//! ```
//!
//! Владелец — [`WindowDesktopTransport`] живёт ровно пока открыто окно: стартует вместе с
//! ним и останавливается при закрытии.

mod plan;

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use desktop_integration::{
    DesktopCommand, DesktopCommandSink, DesktopIntegration, DesktopIntegrationError,
    DesktopIntegrationResult, DesktopSnapshotView, DesktopWindowRequest, DesktopWindowRequestSink,
};
use player_core::PlayerCommand;
use tracing::{debug, warn};

pub(crate) use self::plan::TransportObservation;
use self::plan::{PageInfo, PlannedAction, build_view, differs_beyond_position, plan_command};
use super::super::opened_page::PageArrival;
use crate::app_wake::{AppWakePort, WakeDelivery};
use crate::page_label::PageLabel;
use crate::process_shutdown::ShutdownDeadline;

/// Сколько команд шины ждёт между проходами цикла. Человек жмёт единицы кнопок в секунду.
const DESKTOP_COMMAND_CAPACITY: usize = 16;

/// Сколько просьб `Raise`/`Quit` ждёт между проходами.
const WINDOW_REQUEST_CAPACITY: usize = 4;

/// Как часто обновлять позицию, когда ничего другого не менялось. Виджет читает позицию
/// сам; 4 Гц достаточно для полосы прогресса и не нагружает шину.
const POSITION_PUBLISH_PERIOD: Duration = Duration::from_millis(250);

/// Окно браузерного режима глазами MPRIS: что оно умеет показать и исполнить.
pub(crate) trait DesktopWindowPort {
    /// Что сейчас видит зритель (свежий снимок плеера окна).
    fn observe_transport(&mut self) -> TransportObservation;
    /// Исполнить команду так же, как кнопку окна.
    fn send_transport_command(&mut self, command: PlayerCommand) -> Result<(), String>;
}

/// Что рабочий стол попросил у окна за проход (кроме транспорта).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DesktopWindowRequests {
    /// `Raise`: вывести окно вперёд.
    pub(crate) raise: bool,
    /// `Quit`: закрыть окно, как закрыл бы человек.
    pub(crate) quit: bool,
}

/// Ящик, в который поток шины кладёт команды и просьбы и будит цикл окна.
#[derive(Clone)]
struct DesktopMailbox {
    commands: SyncSender<DesktopCommand>,
    window_requests: SyncSender<DesktopWindowRequest>,
    wake_port: AppWakePort,
}

impl DesktopMailbox {
    /// Будит цикл окна; закрытый цикл — отказ (окно уже уходит).
    fn wake(&self) -> bool {
        !matches!(self.wake_port.request_wake(), WakeDelivery::EventLoopClosed)
    }
}

impl DesktopCommandSink for DesktopMailbox {
    fn send_desktop_command(&self, command: DesktopCommand) -> DesktopIntegrationResult<()> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => DesktopIntegrationError::CommandBackpressure,
                TrySendError::Disconnected(_) => DesktopIntegrationError::CommandDisconnected,
            })?;
        if self.wake() {
            Ok(())
        } else {
            Err(DesktopIntegrationError::CommandDisconnected)
        }
    }
}

impl DesktopWindowRequestSink for DesktopMailbox {
    fn request_window(&self, request: DesktopWindowRequest) {
        // Переполнение не страшно: просьба «поднять»/«закрыть» повторяется человеком.
        if self.window_requests.try_send(request).is_ok() {
            self.wake();
        }
    }
}

/// Владелец MPRIS-сервера окна, его ящика и последнего опубликованного вида.
pub(crate) struct WindowDesktopTransport {
    integration: Option<DesktopIntegration>,
    commands: Receiver<DesktopCommand>,
    window_requests: Receiver<DesktopWindowRequest>,
    /// Страница показанного видео (до первой — `None`).
    page: Option<PageInfo>,
    /// Сколько видео окно уже показало; номер текущего.
    lineage: u64,
    revision: u64,
    last_view: DesktopSnapshotView,
    last_position_publish: Option<Instant>,
}

impl WindowDesktopTransport {
    /// Поднимает сервер на имени `…fastiplayer.browser`. Не вышло (имя занято, нет шины) —
    /// окно работает без MPRIS, причина в журнале.
    pub(crate) fn start(wake_port: AppWakePort, initial: &TransportObservation) -> Self {
        let (command_tx, commands) = sync_channel(DESKTOP_COMMAND_CAPACITY);
        let (request_tx, window_requests) = sync_channel(WINDOW_REQUEST_CAPACITY);
        let mailbox = DesktopMailbox {
            commands: command_tx,
            window_requests: request_tx,
            wake_port,
        };
        let last_view = build_view(0, None, initial);
        let integration = match DesktopIntegration::spawn_browser_window(
            mailbox.clone(),
            Arc::new(mailbox),
            last_view.clone(),
        ) {
            Ok(integration) => Some(integration),
            Err(DesktopIntegrationError::MprisBusNameUnavailable) => {
                warn!("The browser window MPRIS name is busy; the window runs without it");
                None
            }
            Err(error) => {
                warn!(%error, "The browser window MPRIS is disabled; the window keeps working");
                None
            }
        };
        Self {
            integration,
            commands,
            window_requests,
            page: None,
            lineage: 0,
            revision: 0,
            last_view,
            last_position_publish: None,
        }
    }

    /// Окну назвали страницу. Новое видео (старт сеанса, замена вкладки) получает новый
    /// номер: команды, выданные виджетом при прежнем видео, перестают действовать.
    /// Повтор того же сеанса только обновляет подпись.
    pub(crate) fn note_page(&mut self, label: PageLabel, arrival: PageArrival) {
        if arrival == PageArrival::NewVideo || self.page.is_none() {
            self.lineage = self.lineage.saturating_add(1);
        }
        self.page = Some(PageInfo {
            lineage: self.lineage,
            label,
        });
    }

    /// Исполняет накопленные команды шины через окно и отдаёт просьбы `Raise`/`Quit`.
    pub(crate) fn pump(&mut self, window: &mut dyn DesktopWindowPort) -> DesktopWindowRequests {
        let commands: Vec<DesktopCommand> = self.commands.try_iter().collect();
        if !commands.is_empty() {
            let observation = window.observe_transport();
            for command in commands {
                self.execute(&command, window, &observation);
            }
        }
        let mut requests = DesktopWindowRequests::default();
        for request in self.window_requests.try_iter() {
            match request {
                DesktopWindowRequest::Raise => requests.raise = true,
                DesktopWindowRequest::Quit => requests.quit = true,
            }
        }
        requests
    }

    /// Одна команда: план → исполнение. Отброшенные команды — только в журнал отладки.
    fn execute(
        &self,
        command: &DesktopCommand,
        window: &mut dyn DesktopWindowPort,
        observation: &TransportObservation,
    ) {
        match plan_command(command, self.page.as_ref(), observation) {
            PlannedAction::Player(player_command) => {
                if let Err(error) = window.send_transport_command(player_command) {
                    warn!(%error, "The browser window did not accept an MPRIS command");
                }
            }
            PlannedAction::Ignored(reason) => {
                debug!(
                    ?reason,
                    "An MPRIS command was ignored by the browser window"
                );
            }
        }
    }

    /// Публикует вид окна: полную публикацию — когда изменилось что-то кроме позиции,
    /// иначе только позицию и не чаще [`POSITION_PUBLISH_PERIOD`].
    pub(crate) fn publish(&mut self, observation: &TransportObservation, now: Instant) {
        let next = build_view(self.revision, self.page.as_ref(), observation);
        if differs_beyond_position(&self.last_view, &next) {
            self.revision = self.revision.saturating_add(1);
            let mut committed = next;
            committed.revision = desktop_integration::DesktopSnapshotRevision::new(self.revision);
            self.publish_full(committed, now);
            return;
        }
        let position_due = self
            .last_position_publish
            .is_none_or(|at| now.saturating_duration_since(at) >= POSITION_PUBLISH_PERIOD);
        if position_due && next.position != self.last_view.position {
            self.last_view.position = next.position;
            self.last_position_publish = Some(now);
            if let Some(integration) = &self.integration
                && let Err(error) = integration.publish_position(next.position)
            {
                warn!(%error, "Unable to publish the browser window MPRIS position");
            }
        }
    }

    /// Полная публикация вида.
    fn publish_full(&mut self, view: DesktopSnapshotView, now: Instant) {
        self.last_view = view.clone();
        self.last_position_publish = Some(now);
        if let Some(integration) = &self.integration
            && let Err(error) = integration.publish_snapshot(view)
        {
            warn!(%error, "Unable to publish the browser window MPRIS snapshot");
        }
    }

    /// Останавливает сервер и освобождает имя на шине. Окно закрывается, а имя остаётся
    /// занятым только до конца процесса, поэтому ждём в пределах общего бюджета выхода.
    pub(crate) fn shut_down(mut self, deadline: ShutdownDeadline) {
        if let Some(integration) = self.integration.as_mut() {
            let outcome = integration.shutdown_until(deadline.expires_at());
            debug!(?outcome, "The browser window MPRIS stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use desktop_integration::{
        DesktopCommandRequestId, DesktopControlRevision, DesktopTransportAction,
    };
    use media_core::MediaTime;
    use player_core::PlaybackState;

    use super::*;
    use crate::app_wake::{AppWakeEvent, AppWakeOwner, WakeEmitter};

    /// Считает пробуждения цикла, которые поднял «поток шины».
    #[derive(Default)]
    struct CountingEmitter {
        wakes: AtomicUsize,
    }

    impl WakeEmitter for CountingEmitter {
        fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Окно-подделка: запоминает, какие команды ему дали и сколько раз смотрели плеер.
    #[derive(Default)]
    struct FakeWindow {
        commands: Vec<PlayerCommand>,
        observations: usize,
        reject_commands: bool,
    }

    impl DesktopWindowPort for FakeWindow {
        fn observe_transport(&mut self) -> TransportObservation {
            self.observations += 1;
            observation(PlaybackState::Playing, 30)
        }

        fn send_transport_command(&mut self, command: PlayerCommand) -> Result<(), String> {
            if self.reject_commands {
                return Err("player queue is full".to_owned());
            }
            self.commands.push(command);
            Ok(())
        }
    }

    fn observation(state: PlaybackState, position_secs: u64) -> TransportObservation {
        TransportObservation {
            playback_state: state,
            position: MediaTime::from_secs(position_secs),
            duration: None,
            seekable: false,
            volume: 1.0,
        }
    }

    /// Владелец без настоящей шины: команды кладёт тест через тот же ящик, что и поток шины.
    fn transport_without_bus() -> (WindowDesktopTransport, DesktopMailbox, Arc<CountingEmitter>) {
        let emitter = Arc::new(CountingEmitter::default());
        let wake_port = AppWakePort::new(AppWakeOwner::BrowserSession, emitter.clone());
        let (command_tx, commands) = sync_channel(DESKTOP_COMMAND_CAPACITY);
        let (request_tx, window_requests) = sync_channel(WINDOW_REQUEST_CAPACITY);
        let mailbox = DesktopMailbox {
            commands: command_tx,
            window_requests: request_tx,
            wake_port,
        };
        let transport = WindowDesktopTransport {
            integration: None,
            commands,
            window_requests,
            page: None,
            lineage: 0,
            revision: 0,
            last_view: build_view(0, None, &observation(PlaybackState::Idle, 0)),
            last_position_publish: None,
        };
        (transport, mailbox, emitter)
    }

    fn bus_command(action: DesktopTransportAction, revision: u64) -> DesktopCommand {
        DesktopCommand {
            request_id: DesktopCommandRequestId::new(NonZeroU64::MIN),
            observed_control_revision: Some(DesktopControlRevision::new(revision)),
            action,
        }
    }

    fn page() -> PageLabel {
        PageLabel::new("Cats", "video.example")
    }

    #[test]
    fn a_bus_command_reaches_the_window_and_wakes_the_loop() {
        let (mut transport, mailbox, emitter) = transport_without_bus();
        transport.note_page(page(), PageArrival::NewVideo);
        mailbox
            .send_desktop_command(bus_command(DesktopTransportAction::Pause, 1))
            .expect("mailbox accepts");

        let mut window = FakeWindow::default();
        let requests = transport.pump(&mut window);

        assert_eq!(window.commands, vec![PlayerCommand::Pause]);
        assert_eq!(requests, DesktopWindowRequests::default());
        assert_eq!(emitter.wakes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_command_for_the_previous_video_never_reaches_the_window() {
        let (mut transport, mailbox, _emitter) = transport_without_bus();
        transport.note_page(page(), PageArrival::NewVideo);
        transport.note_page(page(), PageArrival::NewVideo);
        mailbox
            .send_desktop_command(bus_command(DesktopTransportAction::Pause, 1))
            .expect("mailbox accepts");

        let mut window = FakeWindow::default();
        transport.pump(&mut window);

        assert!(window.commands.is_empty());
    }

    #[test]
    fn a_repeat_of_the_same_session_keeps_the_video_number() {
        let (mut transport, mailbox, _emitter) = transport_without_bus();
        transport.note_page(page(), PageArrival::NewVideo);
        transport.note_page(
            PageLabel::new("Cats 2", "video.example"),
            PageArrival::SameSessionAgain,
        );
        mailbox
            .send_desktop_command(bus_command(DesktopTransportAction::Play, 1))
            .expect("mailbox accepts");

        let mut window = FakeWindow::default();
        transport.pump(&mut window);

        assert_eq!(window.commands, vec![PlayerCommand::Play]);
    }

    #[test]
    fn an_idle_pump_does_not_look_at_the_player_and_raise_quit_are_reported() {
        let (mut transport, mailbox, _emitter) = transport_without_bus();
        let mut window = FakeWindow::default();
        assert_eq!(
            transport.pump(&mut window),
            DesktopWindowRequests::default()
        );
        assert_eq!(window.observations, 0);

        mailbox.request_window(DesktopWindowRequest::Raise);
        mailbox.request_window(DesktopWindowRequest::Quit);
        assert_eq!(
            transport.pump(&mut window),
            DesktopWindowRequests {
                raise: true,
                quit: true
            }
        );
    }

    #[test]
    fn a_rejecting_window_does_not_break_the_pump() {
        let (mut transport, mailbox, _emitter) = transport_without_bus();
        transport.note_page(page(), PageArrival::NewVideo);
        for _ in 0..2 {
            mailbox
                .send_desktop_command(bus_command(DesktopTransportAction::Play, 1))
                .expect("mailbox accepts");
        }
        let mut window = FakeWindow {
            reject_commands: true,
            ..FakeWindow::default()
        };
        transport.pump(&mut window);
        assert!(window.commands.is_empty());
        assert_eq!(window.observations, 1);
    }

    #[test]
    fn full_publication_only_when_more_than_the_position_changed() {
        let (mut transport, _mailbox, _emitter) = transport_without_bus();
        transport.note_page(page(), PageArrival::NewVideo);
        let start = Instant::now();

        transport.publish(&observation(PlaybackState::Playing, 1), start);
        let first_revision = transport.revision;
        assert_eq!(first_revision, 1);

        // Только позиция: ревизия не растёт, позиция лежит в виде, но не чаще 4 Гц.
        transport.publish(
            &observation(PlaybackState::Playing, 2),
            start + Duration::from_millis(10),
        );
        assert_eq!(transport.revision, first_revision);
        assert_eq!(transport.last_view.position, MediaTime::from_secs(1));
        transport.publish(
            &observation(PlaybackState::Playing, 3),
            start + Duration::from_millis(400),
        );
        assert_eq!(transport.revision, first_revision);
        assert_eq!(transport.last_view.position, MediaTime::from_secs(3));

        // Пауза — другое состояние: новая ревизия.
        transport.publish(
            &observation(PlaybackState::Paused, 3),
            start + Duration::from_millis(500),
        );
        assert_eq!(transport.revision, first_revision + 1);
    }
}
