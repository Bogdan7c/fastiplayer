//! Linux-доставка уведомления через freedesktop Notifications по сессионной D-Bus.
//!
//! Используется блокирующий API zbus: вызывающий код находится вне event loop
//! (в `main` до создания окна или после его остановки) и async runtime не имеет.

use std::collections::HashMap;
use std::time::Duration;

use zbus::blocking::{Connection, connection};
use zbus::zvariant::Value;

use super::{CriticalDesktopNotification, DesktopNotificationError};

/// Имя, путь и интерфейс службы из спецификации Desktop Notifications.
const NOTIFICATIONS_SERVICE: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFICATIONS_INTERFACE: &str = "org.freedesktop.Notifications";
const NOTIFY_METHOD: &str = "Notify";

/// Имя приложения, которое служба показывает рядом с уведомлением.
const APPLICATION_NAME: &str = "Fastiplayer";
/// Стандартная иконка ошибки из freedesktop Icon Naming Specification.
const ERROR_ICON_NAME: &str = "dialog-error";
/// `replaces_id = 0` по спецификации означает «новое уведомление, ничего не заменять».
const NEW_NOTIFICATION_ID: u32 = 0;
/// Hint `urgency = 2` — critical: KDE и GNOME не прячут такие уведомления по таймеру.
const CRITICAL_URGENCY: u8 = 2;
/// `expire_timeout = 0` по спецификации означает «не скрывать автоматически».
const NEVER_EXPIRE: i32 = 0;
/// Сколько ждать ответа службы. Без лимита зависшая служба уведомлений повесила бы
/// и завершение процесса, хотя текст ошибки к этому моменту уже выведен в stderr.
const NOTIFY_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Подключается к сессионной шине пользователя и отправляет уведомление.
pub(super) fn send_on_session_bus(
    notification: CriticalDesktopNotification<'_>,
) -> Result<(), DesktopNotificationError> {
    send_via(connect_to_session_bus, notification)
}

/// Сессионная шина с ограниченным ожиданием ответа на вызовы методов.
fn connect_to_session_bus() -> zbus::Result<Connection> {
    connection::Builder::session()?
        .method_timeout(NOTIFY_REPLY_TIMEOUT)
        .build()
}

/// Отправляет уведомление по шине, которую открывает `connect`.
///
/// Подключение передаётся снаружи, чтобы тесты могли подставить частную шину
/// с поддельной службой уведомлений вместо настоящего рабочего стола.
fn send_via(
    connect: impl FnOnce() -> zbus::Result<Connection>,
    notification: CriticalDesktopNotification<'_>,
) -> Result<(), DesktopNotificationError> {
    let connection = connect()
        .map_err(|error| DesktopNotificationError::SessionBusUnavailable(error.to_string()))?;

    // Спецификация разрешает службе трактовать body как разметку, поэтому `<`, `>`
    // и `&` экранируются. Заголовок (summary) по спецификации всегда простой текст.
    let escaped_body = escape_notification_markup(notification.body);
    let hints = HashMap::from([("urgency", Value::U8(CRITICAL_URGENCY))]);
    // Кнопки действий не нужны: уведомление только сообщает, ответ не ждём.
    let no_actions: &[&str] = &[];

    connection
        .call_method(
            Some(NOTIFICATIONS_SERVICE),
            NOTIFICATIONS_OBJECT_PATH,
            Some(NOTIFICATIONS_INTERFACE),
            NOTIFY_METHOD,
            &(
                APPLICATION_NAME,
                NEW_NOTIFICATION_ID,
                ERROR_ICON_NAME,
                notification.title,
                escaped_body.as_str(),
                no_actions,
                hints,
                NEVER_EXPIRE,
            ),
        )
        .map_err(|error| {
            DesktopNotificationError::NotificationServiceRejected(error.to_string())
        })?;

    Ok(())
}

/// Экранирует три символа разметки, которые служба могла бы принять за тег.
fn escape_notification_markup(plain_text: &str) -> String {
    let mut escaped = String::with_capacity(plain_text.len());
    for character in plain_text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests;
