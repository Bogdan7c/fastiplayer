//! Закрытый stderr не роняет процесс (03e).
//!
//! Процесс браузерного режима запускается D-Bus-активацией, и его stderr — труба
//! активатора. Когда читатель исчезает (выход из сеанса, упала шина), запись журнала
//! даёт `BrokenPipe`; раньше слой журнала отвечал на это `eprintln!`, паниковал, а
//! паника в деструкторе во время раскрутки кончалась `abort` (coredump-ы в 03e).
//!
//! Тест запускает настоящий бинарь с уже закрытым читающим концом stderr и с шиной,
//! которой нет: процесс должен честно выйти своим кодом отказа, а не паникой (101)
//! и не сигналом.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Сколько ждать выхода процесса.
const WAIT: Duration = Duration::from_secs(30);
/// Код выхода паники Rust.
const RUST_PANIC_EXIT_CODE: i32 = 101;

/// Ждёт выхода процесса не дольше `WAIT`.
fn wait_with_deadline(child: &mut std::process::Child) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().expect("child status") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("the process did not exit in {WAIT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_closed_stderr_does_not_crash_the_browser_session_process() {
    let home =
        std::env::temp_dir().join(format!("fastiplayer-closed-stderr-{}", std::process::id()));
    let xdg = |name: &str| {
        let path = home.join(name);
        std::fs::create_dir_all(&path).expect("XDG directory");
        path
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_fastiplayer"));
    command
        .arg("--browser-session")
        // Шины нет: процесс пишет в журнал и выходит с отказом.
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", home.join("no-bus").display()),
        )
        .env("XDG_CONFIG_HOME", xdg("config"))
        .env("XDG_DATA_HOME", xdg("data"))
        .env("XDG_STATE_HOME", xdg("state"))
        .env("XDG_CACHE_HOME", xdg("cache"))
        .env("XDG_RUNTIME_DIR", xdg("runtime"))
        .env("RUST_LOG", "info")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("the binary starts");
    // Читатель stderr исчезает до первой строки журнала.
    drop(child.stderr.take());

    let status = wait_with_deadline(&mut child);
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(status.signal(), None, "killed by a signal: {status:?}");
    assert_ne!(
        status.code(),
        Some(RUST_PANIC_EXIT_CODE),
        "the process panicked: {status:?}"
    );
    assert!(
        status.code().is_some_and(|code| code != 0),
        "without a bus the process must report a failure: {status:?}"
    );
}
