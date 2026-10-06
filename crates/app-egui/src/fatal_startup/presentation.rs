//! Показ фатальной ошибки запуска пользователю.
//!
//! Решение владельца (UX06): сначала окно ошибки через утилиту рабочего стола
//! (kdialog на KDE, zenity в остальных окружениях), если не вышло — критическое
//! уведомление. Текст всегда дублируется в stderr, а техническая деталь — в лог.
//! Окно показывается и при запуске из терминала: поведение одинаково при любом
//! способе запуска.

use std::ffi::OsStr;
use std::io::Write;

use desktop_integration::DesktopNotificationError;

use super::FatalStartupError;
use super::desktop_notification_channel::DesktopNotificationChannel;
use super::dialog_utility::{DialogUtility, DialogUtilityChannel};
use super::messages::{FatalStartupNotice, notice_for};

/// Кто показывает фатальную ошибку. В production — `SystemFatalStartupPresenter`,
/// в тестах — fake, записывающий показанные ошибки.
pub(crate) trait FatalStartupPresenter {
    /// Показывает ошибку и возвращается, когда показ завершён (окно закрыто
    /// или все каналы исчерпаны).
    fn present(&self, error: &FatalStartupError);
}

/// Один способ показать текст пользователю (окно утилиты, уведомление).
pub(super) trait FatalNoticeChannel {
    /// Короткое имя для лога.
    fn channel_name(&self) -> &'static str;

    /// Пытается показать текст; `Ok` — пользователь его увидел.
    fn try_show(&self, notice: &FatalStartupNotice) -> Result<(), NoticeChannelFailure>;
}

/// Почему канал не показал текст. Пишется в лог, после чего пробуется следующий канал.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NoticeChannelFailure {
    /// Утилиты нет в системе.
    ProgramMissing,
    /// Утилита есть, но не запустилась.
    LaunchFailed(String),
    /// Утилита завершилась с ненулевым кодом (`None` — убита сигналом).
    ClosedUnsuccessfully { exit_code: Option<i32> },
    /// Уведомление не доставлено.
    DesktopNotificationFailed(DesktopNotificationError),
}

/// Чем закончился показ: какой канал сработал или только текстовый вывод.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoticeDelivery {
    /// Текст показан этим каналом (и выведен в stderr).
    ShownBy(&'static str),
    /// Ни один графический канал не сработал; текст есть только в stderr.
    TextOutputOnly,
}

/// Виды графических каналов в порядке попытки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoticeChannelKind {
    Dialog(DialogUtility),
    DesktopNotification,
}

/// Порядок каналов для текущего рабочего стола.
///
/// `XDG_CURRENT_DESKTOP` — список через `:` (например `KDE` или `ubuntu:GNOME`).
/// На KDE первым идёт kdialog (zenity там часто не установлен), в остальных
/// окружениях — zenity. Вторая утилита остаётся запасной, уведомление — последним.
pub(super) fn notice_channel_order(current_desktop: Option<&OsStr>) -> [NoticeChannelKind; 3] {
    let is_kde = current_desktop
        .map(|desktops| {
            desktops
                .to_string_lossy()
                .split(':')
                .any(|desktop| desktop.eq_ignore_ascii_case("KDE"))
        })
        .unwrap_or(false);
    let (first_dialog, second_dialog) = if is_kde {
        (DialogUtility::Kdialog, DialogUtility::Zenity)
    } else {
        (DialogUtility::Zenity, DialogUtility::Kdialog)
    };
    [
        NoticeChannelKind::Dialog(first_dialog),
        NoticeChannelKind::Dialog(second_dialog),
        NoticeChannelKind::DesktopNotification,
    ]
}

/// Пишет ошибку в лог и stderr, затем пробует каналы по порядку до первого успеха.
pub(super) fn deliver_notice(
    error: &FatalStartupError,
    channels: &[Box<dyn FatalNoticeChannel>],
    text_output: &mut dyn Write,
) -> NoticeDelivery {
    tracing::error!(
        reason = ?error.reason(),
        detail = %error.technical_detail(),
        "Фатальная ошибка запуска"
    );

    let notice = notice_for(error.reason());
    write_notice_as_text(&notice, text_output);

    for channel in channels {
        match channel.try_show(&notice) {
            Ok(()) => return NoticeDelivery::ShownBy(channel.channel_name()),
            Err(failure) => tracing::warn!(
                channel = channel.channel_name(),
                ?failure,
                "Канал показа фатальной ошибки не сработал; пробуем следующий"
            ),
        }
    }
    NoticeDelivery::TextOutputOnly
}

/// Текстовая копия для терминала и журнала; ошибка записи только логируется,
/// потому что показ через графические каналы всё равно должен продолжиться.
fn write_notice_as_text(notice: &FatalStartupNotice, text_output: &mut dyn Write) {
    let written = writeln!(text_output, "{}.\n{}", notice.title, notice.message)
        .and_then(|()| text_output.flush());
    if let Err(write_error) = written {
        tracing::warn!(%write_error, "Не удалось вывести текст фатальной ошибки в stderr");
    }
}

/// Production-показ: настоящие утилиты, D-Bus уведомление и stderr процесса.
pub(crate) struct SystemFatalStartupPresenter;

impl FatalStartupPresenter for SystemFatalStartupPresenter {
    fn present(&self, error: &FatalStartupError) {
        let current_desktop = std::env::var_os("XDG_CURRENT_DESKTOP");
        let channels: Vec<Box<dyn FatalNoticeChannel>> =
            notice_channel_order(current_desktop.as_deref())
                .into_iter()
                .map(system_channel)
                .collect();

        let delivery = deliver_notice(error, &channels, &mut std::io::stderr().lock());
        match delivery {
            NoticeDelivery::ShownBy(channel) => {
                tracing::info!(channel, "Фатальная ошибка запуска показана пользователю");
            }
            NoticeDelivery::TextOutputOnly => tracing::warn!(
                "Ни окно ошибки, ни уведомление недоступны; текст ошибки есть только в stderr"
            ),
        }
    }
}

fn system_channel(kind: NoticeChannelKind) -> Box<dyn FatalNoticeChannel> {
    match kind {
        NoticeChannelKind::Dialog(utility) => Box::new(DialogUtilityChannel::from_path(utility)),
        NoticeChannelKind::DesktopNotification => Box::new(DesktopNotificationChannel),
    }
}

#[cfg(test)]
mod tests;
