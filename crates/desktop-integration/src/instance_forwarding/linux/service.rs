//! Служба приёма: объект `org.freedesktop.Application` первого экземпляра.
//!
//! Порядок обработки каждого вызова:
//! 1. отправитель опознаётся по uid через шину; чужой uid — отказ;
//! 2. аргументы проверяются правилами `admission` (лимиты, только URI);
//! 3. запрос кладётся в неблокирующий приёмник приложения;
//! 4. обработчик асинхронно ждёт подтверждения UI-потока с таймаутом и только
//!    потом отвечает отправителю.
//!
//! Ожидание обязано быть асинхронным: обработчики zbus выполняются на внутреннем
//! executor соединения, и блокировка потока остановила бы чтение сокета, включая
//! ответ на собственный запрос uid отправителя (взаимная блокировка).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_lite::FutureExt;
use tracing::{debug, info, warn};
use zbus::blocking::{Connection, connection};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::BusName;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{DBusError, interface};

use super::{
    ACTIVATION_TOKEN_PLATFORM_KEY, APPLICATION_OBJECT_PATH, DESKTOP_STARTUP_ID_PLATFORM_KEY,
};
use crate::instance_forwarding::admission::admit_peer;
use crate::instance_forwarding::{
    FASTIPLAYER_APPLICATION_ID, ForwardedInstanceRequest, ForwardedRequestAcknowledgement,
    ForwardedRequestDelivery, ForwardedRequestRejection, ForwardedRequestSink,
    ForwardedRequestSinkError, InstanceForwardingServiceConfig, InstanceForwardingServiceError,
    WindowActivationToken,
};

/// Ошибки, которые служба возвращает отправителю по шине.
///
/// Имена D-Bus ошибок складываются из `prefix` и имени варианта; клиент различает
/// их по строковым константам из `super` (совпадение закреплено тестом).
#[derive(Debug, DBusError)]
#[zbus(prefix = "io.github.Bogdan7c.Fastiplayer.Error")]
pub(super) enum ForwardingServiceFailure {
    /// Внутренняя ошибка zbus при обработке вызова.
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Запрос некорректен или отправитель не допущен.
    Rejected(String),
    /// UI-поток не забрал запрос за отведённое время.
    NotResponding(String),
    /// Приложение завершается и запрос не примет.
    ShuttingDown(String),
}

impl From<ForwardedRequestRejection> for ForwardingServiceFailure {
    fn from(rejection: ForwardedRequestRejection) -> Self {
        Self::Rejected(rejection.to_string())
    }
}

/// Объект на шине. Владеет только приёмником и параметрами проверки.
struct FreedesktopApplicationEndpoint {
    sink: Arc<dyn ForwardedRequestSink>,
    /// uid владельца сеанса, вычисленный при старте по собственному соединению.
    own_uid: u32,
    acknowledgement_timeout: Duration,
}

#[interface(name = "org.freedesktop.Application")]
impl FreedesktopApplicationEndpoint {
    /// `Activate(a{sv})`: второй запуск без файлов — только поднять окно.
    async fn activate(
        &self,
        platform_data: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> Result<(), ForwardingServiceFailure> {
        self.admit_sender(&header, connection).await?;
        let request =
            ForwardedInstanceRequest::activate(activation_token_from_platform_data(&platform_data));
        self.deliver_and_wait(request).await
    }

    /// `Open(as, a{sv})`: открыть URI в порядке запроса и поднять окно.
    async fn open(
        &self,
        uris: Vec<String>,
        platform_data: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> Result<(), ForwardingServiceFailure> {
        self.admit_sender(&header, connection).await?;
        let request = ForwardedInstanceRequest::open(
            uris,
            activation_token_from_platform_data(&platform_data),
        )
        .inspect_err(|rejection| warn!(%rejection, "Пересланный запрос Open отклонён"))?;
        self.deliver_and_wait(request).await
    }

    /// `ActivateAction(s, av, a{sv})`: обязателен по спецификации интерфейса, но
    /// именованных действий у приложения нет. Протокол исполняет только Open/Activate.
    async fn activate_action(
        &self,
        _action_name: String,
        _parameter: Vec<OwnedValue>,
        _platform_data: HashMap<String, OwnedValue>,
    ) -> Result<(), ForwardingServiceFailure> {
        warn!("Пересланный запрос ActivateAction отклонён: действия не поддерживаются");
        Err(ForwardedRequestRejection::UnsupportedAction.into())
    }
}

/// Чем закончилось ожидание подтверждения UI-потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcknowledgementOutcome {
    /// UI-поток забрал запрос.
    Accepted,
    /// Подтверждение уничтожено без ответа (приложение завершается).
    Discarded,
    /// UI-поток не ответил вовремя.
    TimedOut,
}

impl FreedesktopApplicationEndpoint {
    /// Допускает только процессы того же пользователя.
    async fn admit_sender(
        &self,
        header: &Header<'_>,
        connection: &zbus::Connection,
    ) -> Result<(), ForwardingServiceFailure> {
        let peer_uid = match header.sender() {
            Some(sender) => query_peer_unix_user(connection, BusName::from(sender.clone())).await,
            None => None,
        };
        admit_peer(peer_uid, self.own_uid)
            .inspect_err(|rejection| warn!(%rejection, "Пересланный запрос отклонён"))
            .map_err(ForwardingServiceFailure::from)
    }

    /// Передаёт запрос приложению и ждёт, пока UI-поток его заберёт.
    async fn deliver_and_wait(
        &self,
        request: ForwardedInstanceRequest,
    ) -> Result<(), ForwardingServiceFailure> {
        // Канал на одно сообщение: подтверждение приходит из UI-потока синхронно,
        // а ждётся здесь асинхронно, не блокируя executor соединения.
        let (acknowledged_sender, acknowledged_receiver) = async_channel::bounded::<()>(1);
        let acknowledgement = ForwardedRequestAcknowledgement::from_callback(move || {
            if acknowledged_sender.try_send(()).is_err() {
                debug!("Подтверждение пересланного запроса пришло после таймаута");
            }
        });

        self.sink
            .deliver(ForwardedRequestDelivery::new(request, acknowledgement))
            .map_err(|sink_error| match sink_error {
                // Переполненный ящик значит, что UI-поток давно не разбирал запросы.
                ForwardedRequestSinkError::Full => {
                    ForwardingServiceFailure::NotResponding(sink_error.to_string())
                }
                ForwardedRequestSinkError::Closed => {
                    ForwardingServiceFailure::ShuttingDown(sink_error.to_string())
                }
            })?;

        match wait_for_acknowledgement(acknowledged_receiver, self.acknowledgement_timeout).await {
            AcknowledgementOutcome::Accepted => {
                info!("Пересланный запрос принят UI-потоком");
                Ok(())
            }
            AcknowledgementOutcome::Discarded => Err(ForwardingServiceFailure::ShuttingDown(
                "приложение завершается".to_owned(),
            )),
            AcknowledgementOutcome::TimedOut => {
                warn!("UI-поток не забрал пересланный запрос вовремя");
                Err(ForwardingServiceFailure::NotResponding(
                    "UI-поток не ответил вовремя".to_owned(),
                ))
            }
        }
    }
}

/// Ждёт подтверждения не дольше `timeout`.
async fn wait_for_acknowledgement(
    acknowledged_receiver: async_channel::Receiver<()>,
    timeout: Duration,
) -> AcknowledgementOutcome {
    let acknowledged = async {
        match acknowledged_receiver.recv().await {
            Ok(()) => AcknowledgementOutcome::Accepted,
            // Отправитель канала уничтожен без вызова: подтверждение выброшено.
            Err(_closed) => AcknowledgementOutcome::Discarded,
        }
    };
    let expired = async {
        async_io::Timer::after(timeout).await;
        AcknowledgementOutcome::TimedOut
    };
    acknowledged.or(expired).await
}

/// Спрашивает у шины uid владельца соединения. `None` — шина не ответила.
async fn query_peer_unix_user(connection: &zbus::Connection, peer: BusName<'_>) -> Option<u32> {
    let bus = match zbus::fdo::DBusProxy::new(connection).await {
        Ok(bus) => bus,
        Err(error) => {
            warn!(%error, "Не удалось создать proxy шины для проверки отправителя");
            return None;
        }
    };
    match bus.get_connection_unix_user(peer).await {
        Ok(peer_uid) => Some(peer_uid),
        Err(error) => {
            warn!(%error, "Шина не сообщила uid отправителя");
            None
        }
    }
}

/// Достаёт билет активации из `platform_data`. Непригодное значение игнорируется
/// (окно тогда только попросит внимания), запрос из-за него не отклоняется.
fn activation_token_from_platform_data(
    platform_data: &HashMap<String, OwnedValue>,
) -> Option<WindowActivationToken> {
    [
        ACTIVATION_TOKEN_PLATFORM_KEY,
        DESKTOP_STARTUP_ID_PLATFORM_KEY,
    ]
    .into_iter()
    .filter_map(|key| platform_data.get(key))
    .find_map(|value| match &**value {
        Value::Str(token) => WindowActivationToken::from_raw(token.as_str().to_owned()),
        _ => None,
    })
}

/// Работающая служба: соединение, на котором зарегистрированы объект и имя.
pub(crate) struct ServiceBackend {
    connection: Option<Connection>,
}

impl ServiceBackend {
    /// Явная остановка; повторная (из `Drop`) ничего не делает.
    pub(crate) fn shutdown(mut self) {
        self.close();
    }

    fn close(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        // Имя освобождается первым: новые запуски сразу увидят, что приёмника нет,
        // и не будут ждать ответа от закрывающегося соединения.
        if let Err(error) = connection.release_name(FASTIPLAYER_APPLICATION_ID) {
            warn!(%error, "Не удалось освободить имя службы пересылки");
        }
        if let Err(error) = connection.close() {
            warn!(%error, "Не удалось закрыть соединение службы пересылки");
        }
    }
}

impl Drop for ServiceBackend {
    fn drop(&mut self) {
        self.close();
    }
}

/// Запускает службу на сессионной шине пользователя.
pub(crate) fn start_on_session_bus(
    config: InstanceForwardingServiceConfig,
    sink: Arc<dyn ForwardedRequestSink>,
) -> Result<ServiceBackend, InstanceForwardingServiceError> {
    start_with(|| connection::Builder::session()?.build(), config, sink)
}

/// Запускает службу на шине, которую открывает `connect` (тесты — частная шина).
pub(super) fn start_with(
    connect: impl FnOnce() -> zbus::Result<Connection>,
    config: InstanceForwardingServiceConfig,
    sink: Arc<dyn ForwardedRequestSink>,
) -> Result<ServiceBackend, InstanceForwardingServiceError> {
    let connection = connect().map_err(|error| {
        InstanceForwardingServiceError::SessionBusUnavailable(error.to_string())
    })?;
    let own_uid = own_unix_user(&connection)?;
    let endpoint = FreedesktopApplicationEndpoint {
        sink,
        own_uid,
        acknowledgement_timeout: config.acknowledgement_timeout,
    };

    // Объект регистрируется до запроса имени: иначе первый вызов мог бы прийти
    // на имя, у которого ещё нет объекта.
    connection
        .object_server()
        .at(APPLICATION_OBJECT_PATH, endpoint)
        .map_err(transport_error)?;

    // Только `DoNotQueue`: без `AllowReplacement` никто не перехватит имя у живого
    // экземпляра, без `ReplaceExisting` мы не отнимаем имя у чужого процесса.
    match connection.request_name_with_flags(
        FASTIPLAYER_APPLICATION_ID,
        RequestNameFlags::DoNotQueue.into(),
    ) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            info!("Служба пересылки запросов второго запуска запущена");
            Ok(ServiceBackend {
                connection: Some(connection),
            })
        }
        // С `DoNotQueue` шина не ставит в очередь, но ответ обрабатывается явно.
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
            Err(InstanceForwardingServiceError::BusNameUnavailable)
        }
        Err(error) => Err(transport_error(error)),
    }
}

/// uid владельца собственного соединения — эталон для проверки отправителей.
fn own_unix_user(connection: &Connection) -> Result<u32, InstanceForwardingServiceError> {
    let unique_name = connection.unique_name().ok_or_else(|| {
        InstanceForwardingServiceError::Transport("у соединения нет уникального имени".to_owned())
    })?;
    let bus = zbus::blocking::fdo::DBusProxy::new(connection).map_err(transport_error)?;
    bus.get_connection_unix_user(BusName::from(unique_name.as_ref()))
        .map_err(|error| transport_error(error.into()))
}

fn transport_error(error: zbus::Error) -> InstanceForwardingServiceError {
    InstanceForwardingServiceError::Transport(error.to_string())
}
