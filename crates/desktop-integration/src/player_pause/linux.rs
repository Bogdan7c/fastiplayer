//! Linux-реализация паузы: один вызов `Pause` на фиксированное MPRIS-имя.

use tracing::{info, warn};
use zbus::blocking::{Connection, connection};

use super::{PlayerPauseConfig, PlayerPauseError, PlayerPauseOutcome};
use crate::mpris_names::{MPRIS_BUS_NAME, MPRIS_OBJECT_PATH, MPRIS_PLAYER_INTERFACE};
use crate::peer_admission::{BusQueryError, blocking_name_has_owner, blocking_unix_user};

/// Стандартная ошибка шины: у имени нет владельца.
const NAME_HAS_NO_OWNER_ERROR_NAME: &str = "org.freedesktop.DBus.Error.NameHasNoOwner";
/// Стандартная ошибка шины: имени нет и запустить его нечем.
const SERVICE_UNKNOWN_ERROR_NAME: &str = "org.freedesktop.DBus.Error.ServiceUnknown";

/// Звонит через сессионную шину пользователя.
pub(crate) fn pause_on_session_bus(
    config: PlayerPauseConfig,
) -> Result<PlayerPauseOutcome, PlayerPauseError> {
    pause_with(|| {
        connection::Builder::session()?
            .method_timeout(config.call_timeout)
            .build()
    })
}

/// Звонит через шину, которую открывает `connect` (тесты — частная шина).
pub(super) fn pause_with(
    connect: impl FnOnce() -> zbus::Result<Connection>,
) -> Result<PlayerPauseOutcome, PlayerPauseError> {
    let connection =
        connect().map_err(|error| PlayerPauseError::SessionBusUnavailable(error.to_string()))?;

    // `NameHasOwner` не запускает службы: «нет владельца» значит «плеер не запущен».
    if !blocking_name_has_owner(&connection, MPRIS_BUS_NAME).map_err(bus_query_error)? {
        info!("Regular player is not running; nothing to pause");
        return Ok(PlayerPauseOutcome::PlayerNotRunning);
    }
    ensure_player_belongs_to_same_user(&connection)?;

    // Низкоуровневый `call_method`: только он соблюдает `method_timeout` (zbus 5.15).
    // Автозапуска не будет: `.service`-файла для MPRIS-имени у приложения нет, а если
    // владелец исчез между проверкой и вызовом, шина ответит `ServiceUnknown`.
    match connection.call_method(
        Some(MPRIS_BUS_NAME),
        MPRIS_OBJECT_PATH,
        Some(MPRIS_PLAYER_INTERFACE),
        "Pause",
        &(),
    ) {
        Ok(_reply) => {
            info!("Regular player was asked to pause");
            Ok(PlayerPauseOutcome::PauseRequested)
        }
        Err(error) => classify_pause_error(error),
    }
}

/// Отказывается слать команду процессу другого пользователя.
fn ensure_player_belongs_to_same_user(connection: &Connection) -> Result<(), PlayerPauseError> {
    let own_unique_name = connection.unique_name().ok_or_else(|| {
        PlayerPauseError::Transport("The connection has no unique name".to_owned())
    })?;
    let own_uid =
        blocking_unix_user(connection, own_unique_name.as_str()).map_err(bus_query_error)?;
    match blocking_unix_user(connection, MPRIS_BUS_NAME) {
        Ok(player_uid) if player_uid == own_uid => Ok(()),
        Ok(player_uid) => {
            warn!(
                player_uid,
                "The MPRIS name of the player belongs to another user"
            );
            Err(PlayerPauseError::ForeignOwner)
        }
        // Владелец исчез между проверками: плеер как раз завершился.
        Err(BusQueryError::NoOwner) => Ok(()),
        Err(error) => Err(bus_query_error(error)),
    }
}

fn classify_pause_error(error: zbus::Error) -> Result<PlayerPauseOutcome, PlayerPauseError> {
    match &error {
        zbus::Error::InputOutput(io_error) if io_error.kind() == std::io::ErrorKind::TimedOut => {
            Err(PlayerPauseError::NotResponding)
        }
        zbus::Error::MethodError(name, _detail, _reply)
            if matches!(
                name.as_str(),
                NAME_HAS_NO_OWNER_ERROR_NAME | SERVICE_UNKNOWN_ERROR_NAME
            ) =>
        {
            Ok(PlayerPauseOutcome::PlayerNotRunning)
        }
        _ => Err(transport_error(error)),
    }
}

/// Таймаут запроса к шине — это «не отвечает», остальное — транспорт.
fn bus_query_error(error: BusQueryError) -> PlayerPauseError {
    match error {
        BusQueryError::TimedOut => PlayerPauseError::NotResponding,
        other => PlayerPauseError::Transport(other.to_string()),
    }
}

fn transport_error(error: impl std::fmt::Display) -> PlayerPauseError {
    PlayerPauseError::Transport(error.to_string())
}

#[cfg(test)]
mod tests;
