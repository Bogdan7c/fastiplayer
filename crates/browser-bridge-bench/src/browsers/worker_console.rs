//! Консоль служебного воркера экстеншена в Chromium (16b3c4).
//!
//! Зачем. Фон экстеншена пишет заметки для разработчика в `console.debug("[fastiplayer] …")`:
//! например, почему память выбора не записала смену озвучки на странице. Firefox стенда дублирует
//! консоль в свой stdout (журнал запуска), а Chromium консоль служебного воркера в stderr не пишет.
//! Поэтому стенд сам слушает её по CDP и пишет строки в отдельный журнал запуска
//! `<work>/chromium.worker-console.log` (`BrowserKind::extension_notes_log_path`).
//!
//! Как (документация CDP, домены Target и Runtime):
//! 1. Отдельное соединение с браузерной целью (`/json/version` → `webSocketDebuggerUrl`).
//! 2. `Target.setAutoAttach { autoAttach, flatten, filter: service_worker }` — браузер подключает нас
//!    к уже живым служебным воркерам и к каждому новому (воркер MV3 может перезапуститься).
//! 3. На `Target.attachedToTarget` воркера с адресом `chrome-extension://…` — `Runtime.enable` в его
//!    сеансе; дальше каждый вызов консоли приходит событием `Runtime.consoleAPICalled`.
//!
//! Ограничение: вызовы консоли до подключения (самый старт фона) не повторяются — `Runtime.enable`
//! отдаёт только новые. Заметки сеансов передачи идут намного позже старта и доходят.
//! Подключённый отладчик держит воркер живым; у стенда его и так держит открытый порт посредника.
//!
//! Журнал пишет только этот слушатель (у Chromium свой файл stderr): иначе два писателя с разными
//! смещениями в одном файле затирали бы строки друг друга.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use super::BrowserError;
use super::devtools::{CdpConnection, CdpRead, http_json};
use super::launch_log::set_aside_previous_launch_log;

/// Как часто слушатель просыпается проверить просьбу остановиться, если событий нет.
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(500);
/// Начало адреса служебного воркера экстеншена.
const EXTENSION_WORKER_URL_PREFIX: &str = "chrome-extension://";
/// Метка строки консоли воркера в журнале (по ней строку видно среди будущих других источников).
pub(crate) const WORKER_CONSOLE_LINE_PREFIX: &str = "worker console: ";

/// Слушатель консоли служебного воркера одного запуска Chromium.
pub(crate) struct WorkerConsoleRecorder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerConsoleRecorder {
    /// Подключается к браузерной цели и начинает писать консоль воркеров экстеншена в `log_path`.
    /// Журнал прошлого запуска по тому же пути откладывается, а не затирается.
    pub(crate) fn start(devtools_port: u16, log_path: PathBuf) -> Result<Self, BrowserError> {
        set_aside_previous_launch_log(&log_path).map_err(|source| {
            BrowserError::io(format!("set aside {}", log_path.display()), source)
        })?;
        let log = File::create(&log_path)
            .map_err(|source| BrowserError::io(format!("create {}", log_path.display()), source))?;
        let browser_socket = browser_socket_url(devtools_port)?;
        let mut connection = CdpConnection::connect(&browser_socket)?;
        connection.set_read_timeout(STOP_CHECK_INTERVAL)?;
        // Ответ на эту команду придёт в общем потоке; события подключения — раньше него.
        connection.send_command(
            "Target.setAutoAttach",
            json!({
                "autoAttach": true,
                "waitForDebuggerOnStart": false,
                "flatten": true,
                "filter": [{ "type": "service_worker", "exclude": false }],
            }),
            None,
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("bench-worker-console".to_owned())
            .spawn(move || record_console(connection, BufWriter::new(log), &thread_stop))
            .map_err(|source| BrowserError::io("spawn the worker console thread", source))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Останавливает слушателя (после закрытия браузера): дальше журнал никто не пишет.
    pub(crate) fn finish(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // Паника слушателя не должна ронять закрытие браузера: журнал до паники уже на диске,
            // а в stderr стенда она уже напечатана.
            let _panicked = thread.join();
        }
    }
}

impl Drop for WorkerConsoleRecorder {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// Адрес WebSocket браузерной цели.
fn browser_socket_url(devtools_port: u16) -> Result<String, BrowserError> {
    let version = http_json(devtools_port, "GET", "/json/version")?;
    version
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            BrowserError::protocol("DevTools", format!("no webSocketDebuggerUrl in {version}"))
        })
}

/// Цикл слушателя: подключает консоль воркеров экстеншена и пишет их вызовы в журнал, пока браузер
/// не закрыл соединение или не попросили остановиться. Сбой записи или чтения печатается в stderr
/// стенда, а не глотается: тогда в приговоре будет «заметок нет», и причина видна рядом.
fn record_console(mut connection: CdpConnection, mut log: BufWriter<File>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let message = match connection.next_message() {
            Ok(CdpRead::Message(message)) => message,
            Ok(CdpRead::Quiet) => continue,
            Ok(CdpRead::Closed) => break,
            Err(error) => {
                eprintln!("bench: the worker console listener stopped: {error}");
                break;
            }
        };
        if let Some(session_id) = extension_worker_session(&message) {
            if let Err(error) =
                connection.send_command("Runtime.enable", Value::Null, Some(&session_id))
            {
                eprintln!("bench: the worker console was not enabled: {error}");
            }
            continue;
        }
        if let Some(line) = console_line_of(&message) {
            let written =
                writeln!(log, "{WORKER_CONSOLE_LINE_PREFIX}{line}").and_then(|()| log.flush());
            if let Err(error) = written {
                eprintln!("bench: the worker console line was not written: {error}");
                break;
            }
        }
    }
}

/// Сеанс подключения к служебному воркеру ЭКСТЕНШЕНА (`Target.attachedToTarget`); остальные цели — `None`.
fn extension_worker_session(message: &Value) -> Option<String> {
    if message.get("method").and_then(Value::as_str) != Some("Target.attachedToTarget") {
        return None;
    }
    let params = message.get("params")?;
    let target = params.get("targetInfo")?;
    let is_worker = target.get("type").and_then(Value::as_str) == Some("service_worker");
    let is_extension = target
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|url| url.starts_with(EXTENSION_WORKER_URL_PREFIX));
    if !(is_worker && is_extension) {
        return None;
    }
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Текст вызова консоли (`Runtime.consoleAPICalled`): аргументы через пробел, как печатает DevTools.
/// Строка — своим значением, остальное — описанием объекта.
fn console_line_of(message: &Value) -> Option<String> {
    if message.get("method").and_then(Value::as_str) != Some("Runtime.consoleAPICalled") {
        return None;
    }
    let arguments = message.pointer("/params/args")?.as_array()?;
    let parts: Vec<String> = arguments
        .iter()
        .map(|argument| match argument.get("value") {
            Some(Value::String(text)) => text.clone(),
            Some(other) if !other.is_null() => other.to_string(),
            _ => argument
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned(),
        })
        .collect();
    // Многострочный вызов — одной строкой журнала: иначе разбор заметок разорвал бы его.
    Some(parts.join(" ").replace('\n', " "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached(kind: &str, url: &str) -> Value {
        json!({
            "method": "Target.attachedToTarget",
            "params": {
                "sessionId": "S1",
                "targetInfo": { "type": kind, "url": url },
                "waitingForDebugger": false,
            }
        })
    }

    #[test]
    fn only_the_extension_service_worker_is_enabled() {
        assert_eq!(
            extension_worker_session(&attached(
                "service_worker",
                "chrome-extension://abc/background.js"
            )),
            Some("S1".to_owned())
        );
        assert_eq!(
            extension_worker_session(&attached("service_worker", "http://127.0.0.1/sw.js")),
            None
        );
        assert_eq!(
            extension_worker_session(&attached("page", "chrome-extension://abc/options.html")),
            None
        );
        assert_eq!(
            extension_worker_session(&json!({ "id": 1, "result": {} })),
            None
        );
    }

    #[test]
    fn a_console_call_becomes_one_journal_line() {
        let call = json!({
            "method": "Runtime.consoleAPICalled",
            "sessionId": "S1",
            "params": {
                "type": "debug",
                "args": [
                    { "type": "string", "value": "[fastiplayer] choice remembered\n(audio chosen on the page)" },
                    { "type": "number", "value": 2 },
                    { "type": "object", "description": "Error: boom" },
                ],
            }
        });
        assert_eq!(
            console_line_of(&call),
            Some(
                "[fastiplayer] choice remembered (audio chosen on the page) 2 Error: boom"
                    .to_owned()
            )
        );
        assert_eq!(
            console_line_of(&attached("service_worker", "chrome-extension://abc/")),
            None
        );
    }
}
