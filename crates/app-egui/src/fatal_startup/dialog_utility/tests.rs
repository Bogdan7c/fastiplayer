use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{DialogUtility, DialogUtilityChannel};
use crate::fatal_startup::messages::FatalStartupNotice;
use crate::fatal_startup::presentation::{FatalNoticeChannel, NoticeChannelFailure};

fn notice() -> FatalStartupNotice {
    FatalStartupNotice {
        title: "Fastiplayer не запустился",
        message: "Нет Vulkan <драйвера> & окна\nвторая строка".to_owned(),
    }
}

/// Поддельная утилита: записывает свои аргументы (по одному на строку) и
/// завершается с заданным кодом.
fn fake_dialog_program(directory: &Path, exit_code: i32) -> (PathBuf, PathBuf) {
    let arguments_log = directory.join("arguments.log");
    let program = directory.join("fake-dialog");
    let script = format!(
        "#!/bin/sh\nfor argument in \"$@\"; do printf '%s\\0' \"$argument\" >> '{}'; done\nexit {exit_code}\n",
        arguments_log.display()
    );
    fs::write(&program, script).expect("fake dialog script");
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).expect("fake dialog mode");
    (program, arguments_log)
}

fn recorded_arguments(arguments_log: &Path) -> Vec<String> {
    let recorded = fs::read_to_string(arguments_log).expect("arguments log");
    recorded
        .split('\0')
        .filter(|argument| !argument.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn kdialog_receives_title_and_error_text_as_separate_arguments() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (program, arguments_log) = fake_dialog_program(directory.path(), 0);
    let channel = DialogUtilityChannel::with_program(DialogUtility::Kdialog, program);

    let outcome = channel.try_show(&notice());

    assert_eq!(outcome, Ok(()));
    assert_eq!(
        recorded_arguments(&arguments_log),
        [
            "--title",
            "Fastiplayer не запустился",
            "--error",
            "Нет Vulkan <драйвера> & окна\nвторая строка",
        ]
    );
}

#[test]
fn zenity_disables_markup_so_text_is_shown_verbatim() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (program, arguments_log) = fake_dialog_program(directory.path(), 0);
    let channel = DialogUtilityChannel::with_program(DialogUtility::Zenity, program);

    assert_eq!(channel.try_show(&notice()), Ok(()));
    assert_eq!(
        recorded_arguments(&arguments_log),
        [
            "--error",
            "--no-markup",
            "--title",
            "Fastiplayer не запустился",
            "--text",
            "Нет Vulkan <драйвера> & окна\nвторая строка",
        ]
    );
}

#[test]
fn unsuccessful_dialog_exit_is_not_counted_as_shown() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (program, _) = fake_dialog_program(directory.path(), 1);
    let channel = DialogUtilityChannel::with_program(DialogUtility::Zenity, program);

    assert_eq!(
        channel.try_show(&notice()),
        Err(NoticeChannelFailure::ClosedUnsuccessfully { exit_code: Some(1) })
    );
}

#[test]
fn missing_dialog_program_is_reported_as_missing() {
    let directory = tempfile::tempdir().expect("tempdir");
    let channel = DialogUtilityChannel::with_program(
        DialogUtility::Kdialog,
        directory.path().join("not-installed-dialog"),
    );

    assert_eq!(
        channel.try_show(&notice()),
        Err(NoticeChannelFailure::ProgramMissing)
    );
}

#[test]
fn production_channels_look_up_real_utility_names() {
    assert_eq!(
        DialogUtilityChannel::from_path(DialogUtility::Kdialog).channel_name(),
        "kdialog"
    );
    assert_eq!(
        DialogUtilityChannel::from_path(DialogUtility::Zenity).program,
        "zenity"
    );
}
