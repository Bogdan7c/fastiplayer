//! Транспорт «входа» браузерного режима: посредник браузера → процесс плеера.
//!
//! # Зачем
//!
//! Родной посредник расширения (native messaging host) не знает, запущен ли плеер.
//! Он звонит по сессионной шине D-Bus на имя [`BROWSER_SESSION_BUS_NAME`]; если
//! службы ещё нет, шина сама запускает `fastiplayer --browser-session` (D-Bus
//! activation, `.service`-файл из [`browser_session_activation_service_file`]).
//! Через D-Bus едет только «вход» (JSON-запрос и JSON-ответ с портом и пропуском);
//! сами данные потом идут напрямую по WebSocket.
//!
//! # Граница
//!
//! Модуль владеет только транспортом и его безопасностью: допуск по uid, лимит
//! размера, ожидание ответа с таймаутом, типизированные отказы. Он **никогда не
//! разбирает JSON**: сообщение и ответ для него непрозрачные строки, смысл придаёт
//! приложение. Типов приложения здесь нет, поэтому тот же клиент
//! ([`BrowserEntryClient`]) годится для посредника.
//!
//! # Платформы
//!
//! Реализован только Linux (D-Bus). На остальных платформах функции возвращают
//! `UnsupportedPlatform`; генератор `.service`-файла — чистая функция и доступен везде.

mod activation_service_file;
#[cfg(target_os = "linux")]
mod linux;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

pub use activation_service_file::{
    ActivationServiceFileError, browser_session_activation_service_file,
};

/// Имя службы входа на сессионной шине.
pub const BROWSER_SESSION_BUS_NAME: &str = "io.github.Bogdan7c.Fastiplayer.BrowserSession";
/// Объект службы входа.
pub const BROWSER_SESSION_OBJECT_PATH: &str = "/io/github/Bogdan7c/Fastiplayer/BrowserSession";
/// Интерфейс службы входа; метод `Entry(s) -> s`.
pub const BROWSER_SESSION_INTERFACE: &str = "io.github.Bogdan7c.Fastiplayer.BrowserSession1";
/// Аргумент командной строки, по которому процесс понимает, что он браузерный режим.
/// Попадает в `Exec=` `.service`-файла.
pub const BROWSER_SESSION_ARGUMENT: &str = "--browser-session";

/// Максимальный размер сообщения входа и ответа на него, в байтах.
///
/// Обязан совпадать с `browser_bridge_protocol::limits::MAX_JSON_MESSAGE_BYTES`.
/// Этот crate от протокольного crate-а намеренно не зависит (он не разбирает JSON),
/// поэтому равенство закрепляет тест в приложении.
pub const MAX_BROWSER_ENTRY_MESSAGE_BYTES: usize = 128 * 1024;

/// Кто звонит: уникальное имя соединения на шине. Непрозрачный идентификатор:
/// приложение может сравнивать звонящих, но не должно предполагать формат.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrowserEntryCaller(String);

impl BrowserEntryCaller {
    /// Идентификатор звонящего (для логов и сравнения).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Создаёт идентификатор из имени отправителя (транспорт, тесты приложения).
    pub fn from_sender_name(sender_name: String) -> Self {
        Self(sender_name)
    }
}

/// Запрос входа: кто звонит и непрозрачное сообщение (JSON).
#[derive(Clone, PartialEq, Eq)]
pub struct BrowserEntryRequest {
    /// Звонящий.
    pub caller: BrowserEntryCaller,
    /// Сообщение, уже проверенное по размеру; содержимое транспорт не читает.
    pub message: String,
}

impl fmt::Debug for BrowserEntryRequest {
    /// В сообщении может быть одноразовый пропуск, поэтому его текст в `Debug` не попадает.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserEntryRequest")
            .field("caller", &self.caller)
            .field("message_bytes", &self.message.len())
            .finish()
    }
}

/// Одноразовый слот ответа звонящему.
///
/// Уничтожение слота без [`Self::send`] означает «приложение завершается»:
/// звонящий получит `ShuttingDown`, а не вечное ожидание.
pub struct BrowserEntryReply {
    on_reply: Box<dyn FnOnce(String) + Send>,
}

impl BrowserEntryReply {
    /// Слот, который при ответе вызывает `on_reply` (транспорт, тесты приложения).
    ///
    /// Если слот уничтожат без ответа, `on_reply` не вызывается: транспорт сам
    /// замечает закрытый канал и отвечает звонящему `ShuttingDown`.
    pub fn from_callback(on_reply: impl FnOnce(String) + Send + 'static) -> Self {
        Self {
            on_reply: Box::new(on_reply),
        }
    }

    /// Отправляет ответ звонящему. Слот расходуется.
    ///
    /// Ответ длиннее [`MAX_BROWSER_ENTRY_MESSAGE_BYTES`] звонящему не уйдёт
    /// (он получит `NotResponding`, ошибка пишется в лог службы).
    pub fn send(self, reply: String) {
        (self.on_reply)(reply);
    }
}

impl fmt::Debug for BrowserEntryReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserEntryReply")
            .finish_non_exhaustive()
    }
}

/// Запрос вместе с обещанием ответить звонящему.
#[derive(Debug)]
pub struct BrowserEntryDelivery {
    request: BrowserEntryRequest,
    reply: BrowserEntryReply,
}

impl BrowserEntryDelivery {
    /// Связывает запрос со слотом ответа (транспорт, тесты приложения).
    pub fn new(request: BrowserEntryRequest, reply: BrowserEntryReply) -> Self {
        Self { request, reply }
    }

    /// Разбирает доставку на запрос и слот ответа.
    pub fn into_parts(self) -> (BrowserEntryRequest, BrowserEntryReply) {
        (self.request, self.reply)
    }
}

/// Почему приложение не смогло принять доставку.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BrowserEntrySinkError {
    /// Ящик переполнен: главный цикл давно не разбирал запросы.
    #[error("mailbox of browser entry requests is full")]
    Full,
    /// Приложение завершается и запросы больше не принимает.
    #[error("Application no longer accepts browser entry requests")]
    Closed,
}

/// Приёмник запросов входа на стороне приложения.
///
/// Вызывается из потока транспорта; реализация обязана быть неблокирующей
/// (положить в ограниченный ящик и разбудить главный цикл).
pub trait BrowserEntrySink: Send + Sync + 'static {
    /// Передаёт доставку приложению. При ошибке слот ответа уничтожается.
    fn deliver(&self, delivery: BrowserEntryDelivery) -> Result<(), BrowserEntrySinkError>;
}

/// Настройки службы входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserEntryServiceConfig {
    /// Сколько ждать ответа приложения, прежде чем ответить звонящему `NotResponding`.
    pub reply_timeout: Duration,
}

impl Default for BrowserEntryServiceConfig {
    /// 5 с: вход только готовит сеанс (порт и пропуск) и не ждёт ни сети, ни диска;
    /// дольше значит, что главный цикл завис. Меньше звонящего `call_timeout` (8 с).
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(5),
        }
    }
}

/// Работающая служба входа. Остановка — [`Self::shutdown`] или `Drop`.
pub struct BrowserEntryService {
    #[cfg(target_os = "linux")]
    backend: linux::ServiceBackend,
}

impl fmt::Debug for BrowserEntryService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserEntryService")
            .finish_non_exhaustive()
    }
}

impl BrowserEntryService {
    /// Освобождает имя на шине и закрывает соединение. Звонящие, ждущие ответа,
    /// получат `ShuttingDown`.
    pub fn shutdown(self) {
        #[cfg(target_os = "linux")]
        self.backend.shutdown();
    }
}

/// Почему служба входа не запустилась.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrowserEntryServiceError {
    /// Нет сессионной шины.
    #[error("D-Bus session bus not available: {0}")]
    SessionBusUnavailable(String),
    /// Имя службы уже занято (браузерный режим уже запущен).
    #[error("The name of the browser session service on the bus is already taken")]
    BusNameUnavailable,
    /// Прочая ошибка транспорта.
    #[error("Browser entry service transport error: {0}")]
    Transport(String),
    /// На этой платформе транспорт не реализован.
    #[error("Browser entry transport is not supported on this platform.")]
    UnsupportedPlatform,
}

/// Запускает службу входа на сессионной шине.
///
/// Имя запрашивается без очереди и без замены: второй браузерный процесс получит
/// [`BrowserEntryServiceError::BusNameUnavailable`] и должен выйти.
pub fn start_browser_entry_service(
    config: BrowserEntryServiceConfig,
    sink: Arc<dyn BrowserEntrySink>,
) -> Result<BrowserEntryService, BrowserEntryServiceError> {
    #[cfg(target_os = "linux")]
    {
        linux::start_on_session_bus(config, sink).map(|backend| BrowserEntryService { backend })
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, sink);
        Err(BrowserEntryServiceError::UnsupportedPlatform)
    }
}

/// Настройки клиента входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserEntryClientConfig {
    /// Сколько ждать, пока шина запустит службу (D-Bus activation).
    pub activation_timeout: Duration,
    /// Сколько ждать ответа на сам `Entry`. Должно быть больше `reply_timeout`
    /// службы, чтобы получить её осмысленный ответ.
    pub call_timeout: Duration,
}

impl Default for BrowserEntryClientConfig {
    /// Запуск процесса плеера без окна — доли секунды, но на холодном диске и
    /// нагруженной машине бывает секунды: 15 с (в пределах 25 с самой шины).
    /// Вызов: 8 с, больше 5 с ожидания службы.
    fn default() -> Self {
        Self {
            activation_timeout: Duration::from_secs(15),
            call_timeout: Duration::from_secs(8),
        }
    }
}

/// Почему вход не удался.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrowserEntryCallError {
    /// Нет сессионной шины.
    #[error("D-Bus session bus not available: {0}")]
    SessionBusUnavailable(String),
    /// Службы нет и запустить её нечем: `.service`-файл не установлен.
    #[error("Browser mode is not installed (no D-Bus activation file)")]
    NotInstalled,
    /// Шина попыталась запустить службу, но она не поднялась.
    #[error("Failed to start the browser mode: {detail}")]
    ActivationFailed {
        /// Техническая причина для лога.
        detail: String,
    },
    /// Имя службы на шине принадлежит процессу другого пользователя; ничего не отправлено.
    #[error("The name of the browser session service belongs to another user")]
    ForeignOwner,
    /// Служба не ответила вовремя.
    #[error("The browser session does not respond")]
    NotResponding,
    /// Служба завершается и вход не приняла.
    #[error("The browser session is shutting down")]
    ShuttingDown,
    /// Служба отклонила запрос (слишком большой, чужой uid).
    #[error("The browser session rejected the request: {detail}")]
    Rejected {
        /// Техническая причина для лога.
        detail: String,
    },
    /// Прочая ошибка транспорта.
    #[error("Browser entry transport error: {0}")]
    Transport(String),
    /// На этой платформе транспорт не реализован.
    #[error("Browser entry transport is not supported on this platform.")]
    UnsupportedPlatform,
}

/// Какую шину использовать клиенту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserEntryBus {
    /// Сессионная шина пользователя (обычная работа посредника).
    Session,
    /// Шина по явному адресу. Нужна тестам других crate-ов и отладке: сквозной тест
    /// поднимает частный `dbus-daemon` и должен направить на него и клиента, и
    /// запущенный шиной процесс (он получает адрес от самой шины), не трогая сеанс
    /// пользователя.
    Address(String),
}

/// Клиент входа с одним долгоживущим соединением.
///
/// Приложение запоминает `hello` **по звонящему**, то есть по уникальному имени
/// соединения на шине. Поэтому `hello` и следующий за ним `open_session` обязаны идти
/// по одному и тому же соединению: клиент держит его от [`Self::connect`] до `Drop`.
///
/// Если служба исчезла между звонками, шина может сама запустить новый процесс
/// (activation). Он ничего не знает о прежнем `hello`, поэтому ответ на
/// `open_session` будет протокольным отказом `hello_required`: звонящий должен
/// начать с `hello` заново. Транспорт этого не скрывает и не повторяет за звонящего.
pub struct BrowserEntryClient {
    #[cfg(target_os = "linux")]
    backend: linux::ClientBackend,
}

impl fmt::Debug for BrowserEntryClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserEntryClient")
            .finish_non_exhaustive()
    }
}

impl BrowserEntryClient {
    /// Подключается к шине, будит браузерный режим (D-Bus activation, не дольше
    /// `activation_timeout`) и сверяет uid владельца службы со своим
    /// ([`BrowserEntryCallError::ForeignOwner`], ничего не отправлено). Блокирующий вызов.
    pub fn connect(
        bus: BrowserEntryBus,
        config: BrowserEntryClientConfig,
    ) -> Result<Self, BrowserEntryCallError> {
        #[cfg(target_os = "linux")]
        {
            linux::connect(bus, config).map(|backend| Self { backend })
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (bus, config);
            Err(BrowserEntryCallError::UnsupportedPlatform)
        }
    }

    /// Отправляет сообщение входа по тому же соединению и возвращает ответ.
    ///
    /// Сообщение длиннее [`MAX_BROWSER_ENTRY_MESSAGE_BYTES`] отклоняется локально
    /// ([`BrowserEntryCallError::Rejected`]) до обращения к шине.
    pub fn call(&self, message: &str) -> Result<String, BrowserEntryCallError> {
        #[cfg(target_os = "linux")]
        {
            self.backend.call(message)
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = message;
            Err(BrowserEntryCallError::UnsupportedPlatform)
        }
    }
}
