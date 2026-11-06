//! Соединение Wayland и вызовы `org_kde_kwin_fake_input`.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

use super::ButtonState;

/// Кому представляется стенд при `authenticate` (видно в журнале kwin).
const APPLICATION_NAME: &str = "fastiplayer-browser-bridge-bench";
/// Причина запроса (kwin показывает её только когда проверки прав включены).
const AUTHENTICATE_REASON: &str = "robot input for the browser extension test stand";
/// Версии интерфейса: `pointer_motion_absolute` с 3, `keyboard_key` с 4.
const MIN_INTERFACE_VERSION: u32 = 4;
const MAX_INTERFACE_VERSION: u32 = 5;

/// Ошибки настоящего ввода.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// Сокета Wayland нет или к нему нельзя подключиться.
    #[error("cannot connect to the Wayland socket {path}: {message}")]
    Connect {
        /// Сокет.
        path: PathBuf,
        /// Текст ошибки.
        message: String,
    },
    /// Композитор не предлагает `org_kde_kwin_fake_input` (нет права или старая версия).
    #[error("the compositor does not offer org_kde_kwin_fake_input v4+: {0}")]
    NoFakeInput(String),
    /// Протокольная ошибка или обрыв соединения при отправке запроса.
    #[error("Wayland protocol failure after {request}: {message}")]
    Protocol {
        /// Какой запрос только что ушёл.
        request: &'static str,
        /// Текст ошибки.
        message: String,
    },
}

/// Состояние обработчика: событий у `fake_input` нет, но очередь требует тип.
struct NoState;

impl Dispatch<WlRegistry, GlobalListContents> for NoState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _contents: &GlobalListContents,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        // Динамические изменения списка глобалов стенду не нужны.
    }
}

impl Dispatch<OrgKdeKwinFakeInput, ()> for NoState {
    fn event(
        _state: &mut Self,
        _proxy: &OrgKdeKwinFakeInput,
        _event: <OrgKdeKwinFakeInput as wayland_client::Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        // У интерфейса нет событий.
    }
}

/// Живое соединение с привязанным `fake_input`.
pub(super) struct FakeInputLink {
    connection: Connection,
    fake_input: OrgKdeKwinFakeInput,
    // Очередь держим живой: её закрытие отвязало бы обработчики прокси.
    _queue: EventQueue<NoState>,
}

impl FakeInputLink {
    /// Подключается по пути сокета (переменные окружения процесса не трогаем),
    /// привязывает `fake_input` и представляется через `authenticate`.
    pub(super) fn connect(socket: &Path) -> Result<Self, InputError> {
        let to_connect_error = |message: String| InputError::Connect {
            path: socket.to_path_buf(),
            message,
        };
        let stream =
            UnixStream::connect(socket).map_err(|error| to_connect_error(error.to_string()))?;
        let connection =
            Connection::from_socket(stream).map_err(|error| to_connect_error(error.to_string()))?;
        let (globals, queue): (GlobalList, EventQueue<NoState>) = registry_queue_init(&connection)
            .map_err(|error| to_connect_error(error.to_string()))?;
        let fake_input: OrgKdeKwinFakeInput = globals
            .bind(
                &queue.handle(),
                MIN_INTERFACE_VERSION..=MAX_INTERFACE_VERSION,
                (),
            )
            .map_err(|error| InputError::NoFakeInput(error.to_string()))?;
        let link = Self {
            connection,
            fake_input,
            _queue: queue,
        };
        link.fake_input
            .authenticate(APPLICATION_NAME.to_owned(), AUTHENTICATE_REASON.to_owned());
        link.round_trip("authenticate")?;
        Ok(link)
    }

    /// Абсолютное перемещение курсора (координаты глобального пространства экрана).
    pub(super) fn pointer_move(&self, x: f64, y: f64) -> Result<(), InputError> {
        self.fake_input.pointer_motion_absolute(x, y);
        self.round_trip("pointer_motion_absolute")
    }

    /// Кнопка мыши.
    pub(super) fn button(&self, code: u32, state: ButtonState) -> Result<(), InputError> {
        self.fake_input.button(code, state.wire_value());
        self.round_trip("button")
    }

    /// Клавиша клавиатуры.
    pub(super) fn key(&self, code: u32, state: ButtonState) -> Result<(), InputError> {
        self.fake_input.keyboard_key(code, state.wire_value());
        self.round_trip("keyboard_key")
    }

    /// Отправляет запросы и ждёт ответа композитора: так протокольная ошибка видна сразу.
    fn round_trip(&self, request: &'static str) -> Result<(), InputError> {
        self.connection
            .roundtrip()
            .map(|_| ())
            .map_err(|error| InputError::Protocol {
                request,
                message: error.to_string(),
            })
    }
}
