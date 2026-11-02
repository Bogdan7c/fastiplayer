//! Настоящая реализация `flatpak` на скриптах-заглушках: аргументы, таймаут, ошибки.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::support::write_executable;
use crate::{FlatpakError, FlatpakTool, RealFlatpakTool};

const GENEROUS_TIMEOUT: Duration = Duration::from_secs(20);

/// Тесты процессов идут по одному. Иначе `fork` соседнего теста успевает унаследовать
/// открытый на запись дескриптор свежего скрипта, и `exec` падает с `ETXTBSY`.
static PROCESS_TESTS: Mutex<()> = Mutex::new(());

/// Скрипт-заглушка `flatpak` в отдельном каталоге; охранник держит очередь тестов.
fn fake_flatpak(directory: &Path, body: &str) -> (MutexGuard<'static, ()>, PathBuf) {
    let serial = PROCESS_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
    let script = directory.join("fake-flatpak");
    write_executable(&script, format!("#!/bin/sh\n{body}\n").as_bytes());
    (serial, script)
}

#[test]
fn list_output_becomes_app_ids_without_blank_lines() {
    let directory = tempfile::tempdir().expect("dir");
    let (_serial, script) = fake_flatpak(
        directory.path(),
        "printf 'com.google.Chrome\\n\\n  org.mozilla.firefox  \\n'",
    );
    let tool = RealFlatpakTool::with_program(script, GENEROUS_TIMEOUT);
    assert_eq!(
        tool.installed_apps().expect("list"),
        vec![
            "com.google.Chrome".to_owned(),
            "org.mozilla.firefox".to_owned()
        ]
    );
}

#[test]
fn list_arguments_ask_for_applications_only_and_one_column() {
    let directory = tempfile::tempdir().expect("dir");
    let log = directory.path().join("args.log");
    let (_serial, script) = fake_flatpak(
        directory.path(),
        &format!("printf '%s\\n' \"$@\" > '{}'", log.display()),
    );
    let tool = RealFlatpakTool::with_program(script, GENEROUS_TIMEOUT);
    tool.installed_apps().expect("list");
    assert_eq!(
        std::fs::read_to_string(log).expect("log"),
        "list\n--app\n--columns=application\n"
    );
}

#[test]
fn missing_flatpak_program_means_an_empty_list_but_not_available_for_permissions() {
    let tool = RealFlatpakTool::with_program("/nonexistent/flatpak", GENEROUS_TIMEOUT);
    assert_eq!(tool.installed_apps(), Ok(Vec::new()));
    assert_eq!(
        tool.permissions("com.google.Chrome"),
        Err(FlatpakError::NotAvailable)
    );
}

#[test]
fn permissions_returns_stdout_and_asks_for_the_exact_app() {
    let directory = tempfile::tempdir().expect("dir");
    let log = directory.path().join("args.log");
    let (_serial, script) = fake_flatpak(
        directory.path(),
        &format!(
            "printf '%s\\n' \"$@\" > '{}'; printf '[Context]\\nsockets=session-bus;\\n'",
            log.display()
        ),
    );
    let tool = RealFlatpakTool::with_program(script, GENEROUS_TIMEOUT);
    assert_eq!(
        tool.permissions("com.google.Chrome").expect("permissions"),
        "[Context]\nsockets=session-bus;\n"
    );
    assert_eq!(
        std::fs::read_to_string(log).expect("log"),
        "info\n--show-permissions\ncom.google.Chrome\n"
    );
}

#[test]
fn grant_runs_the_user_override_with_the_application_mask() {
    let directory = tempfile::tempdir().expect("dir");
    let log = directory.path().join("args.log");
    let (_serial, script) = fake_flatpak(
        directory.path(),
        &format!("printf '%s\\n' \"$@\" > '{}'", log.display()),
    );
    let tool = RealFlatpakTool::with_program(script, GENEROUS_TIMEOUT);
    tool.grant_bridge_bus_access("com.google.Chrome")
        .expect("grant");
    assert_eq!(
        std::fs::read_to_string(log).expect("log"),
        "override\n--user\n--talk-name=io.github.Bogdan7c.Fastiplayer.*\ncom.google.Chrome\n"
    );
}

#[test]
fn failing_command_reports_exit_code_and_stderr() {
    let directory = tempfile::tempdir().expect("dir");
    let (_serial, script) =
        fake_flatpak(directory.path(), "echo 'error: not installed' >&2; exit 3");
    let tool = RealFlatpakTool::with_program(script, GENEROUS_TIMEOUT);
    assert_eq!(
        tool.permissions("x.y.Z"),
        Err(FlatpakError::Failed {
            exit_code: Some(3),
            stderr: "error: not installed".into()
        })
    );
}

#[test]
fn hanging_command_is_stopped_at_the_timeout() {
    let directory = tempfile::tempdir().expect("dir");
    let (_serial, script) = fake_flatpak(directory.path(), "exec sleep 30");
    let tool = RealFlatpakTool::with_program(script, Duration::from_millis(300));
    let started_at = Instant::now();
    assert_eq!(
        tool.permissions("com.google.Chrome"),
        Err(FlatpakError::TimedOut)
    );
    assert!(
        started_at.elapsed() < Duration::from_secs(10),
        "must not wait for the process"
    );
}

#[test]
fn large_output_does_not_deadlock_the_pipe() {
    let directory = tempfile::tempdir().expect("dir");
    let (_serial, script) =
        fake_flatpak(directory.path(), "head -c 300000 /dev/zero | tr '\\0' 'a'");
    let tool = RealFlatpakTool::with_program(script, GENEROUS_TIMEOUT);
    assert_eq!(
        tool.permissions("com.google.Chrome").expect("output").len(),
        300_000
    );
}

#[test]
fn background_child_holding_stdout_cannot_hang_the_call() {
    let directory = tempfile::tempdir().expect("dir");
    // Потомок наследует stdout и живёт дольше самой команды.
    let (_serial, script) = fake_flatpak(directory.path(), "sleep 6 &\necho started");
    let tool = RealFlatpakTool::with_program(script, Duration::from_millis(500));
    let started_at = Instant::now();

    let result = tool.permissions("com.google.Chrome");

    assert_eq!(result, Err(FlatpakError::TimedOut));
    assert!(
        started_at.elapsed() < Duration::from_secs(4),
        "{:?}",
        started_at.elapsed()
    );
}
