//! Передача запроса «открой эти файлы / подними окно» в уже запущенный экземпляр.
//!
//! # Зачем
//!
//! Второй запуск Fastiplayer («Открыть с помощью» в файловом менеджере при уже
//! открытом плеере) не может стать вторым плеером: единственный экземпляр
//! гарантирует process lease приложения. Вместо выхода с ошибкой второй процесс
//! пересылает свой запрос первому и завершается.
//!
//! # Граница
//!
//! Модуль владеет только **транспортом** и его безопасностью:
//! - нейтральные типы запроса ([`ForwardedInstanceRequest`]) без знания ОС;
//! - проверка запроса до передачи приложению ([`ForwardedRequestRejection`]:
//!   лимиты размера, только URI со схемой, только «открыть» и «активировать»);
//! - служба приёма ([`start_instance_forwarding_service`]) и клиент отправки
//!   ([`forward_to_running_instance`]).
//!
//! Что означают URI (локальный файл, ссылка), куда их открыть и как поднять окно,
//! решает приложение. Модуль не раскодирует `file://` и не трогает очередь.
//!
//! # Linux
//!
//! Транспорт — стандартный интерфейс `org.freedesktop.Application` (методы `Open`,
//! `Activate`) на сессионной шине D-Bus под именем [`FASTIPLAYER_APPLICATION_ID`].
//! Сессионная шина по устройству доступна только пользователю, запустившему сеанс;
//! поверх этого служба явно сверяет uid отправителя со своим и отклоняет чужие
//! запросы. На других платформах функции возвращают `UnsupportedPlatform`
//! (на macOS открытие файлов доставляет сама ОС, на Windows нужен свой транспорт).
//!
//! # Подтверждение
//!
//! Служба отвечает отправителю только после того, как UI-поток приложения забрал
//! запрос ([`ForwardedRequestAcknowledgement::acknowledge`]). Поэтому зависший первый
//! экземпляр виден второму как «не отвечает», а не как ложный успех.

mod admission;
#[cfg(target_os = "linux")]
mod linux;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

pub use admission::{
    ForwardedRequestRejection, MAX_ACTIVATION_TOKEN_BYTES, MAX_FORWARDED_URI_BYTES,
    MAX_FORWARDED_URIS, MAX_TOTAL_FORWARDED_URI_BYTES,
};

/// Постоянный идентификатор приложения (решение владельца, сессия 13).
///
/// Это имя на сессионной шине D-Bus; в будущем — имя `.desktop`-файла и Flatpak.
/// Менять нельзя без миграции: уже запущенный старый экземпляр перестанет
/// принимать запросы от нового.
pub const FASTIPLAYER_APPLICATION_ID: &str = "io.github.Bogdan7c.Fastiplayer";

/// Один URI из запроса: `file:///…` или ссылка (`https://…`).
///
/// Значение уже прошло проверку ([`ForwardedRequestRejection`]): непустое, со
/// схемой, без управляющих символов, в пределах лимита. Ссылка может содержать
/// секреты (токены в query), поэтому `Debug` её не показывает.
#[derive(Clone, PartialEq, Eq)]
pub struct ForwardedUri(String);

impl ForwardedUri {
    /// Текст URI для разбора приложением.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Забирает текст URI без копирования.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for ForwardedUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForwardedUri")
            .field("bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Билет активации окна, выданный рабочим столом второму процессу.
///
/// На Wayland это `XDG_ACTIVATION_TOKEN`, на X11 — `DESKTOP_STARTUP_ID`. Первый
/// экземпляр предъявляет билет композитору, чтобы тот разрешил поднять окно (защита
/// от кражи фокуса). Билет одноразовый и короткоживущий, но в лог не пишется.
#[derive(Clone, PartialEq, Eq)]
pub struct WindowActivationToken(String);

impl WindowActivationToken {
    /// Проверяет значение из окружения или из запроса.
    ///
    /// `None` — значение пустое, слишком длинное или содержит не печатные ASCII
    /// символы: такой билет композитор всё равно не примет.
    pub fn from_raw(raw_token: String) -> Option<Self> {
        admission::is_valid_activation_token(&raw_token).then_some(Self(raw_token))
    }

    /// Текст билета для передачи композитору.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Забирает текст билета без копирования.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for WindowActivationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WindowActivationToken")
            .finish_non_exhaustive()
    }
}

/// Что просит второй экземпляр. Других действий протокол не исполняет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardedInstanceAction {
    /// Только поднять окно (второй запуск без файлов).
    Activate,
    /// Открыть эти URI в порядке запроса и поднять окно.
    Open(Vec<ForwardedUri>),
}

/// Полный запрос второго экземпляра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedInstanceRequest {
    action: ForwardedInstanceAction,
    activation_token: Option<WindowActivationToken>,
}

impl ForwardedInstanceRequest {
    /// Запрос «только поднять окно».
    pub fn activate(activation_token: Option<WindowActivationToken>) -> Self {
        Self {
            action: ForwardedInstanceAction::Activate,
            activation_token,
        }
    }

    /// Запрос «открыть URI». Проверяется теми же правилами, что и на приёме:
    /// отправитель узнаёт о слишком большом запросе сразу, без похода по шине.
    ///
    /// Пустой список означает «только поднять окно».
    pub fn open(
        uris: Vec<String>,
        activation_token: Option<WindowActivationToken>,
    ) -> Result<Self, ForwardedRequestRejection> {
        let action = admission::admit_open_uris(uris)?;
        Ok(Self {
            action,
            activation_token,
        })
    }

    /// Действие запроса.
    pub fn action(&self) -> &ForwardedInstanceAction {
        &self.action
    }

    /// Билет активации окна, если рабочий стол его выдал.
    pub fn activation_token(&self) -> Option<&WindowActivationToken> {
        self.activation_token.as_ref()
    }

    /// Разбирает запрос на части для исполнителя.
    pub fn into_parts(self) -> (ForwardedInstanceAction, Option<WindowActivationToken>) {
        (self.action, self.activation_token)
    }
}

/// Обещание ответить отправителю «запрос принят».
///
/// Вызывается UI-потоком приложения, когда он забрал запрос. Если подтверждение
/// уничтожено без вызова [`Self::acknowledge`] (приложение завершается), отправитель
/// получает ответ «экземпляр завершается», а не успех.
pub struct ForwardedRequestAcknowledgement {
    on_acknowledged: Box<dyn FnOnce() + Send>,
}

impl ForwardedRequestAcknowledgement {
    /// Подтверждение с произвольным действием: его используют транспорт и тесты.
    pub fn from_callback(on_acknowledged: impl FnOnce() + Send + 'static) -> Self {
        Self {
            on_acknowledged: Box::new(on_acknowledged),
        }
    }

    /// Сообщает отправителю, что UI-поток принял запрос.
    pub fn acknowledge(self) {
        (self.on_acknowledged)();
    }
}

impl fmt::Debug for ForwardedRequestAcknowledgement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForwardedRequestAcknowledgement")
            .finish_non_exhaustive()
    }
}

/// Проверенный запрос вместе с обещанием ответить отправителю.
#[derive(Debug)]
pub struct ForwardedRequestDelivery {
    request: ForwardedInstanceRequest,
    acknowledgement: ForwardedRequestAcknowledgement,
}

impl ForwardedRequestDelivery {
    /// Связывает запрос с подтверждением (транспорт и тесты).
    pub fn new(
        request: ForwardedInstanceRequest,
        acknowledgement: ForwardedRequestAcknowledgement,
    ) -> Self {
        Self {
            request,
            acknowledgement,
        }
    }

    /// Разбирает доставку на запрос и подтверждение.
    pub fn into_parts(self) -> (ForwardedInstanceRequest, ForwardedRequestAcknowledgement) {
        (self.request, self.acknowledgement)
    }
}

/// Почему приложение не смогло принять доставку в свой почтовый ящик.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ForwardedRequestSinkError {
    /// Ящик переполнен: UI-поток давно не разбирал запросы.
    #[error("почтовый ящик пересланных запросов переполнен")]
    Full,
    /// Приложение завершается и больше не принимает запросы.
    #[error("приложение больше не принимает пересланные запросы")]
    Closed,
}

/// Приёмник проверенных запросов на стороне приложения.
///
/// Вызывается из потока транспорта; реализация обязана быть неблокирующей
/// (положить в ограниченный ящик и разбудить UI-поток).
pub trait ForwardedRequestSink: Send + Sync + 'static {
    /// Передаёт доставку приложению. При ошибке доставка возвращается не будет:
    /// подтверждение уничтожается, и отправитель получает отказ.
    fn deliver(&self, delivery: ForwardedRequestDelivery) -> Result<(), ForwardedRequestSinkError>;
}

/// Настройки службы приёма.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceForwardingServiceConfig {
    /// Сколько ждать, пока UI-поток заберёт запрос, прежде чем ответить «не отвечает».
    pub acknowledgement_timeout: Duration,
}

/// Работающая служба приёма. Остановка — [`Self::shutdown`] или `Drop`.
pub struct InstanceForwardingService {
    #[cfg(target_os = "linux")]
    backend: linux::ServiceBackend,
}

impl fmt::Debug for InstanceForwardingService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstanceForwardingService")
            .finish_non_exhaustive()
    }
}

impl InstanceForwardingService {
    /// Освобождает имя на шине и закрывает соединение. Запросы, ждущие
    /// подтверждения, получат ответ «экземпляр завершается».
    pub fn shutdown(self) {
        #[cfg(target_os = "linux")]
        self.backend.shutdown();
    }
}

/// Почему служба приёма не запустилась. Приложение при этом работает дальше,
/// просто второй запуск не сможет передать ему файл.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstanceForwardingServiceError {
    /// Нет сессионной шины (запуск вне графического сеанса).
    #[error("сессионная шина D-Bus недоступна: {0}")]
    SessionBusUnavailable(String),
    /// Имя приложения на шине уже занято другим процессом.
    #[error("имя приложения на сессионной шине уже занято")]
    BusNameUnavailable,
    /// Прочая ошибка транспорта.
    #[error("ошибка транспорта службы пересылки: {0}")]
    Transport(String),
    /// На этой платформе транспорт пересылки не реализован.
    #[error("пересылка запросов между экземплярами не поддерживается на этой платформе")]
    UnsupportedPlatform,
}

/// Запускает службу приёма запросов от следующих запусков.
///
/// Вызывать только под уже взятым process lease: имя на шине принадлежит
/// единственному настоящему экземпляру.
pub fn start_instance_forwarding_service(
    config: InstanceForwardingServiceConfig,
    sink: Arc<dyn ForwardedRequestSink>,
) -> Result<InstanceForwardingService, InstanceForwardingServiceError> {
    #[cfg(target_os = "linux")]
    {
        linux::start_on_session_bus(config, sink)
            .map(|backend| InstanceForwardingService { backend })
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, sink);
        Err(InstanceForwardingServiceError::UnsupportedPlatform)
    }
}

/// Настройки отправки из второго экземпляра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceForwardingClientConfig {
    /// Сколько ждать, пока первый экземпляр (возможно, ещё стартующий) начнёт
    /// принимать запросы.
    pub listener_wait: Duration,
    /// Как часто проверять, появился ли приёмник.
    pub listener_poll_interval: Duration,
    /// Сколько ждать ответа на сам запрос. Должно быть больше, чем
    /// `acknowledgement_timeout` службы, чтобы получить её осмысленный ответ.
    pub call_timeout: Duration,
}

/// Почему запрос не удалось передать первому экземпляру.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstanceForwardingError {
    /// Нет сессионной шины.
    #[error("сессионная шина D-Bus недоступна: {0}")]
    SessionBusUnavailable(String),
    /// Первый экземпляр так и не начал принимать запросы за время ожидания.
    #[error("запущенный экземпляр не принимает запросы")]
    InstanceNotListening,
    /// Первый экземпляр принимает запросы, но не ответил вовремя (завис).
    #[error("запущенный экземпляр не отвечает")]
    InstanceNotResponding,
    /// Первый экземпляр завершается и запрос не принял.
    #[error("запущенный экземпляр завершается")]
    InstanceShuttingDown,
    /// Имя на шине принадлежит процессу другого пользователя.
    #[error("имя приложения на шине принадлежит другому пользователю")]
    ForeignInstanceOwner,
    /// Первый экземпляр отклонил запрос как некорректный.
    #[error("запущенный экземпляр отклонил запрос: {detail}")]
    Rejected {
        /// Техническая причина для лога.
        detail: String,
    },
    /// Прочая ошибка транспорта.
    #[error("ошибка транспорта пересылки: {0}")]
    Transport(String),
    /// На этой платформе транспорт пересылки не реализован.
    #[error("пересылка запросов между экземплярами не поддерживается на этой платформе")]
    UnsupportedPlatform,
}

/// Передаёт запрос уже запущенному экземпляру и ждёт подтверждения.
///
/// Блокирующий вызов: делается из `main` второго процесса до создания окна.
/// `Ok` означает, что UI-поток первого экземпляра принял запрос.
pub fn forward_to_running_instance(
    request: &ForwardedInstanceRequest,
    config: InstanceForwardingClientConfig,
) -> Result<(), InstanceForwardingError> {
    #[cfg(target_os = "linux")]
    {
        linux::forward_on_session_bus(request, config)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (request, config);
        Err(InstanceForwardingError::UnsupportedPlatform)
    }
}
