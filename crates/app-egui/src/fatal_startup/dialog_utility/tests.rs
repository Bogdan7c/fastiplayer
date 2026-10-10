use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
///
/// Скрипт создаёт отдельный короткоживущий `sh`, а не сам тестовый процесс. Если
/// записать файл через `fs::write`, то дескриптор на запись существует в нашем
/// процессе, и любой `fork` другого теста в этот момент наследует его до своего
/// `exec`. Тогда наш `exec` скрипта падает с `ETXTBSY` («Text file busy»).
/// Когда файл пишет дочерний `sh`, у тестового процесса такого дескриптора нет
/// вообще, а к моменту `wait()` писатель уже закрыт: гонка невозможна.
fn fake_dialog_program(directory: &Path, exit_code: i32) -> (PathBuf, PathBuf) {
    let arguments_log = directory.join("arguments.log");
    let program = directory.join("fake-dialog");
    let script = format!(
        "#!/bin/sh\nfor argument in \"$@\"; do printf '%s\\0' \"$argument\" >> '{}'; done\nexit {exit_code}\n",
        arguments_log.display()
    );
    write_executable_script(&program, &script);
    (program, arguments_log)
}

/// Создаёт исполняемый файл с содержимым `script` руками внешнего `sh` (см. причину
/// в документации `fake_dialog_program`) и дожидается его завершения.
fn write_executable_script(program: &Path, script: &str) {
    let mut writer = Command::new("sh")
        // Путь передаётся позиционным аргументом, а не вклеивается в команду.
        .args(["-c", "cat > \"$1\" && chmod 700 \"$1\"", "sh"])
        .arg(program)
        .stdin(Stdio::piped())
        .spawn()
        .expect("sh writer spawn");
    writer
        .stdin
        .take()
        .expect("sh writer stdin")
        .write_all(script.as_bytes())
        .expect("script into sh writer");
    let status = writer.wait().expect("sh writer wait");
    assert!(status.success(), "sh writer failed: {status:?}");
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
