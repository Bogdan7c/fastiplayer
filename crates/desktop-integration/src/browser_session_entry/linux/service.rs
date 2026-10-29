//! Служба входа: объект `BrowserSession1` процесса браузерного режима.
//!
//! Порядок обработки `Entry`:
//! 1. отправитель опознаётся по uid через шину (общий допуск `peer_admission`);
//! 2. размер сообщения проверяется до любой доставки;
//! 3. сообщение кладётся в неблокирующий приёмник приложения;
//! 4. обработчик асинхронно ждёт ответа приложения с таймаутом.
//!
//! Ожидание обязано быть асинхронным по той же причине, что и в `instance_forwarding`:
//! обработчики zbus идут на executor соединения, и блокировка потока остановила бы
//! чтение сокета, включая ответ на собственный запрос uid отправителя.

use std::sync::Arc;
use std::time::Duration;

use futures_lite::FutureExt;
use tracing::{debug, error, info, warn};
use zbus::blocking::{Connection, connection};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::BusName;
use zbus::{DBusError, interface};

use crate::browser_session_entry::{
    BROWSER_SESSION_BUS_NAME, BROWSER_SESSION_OBJECT_PATH, BrowserEntryCaller,
    BrowserEntryDelivery, BrowserEntryReply, BrowserEntryRequest, BrowserEntryServiceConfig,
    BrowserEntryServiceError, BrowserEntrySink, BrowserEntrySinkError,
    MAX_BROWSER_ENTRY_MESSAGE_BYTES,
};
use crate::peer_admission::{admit_peer_uid, blocking_unix_user, query_peer_unix_user};

/// Ошибки, которые служба возвращает звонящему по шине.
#[derive(Debug, DBusError)]
#[zbus(prefix = "io.github.Bogdan7c.Fastiplayer.BrowserSession.Error")]
pub(super) enum EntryServiceFailure {
    /// Внутренняя ошибка zbus при обработке вызова.
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Запрос некорректен или отправитель не допущен.
    Rejected(String),
    /// Приложение не ответило за отведённое время (или ответ непригоден).
    NotResponding(String),
    /// Приложение завершается и вход не примет.
    ShuttingDown(String),
}

/// Объект на шине. Владеет только приёмником и параметрами проверки.
struct BrowserSessionEndpoint {
    sink: Arc<dyn BrowserEntrySink>,
    /// uid владельца сеанса, вычисленный при старте по собственному соединению.
    own_uid: u32,
    reply_timeout: Duration,
}

// Имя интерфейса продублировано литералом (макрос требует литерал); совпадение с
// `BROWSER_SESSION_INTERFACE` закреплено тестом.
#[interface(name = "io.github.Bogdan7c.Fastiplayer.BrowserSession1")]
impl BrowserSessionEndpoint {
    /// `Entry(s) -> s`: JSON входа от посредника, JSON ответа от приложения.
    async fn entry(
        &self,
        message: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> Result<String, EntryServiceFailure> {
        let caller = self.admit_sender(&header, connection).await?;
        if message.len() > MAX_BROWSER_ENTRY_MESSAGE_BYTES {
            warn!(
                message_bytes = message.len(),
                "Browser entry message is over the size limit and was rejected"
            );
            return Err(EntryServiceFailure::Rejected(format!(
                "message is longer than the limit of {MAX_BROWSER_ENTRY_MESSAGE_BYTES} bytes"
            )));
        }
        self.deliver_and_wait(BrowserEntryRequest { caller, message })
            .await
    }
}

/// Чем закончилось ожидание ответа приложения.
#[derive(Debug, PartialEq, Eq)]
enum ReplyOutcome {
    /// Приложение ответило.
    Replied(String),
    /// Слот ответа уничтожен без ответа (приложение завершается).
    Discarded,
    /// Приложение не ответило вовремя.
    TimedOut,
}

impl BrowserSessionEndpoint {
    /// Допускает только процессы того же пользователя; возвращает идентификатор звонящего.
    async fn admit_sender(
        &self,
        header: &Header<'_>,
        connection: &zbus::Connection,
    ) -> Result<BrowserEntryCaller, EntryServiceFailure> {
        let peer_uid = match header.sender() {
            Some(sender) => query_peer_unix_user(connection, BusName::from(sender.clone())).await,
            None => None,
        };
        admit_peer_uid(peer_uid, self.own_uid).map_err(|denial| {
            warn!(%denial, "Browser entry request denied");
            EntryServiceFailure::Rejected(denial.to_string())
        })?;
        // Допуск прошёл только для опознанного отправителя, значит имя есть.
        let sender_name = header
            .sender()
            .map(ToString::to_string)
            .ok_or_else(|| EntryServiceFailure::Rejected("sender is unknown".to_owned()))?;
        Ok(BrowserEntryCaller::from_sender_name(sender_name))
    }

    /// Передаёт запрос приложению и ждёт его ответа.
    async fn deliver_and_wait(
        &self,
        request: BrowserEntryRequest,
    ) -> Result<String, EntryServiceFailure> {
        // Канал на одно сообщение: ответ приходит из любого потока синхронно, а ждётся
        // здесь асинхронно, не блокируя executor соединения.
        let (reply_sender, reply_receiver) = async_channel::bounded::<String>(1);
        let reply = BrowserEntryReply::from_callback(move |reply_text| {
            if reply_sender.try_send(reply_text).is_err() {
                debug!("Browser entry reply came after the timeout");
            }
        });

        self.sink
            .deliver(BrowserEntryDelivery::new(request, reply))
            .map_err(|sink_error| match sink_error {
                // Переполненный ящик значит, что главный цикл давно не разбирал запросы.
                BrowserEntrySinkError::Full => {
                    EntryServiceFailure::NotResponding(sink_error.to_string())
                }
                BrowserEntrySinkError::Closed => {
                    EntryServiceFailure::ShuttingDown(sink_error.to_string())
                }
            })?;

        match wait_for_reply(reply_receiver, self.reply_timeout).await {
            ReplyOutcome::Replied(reply_text) => accept_reply(reply_text),
            ReplyOutcome::Discarded => Err(EntryServiceFailure::ShuttingDown(
                "application is shutting down".to_owned(),
            )),
            ReplyOutcome::TimedOut => {
                warn!("The application did not answer the browser entry on time");
                Err(EntryServiceFailure::NotResponding(
                    "application failed to respond in time".to_owned(),
                ))
            }
        }
    }
}

/// Проверяет ответ приложения перед отправкой звонящему.
///
/// Ответ больше лимита — ошибка приложения, не звонящего: ему возвращается
/// `NotResponding` (пригодного ответа нет), а подробность пишется в лог. Усечённый
/// JSON отправлять нельзя: получатель принял бы мусор за ответ.
fn accept_reply(reply_text: String) -> Result<String, EntryServiceFailure> {
    if reply_text.len() > MAX_BROWSER_ENTRY_MESSAGE_BYTES {
        error!(
            reply_bytes = reply_text.len(),
            "Application reply to the browser entry is over the size limit and was dropped"
        );
        return Err(EntryServiceFailure::NotResponding(
            "application reply is too large".to_owned(),
        ));
    }
    info!("Browser entry answered");
    Ok(reply_text)
}

/// Ждёт ответа не дольше `timeout`.
async fn wait_for_reply(
    reply_receiver: async_channel::Receiver<String>,
    timeout: Duration,
) -> ReplyOutcome {
    let replied = async {
        match reply_receiver.recv().await {
            Ok(reply_text) => ReplyOutcome::Replied(reply_text),
            // Отправитель канала уничтожен без вызова: слот ответа выброшен.
            Err(_closed) => ReplyOutcome::Discarded,
        }
    };
    let expired = async {
        async_io::Timer::after(timeout).await;
        ReplyOutcome::TimedOut
    };
    replied.or(expired).await
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
        // Имя освобождается первым: новые звонки сразу увидят, что службы нет
        // (и запустят новую), а не будут ждать закрывающегося соединения.
        if let Err(error) = connection.release_name(BROWSER_SESSION_BUS_NAME) {
            warn!(%error, "The name of the browser entry service was not released");
        }
        if let Err(error) = connection.close() {
            warn!(%error, "Failed to close the browser entry service connection");
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
    config: BrowserEntryServiceConfig,
    sink: Arc<dyn BrowserEntrySink>,
) -> Result<ServiceBackend, BrowserEntryServiceError> {
    start_with(|| connection::Builder::session()?.build(), config, sink)
}

/// Запускает службу на шине, которую открывает `connect` (тесты — частная шина).
pub(super) fn start_with(
    connect: impl FnOnce() -> zbus::Result<Connection>,
    config: BrowserEntryServiceConfig,
    sink: Arc<dyn BrowserEntrySink>,
) -> Result<ServiceBackend, BrowserEntryServiceError> {
    let connection = connect()
        .map_err(|error| BrowserEntryServiceError::SessionBusUnavailable(error.to_string()))?;
    let own_uid = own_unix_user(&connection)?;
    let endpoint = BrowserSessionEndpoint {
        sink,
        own_uid,
        reply_timeout: config.reply_timeout,
    };

    // Объект регистрируется до запроса имени: иначе первый звонок, разбудивший
    // службу активацией, мог бы прийти на имя, у которого ещё нет объекта.
    connection
        .object_server()
        .at(BROWSER_SESSION_OBJECT_PATH, endpoint)
        .map_err(transport_error)?;

    // Только `DoNotQueue` без замены: второй браузерный процесс не отнимает имя у живого.
    match connection.request_name_with_flags(
        BROWSER_SESSION_BUS_NAME,
        RequestNameFlags::DoNotQueue.into(),
    ) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            info!("Browser entry service started");
            Ok(ServiceBackend {
                connection: Some(connection),
            })
        }
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
            Err(BrowserEntryServiceError::BusNameUnavailable)
        }
        Err(error) => Err(transport_error(error)),
    }
}

/// uid владельца собственного соединения — эталон для проверки отправителей.
fn own_unix_user(connection: &Connection) -> Result<u32, BrowserEntryServiceError> {
    let unique_name = connection.unique_name().ok_or_else(|| {
        BrowserEntryServiceError::Transport("The connection has no unique name".to_owned())
    })?;
    blocking_unix_user(connection, unique_name.as_str())
        .map_err(|error| BrowserEntryServiceError::Transport(error.to_string()))
}

fn transport_error(error: zbus::Error) -> BrowserEntryServiceError {
    BrowserEntryServiceError::Transport(error.to_string())
}
