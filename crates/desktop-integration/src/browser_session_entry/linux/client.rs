//! Клиент входа: будит браузерный режим и передаёт ему сообщение.
//!
//! Порядок:
//! 1. подключиться к сессионной шине;
//! 2. `StartServiceByName` (запуск по `.service`-файлу, если службы ещё нет) с
//!    ограничением `activation_timeout`; «службы нет и файла тоже» → `NotInstalled`;
//! 3. убедиться, что имя принадлежит процессу того же пользователя (иначе ничего
//!    не отправлять);
//! 4. вызвать `Entry` с ограничением `call_timeout`.
//!
//! Оба вызова идут через низкоуровневый `Connection::call_method`: в zbus 5.15
//! `#[proxy]`-вызовы игнорируют `method_timeout` (см. `instance_forwarding`).
//! Таймаут задаётся на соединение, а нужны два разных, поэтому открываются два
//! соединения: одно на активацию, второе на сам звонок.

use std::time::Duration;

use tracing::{info, warn};
use zbus::blocking::{Connection, connection};

use super::{NOT_RESPONDING_ERROR_NAME, REJECTED_ERROR_NAME, SHUTTING_DOWN_ERROR_NAME};
use crate::browser_session_entry::{
    BROWSER_SESSION_BUS_NAME, BROWSER_SESSION_INTERFACE, BROWSER_SESSION_OBJECT_PATH,
    BrowserEntryBus, BrowserEntryCallError, BrowserEntryClientConfig,
    MAX_BROWSER_ENTRY_MESSAGE_BYTES,
};
use crate::peer_admission::{BusQueryError, blocking_name_has_owner, blocking_unix_user};

const DBUS_SERVICE: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";

/// Стандартная ошибка шины: имени нет и запустить его нечем.
const SERVICE_UNKNOWN_ERROR_NAME: &str = "org.freedesktop.DBus.Error.ServiceUnknown";
/// Стандартная ошибка шины: у имени нет владельца.
const NAME_HAS_NO_OWNER_ERROR_NAME: &str = "org.freedesktop.DBus.Error.NameHasNoOwner";
/// Стандартная ошибка шины: получатель отключился, не ответив.
const NO_REPLY_ERROR_NAME: &str = "org.freedesktop.DBus.Error.NoReply";
/// Префикс ошибок запуска службы шиной (`Spawn.ChildExited`, `Spawn.ExecFailed`, …).
const SPAWN_ERROR_PREFIX: &str = "org.freedesktop.DBus.Error.Spawn.";

/// Флаги `StartServiceByName` зарезервированы и должны быть нулём.
const START_SERVICE_FLAGS: u32 = 0;

/// Соединение клиента: живёт от `connect` до `Drop`, все звонки идут по нему.
pub(crate) struct ClientBackend {
    connection: Connection,
}

/// Подключается к выбранной шине (обычной или по адресу).
pub(crate) fn connect(
    bus: BrowserEntryBus,
    config: BrowserEntryClientConfig,
) -> Result<ClientBackend, BrowserEntryCallError> {
    connect_with(
        |method_timeout| match &bus {
            BrowserEntryBus::Session => connection::Builder::session()?
                .method_timeout(method_timeout)
                .build(),
            BrowserEntryBus::Address(address) => connection::Builder::address(address.as_str())?
                .method_timeout(method_timeout)
                .build(),
        },
        config,
    )
}

/// Подключается через шину, которую открывает `open_connection(method_timeout)`.
///
/// Таймаут задаётся на соединение, а нужны два разных: на запуск службы и на звонок.
/// Поэтому запуск идёт по короткоживущему соединению, а клиенту остаётся второе,
/// с `call_timeout`.
pub(super) fn connect_with(
    open_connection: impl Fn(Duration) -> zbus::Result<Connection>,
    config: BrowserEntryClientConfig,
) -> Result<ClientBackend, BrowserEntryCallError> {
    let open = |timeout| {
        open_connection(timeout)
            .map_err(|error| BrowserEntryCallError::SessionBusUnavailable(error.to_string()))
    };
    let activation_connection = open(config.activation_timeout)?;
    start_service(&activation_connection)?;
    drop(activation_connection);

    let connection = open(config.call_timeout)?;
    ensure_service_belongs_to_same_user(&connection)?;
    Ok(ClientBackend { connection })
}

impl ClientBackend {
    /// Звонок `Entry` по постоянному соединению.
    pub(crate) fn call(&self, message: &str) -> Result<String, BrowserEntryCallError> {
        // Слишком большое сообщение бессмысленно даже пытаться доставить.
        if message.len() > MAX_BROWSER_ENTRY_MESSAGE_BYTES {
            return Err(BrowserEntryCallError::Rejected {
                detail: format!(
                    "message is longer than the limit of {MAX_BROWSER_ENTRY_MESSAGE_BYTES} bytes"
                ),
            });
        }
        let reply = self
            .connection
            .call_method(
                Some(BROWSER_SESSION_BUS_NAME),
                BROWSER_SESSION_OBJECT_PATH,
                Some(BROWSER_SESSION_INTERFACE),
                "Entry",
                &(message,),
            )
            .map_err(classify_call_error)?;
        let reply_text: String = reply
            .body()
            .deserialize()
            .map_err(|error| BrowserEntryCallError::Transport(error.to_string()))?;
        info!("Browser entry call answered");
        Ok(reply_text)
    }
}

/// Просит шину запустить службу (если она уже работает, шина это просто сообщает).
fn start_service(connection: &Connection) -> Result<(), BrowserEntryCallError> {
    // Уже работающая служба без `.service`-файла (запуск вручную) не должна выглядеть
    // как «не установлено», а `StartServiceByName` для неё отвечает именно так:
    // шина сначала ищет файл активации, а не владельца. Поэтому владелец проверяется
    // раньше; гонка (владелец исчез) всё равно закончится честным результатом запуска.
    // Запрос идёт низкоуровневым вызовом, чтобы соблюдался таймаут соединения.
    if blocking_name_has_owner(connection, BROWSER_SESSION_BUS_NAME).map_err(bus_query_error)? {
        return Ok(());
    }
    connection
        .call_method(
            Some(DBUS_SERVICE),
            DBUS_PATH,
            Some(DBUS_INTERFACE),
            "StartServiceByName",
            &(BROWSER_SESSION_BUS_NAME, START_SERVICE_FLAGS),
        )
        .map(|_start_result| ())
        .map_err(classify_start_error)
}

fn classify_start_error(error: zbus::Error) -> BrowserEntryCallError {
    match &error {
        zbus::Error::InputOutput(io_error) if io_error.kind() == std::io::ErrorKind::TimedOut => {
            BrowserEntryCallError::ActivationFailed {
                detail: "activation timed out".to_owned(),
            }
        }
        zbus::Error::MethodError(name, detail, _reply) => match name.as_str() {
            SERVICE_UNKNOWN_ERROR_NAME => BrowserEntryCallError::NotInstalled,
            other if other.starts_with(SPAWN_ERROR_PREFIX) || other == NO_REPLY_ERROR_NAME => {
                BrowserEntryCallError::ActivationFailed {
                    detail: format!("{other}: {}", detail.as_deref().unwrap_or_default()),
                }
            }
            _ => BrowserEntryCallError::Transport(error.to_string()),
        },
        _ => BrowserEntryCallError::Transport(error.to_string()),
    }
}

/// Отказывается слать сообщение процессу другого пользователя.
fn ensure_service_belongs_to_same_user(
    connection: &Connection,
) -> Result<(), BrowserEntryCallError> {
    let own_unique_name = connection.unique_name().ok_or_else(|| {
        BrowserEntryCallError::Transport("The connection has no unique name".to_owned())
    })?;
    let own_uid =
        blocking_unix_user(connection, own_unique_name.as_str()).map_err(bus_query_error)?;
    let service_uid = match blocking_unix_user(connection, BROWSER_SESSION_BUS_NAME) {
        Ok(service_uid) => service_uid,
        // Владелец успел исчезнуть между проверками: служба завершилась.
        Err(BusQueryError::NoOwner) => return Err(BrowserEntryCallError::ShuttingDown),
        Err(error) => return Err(bus_query_error(error)),
    };
    owner_verdict(own_uid, service_uid)
}

/// Таймаут запроса к шине — «не отвечает», остальное — транспорт.
fn bus_query_error(error: BusQueryError) -> BrowserEntryCallError {
    match error {
        BusQueryError::TimedOut => BrowserEntryCallError::NotResponding,
        other => BrowserEntryCallError::Transport(other.to_string()),
    }
}

/// Решение по uid владельца имени (отдельно, чтобы проверить без второго пользователя).
fn owner_verdict(own_uid: u32, service_uid: u32) -> Result<(), BrowserEntryCallError> {
    if own_uid == service_uid {
        Ok(())
    } else {
        warn!(
            service_uid,
            "The name of the browser session belongs to another user"
        );
        Err(BrowserEntryCallError::ForeignOwner)
    }
}

/// Переводит ошибку вызова `Entry` в типизированную причину.
fn classify_call_error(error: zbus::Error) -> BrowserEntryCallError {
    match &error {
        zbus::Error::InputOutput(io_error) if io_error.kind() == std::io::ErrorKind::TimedOut => {
            BrowserEntryCallError::NotResponding
        }
        zbus::Error::MethodError(name, detail, _reply) => match name.as_str() {
            REJECTED_ERROR_NAME => BrowserEntryCallError::Rejected {
                detail: detail.clone().unwrap_or_default(),
            },
            NOT_RESPONDING_ERROR_NAME => BrowserEntryCallError::NotResponding,
            SHUTTING_DOWN_ERROR_NAME | NO_REPLY_ERROR_NAME | NAME_HAS_NO_OWNER_ERROR_NAME => {
                BrowserEntryCallError::ShuttingDown
            }
            _ => BrowserEntryCallError::Transport(error.to_string()),
        },
        _ => BrowserEntryCallError::Transport(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{BrowserEntryCallError, owner_verdict};

    #[test]
    fn service_owned_by_another_user_is_refused() {
        assert_eq!(owner_verdict(1000, 1000), Ok(()));
        assert_eq!(
            owner_verdict(1000, 0),
            Err(BrowserEntryCallError::ForeignOwner)
        );
    }
}
