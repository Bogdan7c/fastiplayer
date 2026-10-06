use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{self, Write};
use std::rc::Rc;

use desktop_integration::DesktopNotificationError;

use super::{
    FatalNoticeChannel, NoticeChannelFailure, NoticeChannelKind, NoticeDelivery, deliver_notice,
    notice_channel_order,
};
use crate::fatal_startup::FatalStartupError;
use crate::fatal_startup::dialog_utility::DialogUtility;
use crate::fatal_startup::test_support::{RecordingChannel, ShownNotice};

fn graphics_error() -> FatalStartupError {
    FatalStartupError::graphics_unavailable(&anyhow::anyhow!("no adapter"))
}

fn channel(
    name: &'static str,
    outcome: Result<(), NoticeChannelFailure>,
    attempts: &Rc<RefCell<Vec<ShownNotice>>>,
) -> Box<dyn FatalNoticeChannel> {
    Box::new(RecordingChannel {
        name,
        outcome,
        attempts: attempts.clone(),
    })
}

fn attempted_channels(attempts: &Rc<RefCell<Vec<ShownNotice>>>) -> Vec<&'static str> {
    attempts
        .borrow()
        .iter()
        .map(|shown| shown.channel)
        .collect()
}

#[test]
fn first_working_channel_shows_text_and_later_channels_are_not_tried() {
    let attempts = Rc::new(RefCell::new(Vec::new()));
    let channels = [
        channel(
            "kdialog",
            Err(NoticeChannelFailure::ProgramMissing),
            &attempts,
        ),
        channel("zenity", Ok(()), &attempts),
        channel("desktop-notification", Ok(()), &attempts),
    ];
    let mut text_output = Vec::new();

    let delivery = deliver_notice(&graphics_error(), &channels, &mut text_output);

    assert_eq!(delivery, NoticeDelivery::ShownBy("zenity"));
    assert_eq!(attempted_channels(&attempts), ["kdialog", "zenity"]);
    assert!(attempts.borrow()[1].notice.message.contains("Vulkan"));
}

#[test]
fn notification_is_used_when_no_dialog_utility_works() {
    let attempts = Rc::new(RefCell::new(Vec::new()));
    let channels = [
        channel(
            "zenity",
            Err(NoticeChannelFailure::ProgramMissing),
            &attempts,
        ),
        channel(
            "kdialog",
            Err(NoticeChannelFailure::ClosedUnsuccessfully { exit_code: Some(1) }),
            &attempts,
        ),
        channel("desktop-notification", Ok(()), &attempts),
    ];

    let delivery = deliver_notice(&graphics_error(), &channels, &mut Vec::new());

    assert_eq!(delivery, NoticeDelivery::ShownBy("desktop-notification"));
    assert_eq!(
        attempted_channels(&attempts),
        ["zenity", "kdialog", "desktop-notification"]
    );
}

#[test]
fn text_reaches_stderr_even_when_every_graphical_channel_fails() {
    let attempts = Rc::new(RefCell::new(Vec::new()));
    let channels = [
        channel(
            "zenity",
            Err(NoticeChannelFailure::ProgramMissing),
            &attempts,
        ),
        channel(
            "desktop-notification",
            Err(NoticeChannelFailure::DesktopNotificationFailed(
                DesktopNotificationError::SessionBusUnavailable("no bus".to_owned()),
            )),
            &attempts,
        ),
    ];
    let mut text_output = Vec::new();

    let delivery = deliver_notice(&graphics_error(), &channels, &mut text_output);

    let printed = String::from_utf8(text_output).expect("utf-8 text");
    assert_eq!(delivery, NoticeDelivery::TextOutputOnly);
    assert!(printed.starts_with("Fastiplayer не запустился.\n"));
    assert!(printed.contains("видеокарта с поддержкой Vulkan"));
    // Техническая деталь — только в лог, не пользователю.
    assert!(!printed.contains("no adapter"));
}

/// Писатель, который всегда отказывает (закрытый stderr).
struct BrokenOutput;

impl Write for BrokenOutput {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

#[test]
fn broken_stderr_does_not_stop_graphical_presentation() {
    let attempts = Rc::new(RefCell::new(Vec::new()));
    let channels = [channel("kdialog", Ok(()), &attempts)];

    let delivery = deliver_notice(&graphics_error(), &channels, &mut BrokenOutput);

    assert_eq!(delivery, NoticeDelivery::ShownBy("kdialog"));
}

#[test]
fn kde_prefers_kdialog_and_other_desktops_prefer_zenity() {
    let kde_first = [
        NoticeChannelKind::Dialog(DialogUtility::Kdialog),
        NoticeChannelKind::Dialog(DialogUtility::Zenity),
        NoticeChannelKind::DesktopNotification,
    ];
    let gtk_first = [
        NoticeChannelKind::Dialog(DialogUtility::Zenity),
        NoticeChannelKind::Dialog(DialogUtility::Kdialog),
        NoticeChannelKind::DesktopNotification,
    ];

    assert_eq!(notice_channel_order(Some(OsStr::new("KDE"))), kde_first);
    assert_eq!(notice_channel_order(Some(OsStr::new("foo:kde"))), kde_first);
    assert_eq!(
        notice_channel_order(Some(OsStr::new("ubuntu:GNOME"))),
        gtk_first
    );
    // «KDEnot» — другое окружение, подстрока не должна считаться KDE.
    assert_eq!(notice_channel_order(Some(OsStr::new("KDEnot"))), gtk_first);
    assert_eq!(notice_channel_order(None), gtk_first);
}
