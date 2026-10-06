//! Запасной канал показа: критическое уведомление рабочего стола.
//!
//! Используется, когда ни одна утилита окна ошибки не сработала. Доставку по D-Bus
//! выполняет `desktop-integration`; здесь только адаптер к цепочке каналов.

use desktop_integration::{CriticalDesktopNotification, send_critical_desktop_notification};

use super::messages::FatalStartupNotice;
use super::presentation::{FatalNoticeChannel, NoticeChannelFailure};

/// Канал «уведомление рабочего стола».
pub(super) struct DesktopNotificationChannel;

impl FatalNoticeChannel for DesktopNotificationChannel {
    fn channel_name(&self) -> &'static str {
        "desktop-notification"
    }

    fn try_show(&self, notice: &FatalStartupNotice) -> Result<(), NoticeChannelFailure> {
        send_critical_desktop_notification(CriticalDesktopNotification {
            title: notice.title,
            body: &notice.message,
        })
        .map_err(NoticeChannelFailure::DesktopNotificationFailed)
    }
}
