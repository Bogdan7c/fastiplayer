//! Клиент второго экземпляра: находит первый на шине и передаёт ему запрос.
//!
//! Порядок:
//! 1. подключиться к сессионной шине с таймаутом ответа на вызовы;
//! 2. дождаться, пока имя приложения появится на шине: первый экземпляр мог взять
//!    process lease, но ещё не дойти до запуска службы;
//! 3. убедиться, что имя принадлежит процессу того же пользователя;
//! 4. вызвать `Open`/`Activate` с таймаутом ответа и разобрать ответ.

use std::collections::HashMap;
use std::time::Instant;

use tracing::{debug, info, warn};
use zbus::blocking::{Connection, connection};
use zbus::names::BusName;
use zbus::zvariant::Value;

use super::{
    ACTIVATION_TOKEN_PLATFORM_KEY, APPLICATION_OBJECT_PATH, NOT_RESPONDING_ERROR_NAME,
    REJECTED_ERROR_NAME, SHUTTING_DOWN_ERROR_NAME,
};
use crate::instance_forwarding::{
    FASTIPLAYER_APPLICATION_ID, ForwardedInstanceAction, ForwardedInstanceRequest,
    InstanceForwardingClientConfig, InstanceForwardingError,
};

/// Стандартная ошибка шины: у имени нет владельца.
const NAME_HAS_NO_OWNER_ERROR_NAME: &str = "org.freedesktop.DBus.Error.NameHasNoOwner";
/// Стандартная ошибка шины: имя неизвестно и автозапуск запрещён.
const SERVICE_UNKNOWN_ERROR_NAME: &str = "org.freedesktop.DBus.Error.ServiceUnknown";
/// Стандартная ошибка шины: получатель отключился, не ответив.
const NO_REPLY_ERROR_NAME: &str = "org.freedesktop.DBus.Error.NoReply";

/// Имя интерфейса из Desktop Entry Specification.
const APPLICATION_INTERFACE: &str = "org.freedesktop.Application";

/// Передаёт запрос через сессионную шину пользователя.
pub(crate) fn forward_on_session_bus(
    request: &ForwardedInstanceRequest,
    config: InstanceForwardingClientConfig,
) -> Result<(), InstanceForwardingError> {
    forward_with(
        || {
            connection::Builder::session()?
                .method_timeout(config.call_timeout)
                .build()
        },
        request,
        config,
    )
}

/// Передаёт запрос через шину, которую открывает `connect` (тесты — частная шина).
pub(super) fn forward_with(
    connect: impl FnOnce() -> zbus::Result<Connection>,
    request: &ForwardedInstanceRequest,
    config: InstanceForwardingClientConfig,
) -> Result<(), InstanceForwardingError> {
    let connection = connect()
        .map_err(|error| InstanceForwardingError::SessionBusUnavailable(error.to_string()))?;
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection).map_err(transport_error)?;
    let application_name =
        BusName::try_from(FASTIPLAYER_APPLICATION_ID).map_err(transport_error)?;

    wait_for_listener(&bus, &application_name, config)?;
    ensure_listener_belongs_to_same_user(&bus, &connection, &application_name)?;

    let platform_data = platform_data_for(request);

    // Низкоуровневый `call_method`, а не `#[proxy]`: в zbus 5.15 только он соблюдает
    // `method_timeout` соединения, proxy-вызов ждал бы зависший экземпляр вечно
    // (поймано тестом `completely_frozen_first_instance_is_reported_by_call_timeout`).
    // Флага «не автозапускать» у него нет; это безопасно: `.service`-файла у
    // приложения нет, а если появится, шина просто запустит плеер с этим запросом.
    let call_outcome = match request.action() {
        ForwardedInstanceAction::Activate => {
            call_application(&connection, "Activate", &(platform_data,))
        }
        ForwardedInstanceAction::Open(uris) => {
            let uri_texts: Vec<&str> = uris.iter().map(|uri| uri.as_str()).collect();
            call_application(&connection, "Open", &(uri_texts, platform_data))
        }
    };
    call_outcome.map_err(classify_call_error)?;
    info!("Запрос передан уже запущенному экземпляру");
    Ok(())
}

/// Вызывает метод `org.freedesktop.Application` первого экземпляра.
fn call_application(
    connection: &Connection,
    method_name: &str,
    arguments: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::Result<()> {
    connection
        .call_method(
            Some(FASTIPLAYER_APPLICATION_ID),
            APPLICATION_OBJECT_PATH,
            Some(APPLICATION_INTERFACE),
            method_name,
            arguments,
        )
        .map(|_reply| ())
}

/// Ждёт появления владельца имени не дольше `config.listener_wait`.
fn wait_for_listener(
    bus: &zbus::blocking::fdo::DBusProxy<'_>,
    application_name: &BusName<'_>,
    config: InstanceForwardingClientConfig,
) -> Result<(), InstanceForwardingError> {
    let started_at = Instant::now();
    loop {
        if bus
            .name_has_owner(application_name.clone())
            .map_err(transport_error)?
        {
            return Ok(());
        }
        if started_at.elapsed() >= config.listener_wait {
            warn!("Запущенный экземпляр так и не начал принимать запросы");
            return Err(InstanceForwardingError::InstanceNotListening);
        }
        debug!("Запущенный экземпляр ещё не принимает запросы, ждём");
        std::thread::sleep(config.listener_poll_interval);
    }
}

/// Отказывается слать пути процессу другого пользователя.
fn ensure_listener_belongs_to_same_user(
    bus: &zbus::blocking::fdo::DBusProxy<'_>,
    connection: &Connection,
    application_name: &BusName<'_>,
) -> Result<(), InstanceForwardingError> {
    let own_unique_name = connection.unique_name().ok_or_else(|| {
        InstanceForwardingError::Transport("у соединения нет уникального имени".to_owned())
    })?;
    let own_uid = bus
        .get_connection_unix_user(BusName::from(own_unique_name.as_ref()))
        .map_err(transport_error)?;
    let listener_uid = match bus.get_connection_unix_user(application_name.clone()) {
        Ok(listener_uid) => listener_uid,
        // Владелец успел исчезнуть между проверками: экземпляр завершился.
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
            return Err(InstanceForwardingError::InstanceNotListening);
        }
        Err(error) => return Err(transport_error(error)),
    };
    if listener_uid == own_uid {
        Ok(())
    } else {
        warn!(
            listener_uid,
            "Имя приложения на шине принадлежит другому пользователю"
        );
        Err(InstanceForwardingError::ForeignInstanceOwner)
    }
}

/// `platform_data` по спецификации: только билет активации, если он есть.
fn platform_data_for(request: &ForwardedInstanceRequest) -> HashMap<&str, Value<'_>> {
    request
        .activation_token()
        .map(|token| (ACTIVATION_TOKEN_PLATFORM_KEY, Value::from(token.as_str())))
        .into_iter()
        .collect()
}

/// Переводит ошибку вызова в типизированную причину для пользователя.
fn classify_call_error(error: zbus::Error) -> InstanceForwardingError {
    match &error {
        zbus::Error::InputOutput(io_error) if io_error.kind() == std::io::ErrorKind::TimedOut => {
            InstanceForwardingError::InstanceNotResponding
        }
        zbus::Error::MethodError(name, detail, _reply) => match name.as_str() {
            REJECTED_ERROR_NAME => InstanceForwardingError::Rejected {
                detail: detail.clone().unwrap_or_default(),
            },
            NOT_RESPONDING_ERROR_NAME => InstanceForwardingError::InstanceNotResponding,
            SHUTTING_DOWN_ERROR_NAME | NO_REPLY_ERROR_NAME => {
                InstanceForwardingError::InstanceShuttingDown
            }
            NAME_HAS_NO_OWNER_ERROR_NAME | SERVICE_UNKNOWN_ERROR_NAME => {
                InstanceForwardingError::InstanceNotListening
            }
            _ => transport_error(error),
        },
        _ => transport_error(error),
    }
}

fn transport_error(error: impl Into<zbus::Error>) -> InstanceForwardingError {
    InstanceForwardingError::Transport(error.into().to_string())
}
