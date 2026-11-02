//! Настоящий запуск браузера на скрипте-заглушке: аргументы, скорость, зомби.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::flatpak_command::PROCESS_TESTS;
use super::support::write_executable;
use crate::{BrowserLaunchCommand, BrowserLauncher, LaunchError, RealBrowserLauncher};

const URL: &str = "https://example.org/page?x=1&y=2";

/// Заглушка: пишет pid и аргументы, затем спит `sleep_seconds`.
fn stub(directory: &Path, sleep_seconds: u32) -> (PathBuf, PathBuf, PathBuf) {
    let pid_file = directory.join("pid");
    let args_file = directory.join("args");
    let script = directory.join("stub");
    let body = format!(
        "#!/bin/sh\necho $$ > '{pid}'\nprintf '%s\\n' \"$@\" > '{args}.tmp'\nmv '{args}.tmp' '{args}'\nsleep {sleep_seconds}\n",
        pid = pid_file.display(),
        args = args_file.display(),
    );
    write_executable(&script, body.as_bytes());
    (script, pid_file, args_file)
}

fn wait_for_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{} did not appear", path.display());
}

#[test]
fn executable_is_started_with_exactly_the_url_and_the_call_does_not_wait_for_it() {
    let _serial = PROCESS_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = tempfile::tempdir().expect("dir");
    let (script, _pid_file, args_file) = stub(directory.path(), 3);
    let started = Instant::now();
    RealBrowserLauncher::new()
        .open_url(&BrowserLaunchCommand::Executable(script), URL)
        .expect("launch");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "open_url must not wait for the browser"
    );
    assert_eq!(wait_for_file(&args_file), format!("{URL}\n"));
}

#[test]
fn flatpak_is_started_as_run_app_id_url() {
    let _serial = PROCESS_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = tempfile::tempdir().expect("dir");
    let (script, _pid_file, args_file) = stub(directory.path(), 0);
    RealBrowserLauncher::with_flatpak_program(script)
        .open_url(
            &BrowserLaunchCommand::Flatpak {
                app_id: "com.google.Chrome".to_owned(),
            },
            URL,
        )
        .expect("launch");
    assert_eq!(
        wait_for_file(&args_file),
        format!("run\ncom.google.Chrome\n{URL}\n")
    );
}

#[test]
fn a_finished_browser_process_is_reaped_and_leaves_no_zombie() {
    let _serial = PROCESS_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = tempfile::tempdir().expect("dir");
    let (script, pid_file, args_file) = stub(directory.path(), 0);
    RealBrowserLauncher::new()
        .open_url(&BrowserLaunchCommand::Executable(script), URL)
        .expect("launch");
    wait_for_file(&args_file);
    let pid = wait_for_file(&pid_file).trim().to_owned();
    let process_dir = PathBuf::from("/proc").join(&pid);
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_dir.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !process_dir.exists(),
        "process {pid} is still in /proc: a zombie"
    );
}

#[test]
fn missing_program_is_a_typed_error() {
    let launcher = RealBrowserLauncher::with_flatpak_program("/nonexistent/flatpak");
    assert_eq!(
        launcher.open_url(
            &BrowserLaunchCommand::Executable("/nonexistent/browser".into()),
            URL
        ),
        Err(LaunchError::ProgramNotFound)
    );
    assert_eq!(
        launcher.open_url(
            &BrowserLaunchCommand::Flatpak {
                app_id: "a.b".to_owned()
            },
            URL
        ),
        Err(LaunchError::ProgramNotFound)
    );
}
