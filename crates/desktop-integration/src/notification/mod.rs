//! Критическое уведомление рабочего стола без окна приложения.
//!
//! Нужен для случаев, когда приложение не смогло открыть собственное окно
//! (нет GPU, нет графического окна) и должно хоть как-то сообщить пользователю о
//! фатальной ошибке. Модуль владеет только доставкой текста в службу уведомлений;
//! что и когда показывать, решает вызывающий composition root.

#[cfg(target_os = "linux")]
mod linux;

use thiserror::Error;

/// Текст критического уведомления: заголовок и основной текст без разметки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CriticalDesktopNotification<'text> {
    /// Короткий заголовок, который служба уведомлений показывает жирным.
    pub title: &'text str,
    /// Основной текст; разметка экранируется, переводы строк сохраняются.
    pub body: &'text str,
}

/// Почему уведомление не было доставлено.
///
/// Варианты различают «шины нет вообще» и «шина есть, но служба отказала»:
/// вызывающему коду это нужно для понятной записи в лог.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DesktopNotificationError {
    /// Не удалось подключиться к сессионной шине D-Bus (нет графического сеанса).
    #[error("сессионная шина D-Bus недоступна: {0}")]
    SessionBusUnavailable(String),

    /// Шина доступна, но службы уведомлений нет или она отклонила вызов.
    #[error("служба уведомлений не приняла уведомление: {0}")]
    NotificationServiceRejected(String),

    /// На этой платформе доставка уведомлений не реализована.
    #[error("уведомления рабочего стола не поддерживаются на этой платформе")]
    UnsupportedPlatform,
}

/// Отправляет критическое уведомление, которое не исчезает само по таймауту.
///
/// Вызов синхронный и не требует async runtime: его можно делать из `main`
/// до создания окна или уже после остановки event loop.
pub fn send_critical_desktop_notification(
    notification: CriticalDesktopNotification<'_>,
) -> Result<(), DesktopNotificationError> {
    #[cfg(target_os = "linux")]
    {
        linux::send_on_session_bus(notification)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = notification;
        Err(DesktopNotificationError::UnsupportedPlatform)
    }
}
