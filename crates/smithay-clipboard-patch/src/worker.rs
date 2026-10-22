use std::io::{Error, ErrorKind, Result};
use std::sync::mpsc::Sender;

use sctk::reexports::calloop::channel::Channel;
use sctk::reexports::calloop::timer::{TimeoutAction, Timer};
use sctk::reexports::calloop::{Dispatcher, EventLoop, LoopHandle, RegistrationToken, channel};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::backend::WaylandError;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::{Connection, EventQueue};

use crate::socket_watch::{IDLE_QUEUE_DRAIN_INTERVAL, SocketWatchController, SocketWatchSwitch};
use crate::state::{SelectionTarget, State};

/// Fastiplayer: источник сокета регистрируется через `Dispatcher`, чтобы его
/// очередь событий оставалась доступной командам и таймеру, пока сам опрос
/// сокета выключен.
type QueueSource = Dispatcher<'static, WaylandSource<State>, State>;

/// Fastiplayer: сколько roundtrip-ов нужно при старте, чтобы состояние seat-ов
/// устоялось. Первый приносит возможности seat-ов (в ответ создаются клавиатура,
/// указатель и data device), второй — начальные события этих объектов (фокус,
/// текущее выделение).
const INITIAL_SETUP_ROUNDTRIPS: usize = 2;

/// Spawn a clipboard worker, which dispatches its own `EventQueue` and handles
/// clipboard requests.
pub fn spawn(
    name: String,
    display: Connection,
    rx_chan: Channel<Command>,
    worker_replier: Sender<Result<String>>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            worker_impl(display, rx_chan, worker_replier);
        })
        .ok()
}

/// Clipboard worker thread command.
#[derive(Eq, PartialEq)]
pub enum Command {
    /// Store data to a clipboard.
    Store(String),
    /// Store data to a primary selection.
    StorePrimary(String),
    /// Load data from a clipboard.
    Load,
    /// Load primary selection.
    LoadPrimary,
    /// Shutdown the worker.
    Exit,
}

/// Handle clipboard requests.
///
/// Fastiplayer: в отличие от upstream, общий Wayland-сокет опрашивается только
/// пока приложение владеет выделением (см. `socket_watch`). В остальное время
/// поток спит и разбирает свою очередь перед каждой командой и по таймеру.
fn worker_impl(
    connection: Connection,
    rx_chan: Channel<Command>,
    reply_tx: Sender<Result<String>>,
) {
    let (globals, event_queue) = match registry_queue_init(&connection) {
        Ok(data) => data,
        Err(_) => return,
    };

    // Ранний выход потока закрывает канал ответов, и `Clipboard::load` вернёт
    // ошибку «clipboard is dead» — так upstream ведёт себя при любом сбое.
    let Ok(mut event_loop) = EventLoop::<State>::try_new() else {
        return;
    };
    let loop_handle = event_loop.handle();

    let mut state = match State::new(&globals, &event_queue.handle(), loop_handle.clone(), reply_tx)
    {
        Some(state) => state,
        None => return,
    };

    let queue_source: QueueSource =
        Dispatcher::new(WaylandSource::new(connection, event_queue), |_, queue, state| {
            queue.dispatch_pending(state)
        });
    let Ok(socket_token) = loop_handle.register_dispatcher(queue_source.clone()) else {
        return;
    };
    let mut socket_switch = CalloopSocketSwitch { loop_handle: loop_handle.clone(), socket_token };
    // Источник зарегистрирован, но до первого копирования сокет не опрашивается.
    if socket_switch.stop_listening().is_err() {
        return;
    }
    let mut socket_watch = SocketWatchController::sleeping();

    for _ in 0..INITIAL_SETUP_ROUNDTRIPS {
        if sync_with_compositor(&queue_source, &mut state).is_err() {
            return;
        }
    }

    let command_queue = queue_source.clone();
    let command_source = loop_handle.insert_source(rx_chan, move |event, _, state| {
        if let channel::Event::Msg(command) = event {
            handle_command(command, &command_queue, state);
        }
    });
    if command_source.is_err() {
        return;
    }

    let drain_queue = queue_source.clone();
    let drain_timer = Timer::from_duration(IDLE_QUEUE_DRAIN_INTERVAL);
    let drain_source = loop_handle.insert_source(drain_timer, move |_, _, state| {
        // Ошибка здесь — разорванное соединение: дальше работать нечем.
        if drain_pending_events(&drain_queue, state).is_err() {
            state.exit = true;
        }
        TimeoutAction::ToDuration(IDLE_QUEUE_DRAIN_INTERVAL)
    });
    if drain_source.is_err() {
        return;
    }

    loop {
        if event_loop.dispatch(None, &mut state).is_err() || state.exit {
            break;
        }
        // Режим опроса сверяется вне колбэков: calloop не переключает источник,
        // пока тот обрабатывает события. Владение меняется в `store` (команда)
        // и в `cancelled` (событие сокета) — оба уже обработаны к этому месту.
        if socket_watch.sync(state.selection_ownership(), &mut socket_switch).is_err() {
            break;
        }
    }
}

/// Fastiplayer: выполняет одну команду, предварительно догнав композитор.
fn handle_command(command: Command, queue_source: &QueueSource, state: &mut State) {
    if command == Command::Exit {
        state.exit = true;
        return;
    }

    // Пока сокет не опрашивался, свежие события (фокус, serial нажатия Ctrl+C/V,
    // текущее выделение) лежат в очереди неразобранными или ещё в пути.
    // Roundtrip гарантирует, что всё отправленное композитором до этой команды
    // разобрано — `load` видит актуальное выделение, `store` берёт свежий serial.
    if let Err(err) = sync_with_compositor(queue_source, state) {
        if matches!(command, Command::Load | Command::LoadPrimary) {
            let _ = state.reply_tx.send(Err(err));
        }
        // Roundtrip падает только на разорванном соединении.
        state.exit = true;
        return;
    }

    match command {
        Command::StorePrimary(contents) => {
            state.store_selection(SelectionTarget::Primary, contents);
        },
        Command::Store(contents) => {
            state.store_selection(SelectionTarget::Clipboard, contents);
        },
        Command::Load if state.data_device_manager_state.is_some() => {
            if let Err(err) = state.load_selection(SelectionTarget::Clipboard) {
                let _ = state.reply_tx.send(Err(err));
            }
        },
        Command::LoadPrimary if state.data_device_manager_state.is_some() => {
            if let Err(err) = state.load_selection(SelectionTarget::Primary) {
                let _ = state.reply_tx.send(Err(err));
            }
        },
        Command::Load | Command::LoadPrimary => {
            let _ = state.reply_tx.send(Err(Error::other("requested selection is not supported")));
        },
        Command::Exit => state.exit = true,
    }

    // Запросы команды (`receive`, `set_selection`) должны уйти сразу: в спящем
    // режиме источник сокета не делает flush перед сном, как в upstream.
    let mut source = queue_source.as_source_mut();
    if flush_requests(source.queue()).is_err() {
        state.exit = true;
    }
}

/// Fastiplayer: roundtrip своей очереди — разбирает всё, что композитор прислал
/// до этого момента, и отправляет накопленные запросы.
fn sync_with_compositor(queue_source: &QueueSource, state: &mut State) -> Result<()> {
    let mut source = queue_source.as_source_mut();
    source.queue().roundtrip(state).map(drop).map_err(Error::other)
}

/// Fastiplayer: разбирает события, которые поток окна уже прочитал из сокета в
/// нашу очередь, не читая сокет самому. Ограничивает рост очереди во сне.
fn drain_pending_events(queue_source: &QueueSource, state: &mut State) -> Result<()> {
    let mut source = queue_source.as_source_mut();
    let queue = source.queue();
    queue.dispatch_pending(state).map_err(Error::other)?;
    flush_requests(queue)
}

/// Fastiplayer: отправляет запросы очереди композитору.
///
/// `WouldBlock` (композитор не успевает читать) не ошибка — как и в
/// calloop-wayland-source, запросы уйдут при следующем flush.
fn flush_requests(queue: &EventQueue<State>) -> Result<()> {
    match queue.flush() {
        Ok(()) => Ok(()),
        Err(WaylandError::Io(err)) if err.kind() == ErrorKind::WouldBlock => Ok(()),
        Err(err) => Err(Error::other(err)),
    }
}

/// Fastiplayer: переключение опроса сокета через calloop.
///
/// `disable` убирает и fd из epoll, и `before_sleep`-хук источника, поэтому в
/// спящем режиме поток не держит `prepare_read` (иначе он блокировал бы чтение
/// сокета потоком окна).
struct CalloopSocketSwitch {
    loop_handle: LoopHandle<'static, State>,
    socket_token: RegistrationToken,
}

impl SocketWatchSwitch for CalloopSocketSwitch {
    fn start_listening(&mut self) -> Result<()> {
        self.loop_handle.enable(&self.socket_token).map_err(Error::other)
    }

    fn stop_listening(&mut self) -> Result<()> {
        self.loop_handle.disable(&self.socket_token).map_err(Error::other)
    }
}
