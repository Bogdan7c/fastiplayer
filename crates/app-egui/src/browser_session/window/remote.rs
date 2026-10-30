//! «Пульт» окна браузерного режима: как медиа-часть сеанса видит плеер окна.
//!
//! Схема владения (04a). Плеер один — в `AppState` окна (решение 120), а медиа-часть
//! сеанса [`SessionMedia`](super::super::session_media::SessionMedia) держит плеер как
//! `Box<dyn MediaPlayback>`. `AppState` при этом принадлежит экземпляру окна: ему его
//! нужно и для кадра, и для ввода. Делить `AppState` между двумя владельцами нельзя,
//! поэтому между ними стоит «стол» ([`PlaybackDesk`]):
//!
//! ```text
//!  SessionMedia ── load/play/pause/seek/set_rate ──► WindowRemote ─► стол: очередь команд
//!       ▲                                                              │
//!       └──────────── pump(): последнее наблюдение ◄── стол ◄──────────┤
//!                                                                      ▼
//!  цикл окна: водитель (`driver`) забирает команды → AppState; кадр/события/снимок
//!             → учёт загрузок и журнал → публикует наблюдение на стол
//! ```
//!
//! Всё это живёт в одном потоке (цикл winit), поэтому стол — `Rc<RefCell<…>>`: один
//! маленький общий ящик, заимствования которого никогда не переживают вызов метода
//! стола (паники «уже занято» быть не может: методы стола не зовут друг друга и
//! не отдают ссылок наружу).
//!
//! Квитанции загрузок выдаёт пульт сразу (как безоконный плеер): медиа-часть получает
//! её синхронно, а водитель окна связывает с ней установку, когда исполнит команду.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use player_core::{PlaybackRate, PlayerSnapshot, PreparedMedia};

use super::super::playback::load_ledger::LoadLedger;
use super::super::playback::{
    BrowserPlaybackCommandError, LoadStart, MediaLoadTicket, MediaLoadTicketIssuer,
    PlaybackObservation,
};
use super::super::session_media::MediaPlayback;

/// Сколько команд ждёт окна, прежде чем пульт начнёт отказывать.
///
/// Медиа-часть шлёт единицы команд за шаг, а окно исполняет их каждый проход цикла;
/// 64 — это много проходов зависшего окна. Переполнение — не тихая потеря: пульт
/// возвращает ошибку, медиа-часть её пишет в журнал.
pub(super) const REMOTE_COMMAND_CAPACITY: usize = 64;

/// Почему окно не приняло команду пульта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WindowCommandRefusal {
    /// Очередь команд полна: окно их не исполняет.
    #[error("the browser window command queue is full")]
    QueueFull,
    /// Окно уже закрыто: исполнять некому.
    #[error("the browser window is closed")]
    WindowClosed,
}

/// Загрузка, ждущая исполнения окном.
pub(super) struct LoadOrder {
    /// Квитанция, уже выданная медиа-части.
    pub(super) ticket: MediaLoadTicket,
    /// Медиа поколения потока.
    pub(super) prepared: PreparedMedia,
    /// Играть сразу или встать на паузу.
    pub(super) start: LoadStart,
}

/// Команда пульта, ждущая исполнения окном (в порядке медиа-части).
pub(super) enum RemoteCommand {
    /// Загрузить новое медиа (в куче: медиа велико, а команды транспорта малы).
    Load(Box<LoadOrder>),
    /// Играть.
    Play,
    /// Пауза.
    Pause,
    /// Скорость.
    SetRate(PlaybackRate),
    /// Точная перемотка.
    Seek(Duration),
}

impl RemoteCommand {
    /// Метка для журнала (без содержимого медиа).
    pub(super) const fn log_label(&self) -> &'static str {
        match self {
            Self::Load(_) => "load",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::SetRate(_) => "set_rate",
            Self::Seek(_) => "seek",
        }
    }
}

/// Содержимое стола.
struct DeskState {
    commands: VecDeque<RemoteCommand>,
    observation: PlaybackObservation,
    window_closed: bool,
}

/// Стол между пультом и окном (см. описание модуля).
#[derive(Clone)]
pub(super) struct PlaybackDesk {
    state: Rc<RefCell<DeskState>>,
}

impl PlaybackDesk {
    /// Пустой стол: команд нет, наблюдение «плеер пуст» (медиа ещё не загружали).
    pub(super) fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(DeskState {
                commands: VecDeque::new(),
                observation: LoadLedger::default().observation(&PlayerSnapshot::default()),
                window_closed: false,
            })),
        }
    }

    /// Кладёт команду в очередь окна.
    ///
    /// Новая загрузка выбрасывает ещё не исполненные загрузки: их поколения уже
    /// заменены, и ставить их в плеер незачем («новее заменяет» — 03e).
    ///
    /// ИНВАРИАНТ: команды транспорта, поставленные после выброшенной загрузки, остаются
    /// на месте (перед новой загрузкой) — и это верно. Медиа-часть (`SessionMedia`)
    /// никогда не адресует команду медиа, которое ещё не показано: перемотка до показа
    /// ждёт в `seek_when_shown`, а «играть/пауза» и скорость, отданные во время
    /// установки, по договору достаются ПРЕЖНЕМУ медиа (или никому) — ровно так же, как
    /// у безоконного плеера, где они сразу уходят в плеер до коммита установки. Новое
    /// поколение получает намерение страницы иначе: `LoadStart` берётся из «играть/пауза»
    /// в момент загрузки, а при первом показе `install_reconcile` один раз приводит
    /// скорость, «играть/пауза» и отложенную перемотку к странице. Выбросить эти
    /// команды вместе с загрузкой было бы ошибкой: пауза для прежнего видео (новый
    /// сеанс, зритель) должна до него дойти.
    fn submit(&self, command: RemoteCommand) -> Result<(), WindowCommandRefusal> {
        let mut state = self.state.borrow_mut();
        if state.window_closed {
            return Err(WindowCommandRefusal::WindowClosed);
        }
        if matches!(command, RemoteCommand::Load(_)) {
            state
                .commands
                .retain(|queued| !matches!(queued, RemoteCommand::Load(_)));
        }
        if state.commands.len() >= REMOTE_COMMAND_CAPACITY {
            return Err(WindowCommandRefusal::QueueFull);
        }
        state.commands.push_back(command);
        Ok(())
    }

    /// Окно забирает все ждущие команды в порядке подачи.
    pub(super) fn take_commands(&self) -> Vec<RemoteCommand> {
        self.state.borrow_mut().commands.drain(..).collect()
    }

    /// Окно публикует свежее наблюдение.
    pub(super) fn publish(&self, observation: PlaybackObservation) {
        self.state.borrow_mut().observation = observation;
    }

    /// Последнее опубликованное наблюдение.
    fn latest_observation(&self) -> PlaybackObservation {
        self.state.borrow().observation.clone()
    }

    /// Окна больше нет: ждущие команды выброшены, новые отвергаются.
    pub(super) fn close_window(&self) {
        let mut state = self.state.borrow_mut();
        state.window_closed = true;
        state.commands.clear();
    }
}

/// Пульт: плеер окна глазами медиа-части.
pub(super) struct WindowRemote {
    desk: PlaybackDesk,
    tickets: MediaLoadTicketIssuer,
}

impl WindowRemote {
    /// Пульт над этим столом.
    pub(super) fn new(desk: PlaybackDesk) -> Self {
        Self {
            desk,
            tickets: MediaLoadTicketIssuer::default(),
        }
    }
}

impl MediaPlayback for WindowRemote {
    fn load(
        &mut self,
        prepared: PreparedMedia,
        start: LoadStart,
    ) -> Result<MediaLoadTicket, BrowserPlaybackCommandError> {
        let ticket = self.tickets.issue();
        self.desk.submit(RemoteCommand::Load(Box::new(LoadOrder {
            ticket,
            prepared,
            start,
        })))?;
        Ok(ticket)
    }

    fn play(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self.desk.submit(RemoteCommand::Play)?)
    }

    fn pause(&mut self) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self.desk.submit(RemoteCommand::Pause)?)
    }

    fn set_rate(&mut self, rate: PlaybackRate) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self.desk.submit(RemoteCommand::SetRate(rate))?)
    }

    fn seek(&mut self, target: Duration) -> Result<(), BrowserPlaybackCommandError> {
        Ok(self.desk.submit(RemoteCommand::Seek(target))?)
    }

    fn pump(&mut self, _now: Instant) -> PlaybackObservation {
        self.desk.latest_observation()
    }

    /// Итог журнала и остановку плеера делает окно после остановки ядра процесса
    /// (`window::app`): плеер принадлежит ему, а не пульту.
    fn finish(self: Box<Self>) {}
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
