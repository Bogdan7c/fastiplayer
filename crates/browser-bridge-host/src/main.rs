//! `fastiplayer-browser-host` — посредник native messaging моста в браузер.
//!
//! Его запускает браузер по `runtime.connectNative()` (файл подключения ставит
//! установщик, сессия 06). Вся логика — в библиотеке `browser_bridge_host`; здесь
//! только настоящие stdin, stdout, аргументы и дверь на сессионной шине.

use std::ffi::OsString;
use std::process::ExitCode;

use browser_bridge_host::door::EntryClientDoor;
use browser_bridge_host::host::run_host;
use browser_bridge_host::log::StderrLog;

fn main() -> ExitCode {
    // Нулевой аргумент — путь к бинарю; кто позвал, говорят следующие.
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let exit = run_host(
        &arguments,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        EntryClientDoor::session_bus,
        &mut StderrLog,
    );
    ExitCode::from(exit.exit_code())
}
