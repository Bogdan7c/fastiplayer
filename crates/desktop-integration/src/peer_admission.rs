//! Общий допуск процессов по uid для D-Bus-служб crate-а.
//!
//! Сессионная шина по устройству доступна только владельцу сеанса, но каждая служба
//! (пересылка запросов второго запуска, вход браузерного режима) поверх этого
//! явно сверяет uid отправителя со своим: это второй рубеж, и правило у всех служб
//! обязано быть одним и тем же. Модуль владеет только этим правилом и спросом uid у
//! шины; что делать с отказом (какой текст и какая D-Bus ошибка), решает вызывающая
//! служба.

use thiserror::Error;

/// Почему отправителя не допустили.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum PeerAdmissionDenial {
    /// Запрос пришёл от процесса другого пользователя.
    #[error("request from another user's process (uid {peer_uid})")]
    ForeignUser {
        /// uid отправителя по данным шины.
        peer_uid: u32,
    },
    /// Отправителя не удалось опознать (шина не сообщила uid).
    #[error("It was not possible to determine the sender of the request")]
    UnidentifiedPeer,
}

/// Политика доступа: допускаются только процессы того же пользователя.
///
/// `peer_uid = None` — шина не сообщила uid отправителя; такой запрос отклоняется,
/// потому что без uid проверку пройти нельзя. root тоже «чужой»: служба сеанса
/// не принимает команды от других uid, даже привилегированных.
pub(crate) fn admit_peer_uid(
    peer_uid: Option<u32>,
    own_uid: u32,
) -> Result<(), PeerAdmissionDenial> {
    match peer_uid {
        Some(peer_uid) if peer_uid == own_uid => Ok(()),
        Some(peer_uid) => Err(PeerAdmissionDenial::ForeignUser { peer_uid }),
        None => Err(PeerAdmissionDenial::UnidentifiedPeer),
    }
}

/// Спрашивает у шины uid владельца соединения, не блокируя поток.
///
/// Асинхронность обязательна: вызывается из обработчиков zbus, которые выполняются на
/// executor соединения. `None` — шина не ответила (причина уходит в лог).
#[cfg(target_os = "linux")]
pub(crate) async fn query_peer_unix_user(
    connection: &zbus::Connection,
    peer: zbus::names::BusName<'_>,
) -> Option<u32> {
    let bus = match zbus::fdo::DBusProxy::new(connection).await {
        Ok(bus) => bus,
        Err(error) => {
            tracing::warn!(%error, "Failed to create a proxy bus to verify the sender");
            return None;
        }
    };
    match bus.get_connection_unix_user(peer).await {
        Ok(peer_uid) => Some(peer_uid),
        Err(error) => {
            tracing::warn!(%error, "Bus did not report the sender UID");
            None
        }
    }
}

/// Почему блокирующий запрос к самой шине не дал ответа.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum BusQueryError {
    /// У имени нет владельца.
    #[error("the bus name has no owner")]
    NoOwner,
    /// Шина не ответила за `method_timeout` соединения.
    #[error("the bus did not answer in time")]
    TimedOut,
    /// Прочая ошибка.
    #[error("bus query failed: {0}")]
    Failed(String),
}

#[cfg(target_os = "linux")]
const BUS_SERVICE: &str = "org.freedesktop.DBus";
#[cfg(target_os = "linux")]
const BUS_PATH: &str = "/org/freedesktop/DBus";
#[cfg(target_os = "linux")]
const NAME_HAS_NO_OWNER_ERROR_NAME: &str = "org.freedesktop.DBus.Error.NameHasNoOwner";

/// Вызов метода шины через низкоуровневый `Connection::call_method`.
///
/// Только он соблюдает `method_timeout` соединения: методы `DBusProxy` в zbus 5.15
/// его игнорируют и при зависшем собеседнике ждали бы вечно (ловушка описана в
/// `instance_forwarding::linux::client`). Поэтому на всех клиентских путях запросы к
/// шине идут здесь, а не через proxy.
#[cfg(target_os = "linux")]
fn call_bus<T>(
    connection: &zbus::blocking::Connection,
    method_name: &str,
    queried_name: &str,
) -> Result<T, BusQueryError>
where
    T: serde::de::DeserializeOwned + zbus::zvariant::Type,
{
    let reply = connection
        .call_method(
            Some(BUS_SERVICE),
            BUS_PATH,
            Some(BUS_SERVICE),
            method_name,
            &(queried_name,),
        )
        .map_err(|error| match &error {
            zbus::Error::InputOutput(io_error)
                if io_error.kind() == std::io::ErrorKind::TimedOut =>
            {
                BusQueryError::TimedOut
            }
            zbus::Error::MethodError(name, _detail, _reply)
                if name.as_str() == NAME_HAS_NO_OWNER_ERROR_NAME =>
            {
                BusQueryError::NoOwner
            }
            _ => BusQueryError::Failed(error.to_string()),
        })?;
    reply
        .body()
        .deserialize()
        .map_err(|error| BusQueryError::Failed(error.to_string()))
}

/// uid владельца соединения или имени (блокирующий вариант, соблюдает таймаут).
#[cfg(target_os = "linux")]
pub(crate) fn blocking_unix_user(
    connection: &zbus::blocking::Connection,
    name: &str,
) -> Result<u32, BusQueryError> {
    call_bus(connection, "GetConnectionUnixUser", name)
}

/// Есть ли у имени владелец (блокирующий вариант, соблюдает таймаут; службы не запускает).
#[cfg(target_os = "linux")]
pub(crate) fn blocking_name_has_owner(
    connection: &zbus::blocking::Connection,
    name: &str,
) -> Result<bool, BusQueryError> {
    call_bus(connection, "NameHasOwner", name)
}

#[cfg(test)]
mod tests {
    use super::{PeerAdmissionDenial, admit_peer_uid};

    #[test]
    fn only_the_same_uid_is_admitted() {
        assert_eq!(admit_peer_uid(Some(1000), 1000), Ok(()));
        assert_eq!(
            admit_peer_uid(Some(1001), 1000),
            Err(PeerAdmissionDenial::ForeignUser { peer_uid: 1001 })
        );
        assert_eq!(
            admit_peer_uid(Some(0), 1000),
            Err(PeerAdmissionDenial::ForeignUser { peer_uid: 0 })
        );
        assert_eq!(
            admit_peer_uid(None, 1000),
            Err(PeerAdmissionDenial::UnidentifiedPeer)
        );
    }
}
